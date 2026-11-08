//! Notification sound playback: synthesis / decoding, looping and the volume ramp
//! (applied in software, sample by sample). The output device is the platform's
//! (`platform::{Output, Pcm}`: ALSA on Linux, waveOut on Windows).

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::platform::Output;
use crate::settings::{Settings, SoundChoice};

const RATE: u32 = 48_000;

/// Interleaved stereo i16 samples.
struct Sound {
    rate: u32,
    data: Vec<i16>,
}

#[derive(Clone, Debug)]
pub struct PlayRequest {
    pub choice: SoundChoice,
    pub file: String,
    pub ramp_secs: f32,
    pub start_volume: f32,
    pub end_volume: f32,
    /// Stop automatically after this long (used for "Test").
    pub max_duration: Option<Duration>,
}

impl PlayRequest {
    pub fn from_settings(s: &Settings, max_duration: Option<Duration>) -> Self {
        PlayRequest {
            choice: s.sound,
            file: s.sound_file.clone(),
            ramp_secs: s.ramp_secs,
            start_volume: s.start_volume,
            end_volume: s.end_volume,
            max_duration,
        }
    }
}

enum Cmd {
    Play(PlayRequest),
    Stop,
}

pub struct Audio {
    tx: Sender<Cmd>,
    /// Last error message, shown in the settings dialog.
    pub last_error: Arc<Mutex<Option<String>>>,
    playing: bool,
}

impl Audio {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let last_error = Arc::new(Mutex::new(None));
        let err = last_error.clone();
        std::thread::Builder::new()
            .name("tasker-audio".into())
            .spawn(move || audio_thread(rx, err))
            .expect("spawn audio thread");
        Audio { tx, last_error, playing: false }
    }

    pub fn play(&mut self, req: PlayRequest) {
        self.playing = true;
        let _ = self.tx.send(Cmd::Play(req));
    }

    pub fn stop(&mut self) {
        self.playing = false;
        let _ = self.tx.send(Cmd::Stop);
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|e| e.clone())
    }
}

// ------------------------------------------------------------------------ playback thread

fn audio_thread(rx: Receiver<Cmd>, last_error: Arc<Mutex<Option<String>>>) {
    let set_err = |e: Option<String>| {
        if let Some(msg) = &e {
            crate::logfile::error(format!("audio: {msg}"));
        }
        if let Ok(mut l) = last_error.lock() {
            *l = e;
        }
    };
    let output = Output::load();
    if let Err(e) = &output {
        set_err(Some(e.clone()));
    }
    let mut cache: HashMap<String, Arc<Sound>> = HashMap::new();
    let mut pending: Option<PlayRequest> = None;

    loop {
        let req = match pending.take() {
            Some(r) => r,
            None => match rx.recv() {
                Ok(Cmd::Play(r)) => r,
                Ok(Cmd::Stop) => continue,
                Err(_) => return,
            },
        };
        let output = match &output {
            Ok(o) => o,
            Err(e) => {
                set_err(Some(format!("Sound unavailable: {e}")));
                continue;
            }
        };
        let (sound, file_err) = match load_sound(&req, &mut cache) {
            Ok(s) => (s, None),
            // Fall back to the built-in chime so a broken file never silences reminders.
            Err(e) => (synth(SoundChoice::Chime), Some(format!("{e} — playing the built-in chime instead"))),
        };
        let mut pcm = match output.open(sound.rate) {
            Ok(p) => p,
            Err(e) => {
                set_err(Some(e));
                // Wait for the next command instead of spinning.
                continue;
            }
        };
        set_err(file_err);

        let started = Instant::now();
        let chunk_frames = (sound.rate / 50) as usize; // 20 ms
        let mut pos = 0usize; // index into interleaved data
        let mut buf = vec![0i16; chunk_frames * 2];
        let total = sound.rate as f32 * req.ramp_secs.max(0.0);
        let mut frame_no: u64 = 0;
        let mut stop_reason_drain = false;
        'play: loop {
            match rx.try_recv() {
                Ok(Cmd::Stop) => break 'play,
                Ok(Cmd::Play(r)) => {
                    pending = Some(r);
                    break 'play;
                }
                Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if let Some(max) = req.max_duration {
                if started.elapsed() >= max {
                    stop_reason_drain = true;
                    break 'play;
                }
            }
            for f in 0..chunk_frames {
                let t = if total > 0.0 { ((frame_no + f as u64) as f32 / total).min(1.0) } else { 1.0 };
                let vol = req.start_volume + (req.end_volume - req.start_volume) * t;
                // Perceptual-ish curve: amplitude grows with the square of the slider value.
                let gain = vol.clamp(0.0, 1.0).powi(2);
                for c in 0..2 {
                    let s = sound.data[pos + c] as f32 * gain;
                    buf[f * 2 + c] = s as i16;
                }
                pos += 2;
                if pos >= sound.data.len() {
                    pos = 0; // loop
                }
            }
            frame_no += chunk_frames as u64;
            if let Err(e) = pcm.write(&buf) {
                set_err(Some(e));
                break 'play;
            }
        }
        pcm.finish(stop_reason_drain);
        // Block briefly for follow-up commands so a Stop+Play pair doesn't reopen twice.
        if pending.is_none() {
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(Cmd::Play(r)) => pending = Some(r),
                Ok(Cmd::Stop) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

fn load_sound(req: &PlayRequest, cache: &mut HashMap<String, Arc<Sound>>) -> Result<Arc<Sound>, String> {
    let key = match req.choice {
        SoundChoice::File => format!("file:{}", req.file),
        c => format!("{c:?}"),
    };
    if let Some(s) = cache.get(&key) {
        return Ok(s.clone());
    }
    let s = match req.choice {
        SoundChoice::File => {
            if req.file.trim().is_empty() {
                return Err("No sound file selected".into());
            }
            Arc::new(decode_file(&req.file)?)
        }
        c => synth(c),
    };
    cache.insert(key, s.clone());
    Ok(s)
}

fn decode_file(path: &str) -> Result<Sound, String> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| format!("{path}: unsupported format ({e})"))?;
    let track = format.default_track(TrackType::Audio).ok_or("no audio track")?;
    let track_id = track.id;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("no audio codec parameters")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("{path}: {e}"))?;

    let mut rate = 0u32;
    let mut out: Vec<i16> = Vec::new();
    let mut tmp: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(Error::DecodeError(_)) => continue,
            Err(_) => break,
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = buf.spec();
                rate = spec.rate();
                let ch = spec.channels().count().max(1);
                tmp.resize(buf.samples_interleaved(), 0.0);
                buf.copy_to_slice_interleaved(&mut tmp);
                for frame in tmp.chunks_exact(ch) {
                    let l = frame[0];
                    let r = if ch > 1 { frame[1] } else { l };
                    out.push((l.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                    out.push((r.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                }
            }
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(format!("{path}: {e}")),
        }
        if out.len() > RATE as usize * 2 * 120 {
            break; // cap at ~2 minutes; it loops anyway
        }
    }
    if out.is_empty() || rate == 0 {
        return Err(format!("{path}: no audio decoded"));
    }
    // Small gap between loops.
    out.extend(std::iter::repeat_n(0, (rate as usize / 2) * 2));
    Ok(Sound { rate, data: out })
}

/// Built-in placeholder sounds, synthesized so the app ships without asset files.
fn synth(choice: SoundChoice) -> Arc<Sound> {
    let rate = RATE as f32;
    let mut mono: Vec<f32> = Vec::new();
    let mut tone = |start: f32, freq: f32, len: f32, decay: f32, harmonics: &[(f32, f32)], total: f32| {
        let n_total = (total * rate) as usize;
        if mono.len() < n_total {
            mono.resize(n_total, 0.0);
        }
        let s0 = (start * rate) as usize;
        let n = (len * rate) as usize;
        for i in 0..n {
            let t = i as f32 / rate;
            let attack = (t / 0.005).min(1.0);
            let env = attack * (-t * decay).exp();
            let mut v = 0.0;
            for &(mul, amp) in harmonics {
                v += amp * (std::f32::consts::TAU * freq * mul * t).sin();
            }
            if let Some(m) = mono.get_mut(s0 + i) {
                *m += v * env;
            }
        }
    };
    match choice {
        SoundChoice::Bell => {
            let h = [(1.0, 0.6), (2.0, 0.25), (2.76, 0.15), (5.4, 0.06)];
            tone(0.0, 660.0, 2.2, 2.2, &h, 3.0);
        }
        SoundChoice::Beep => {
            let h = [(1.0, 0.5), (3.0, 0.08)];
            for k in 0..3 {
                tone(k as f32 * 0.22, 1000.0, 0.14, 4.0, &h, 1.6);
            }
        }
        _ => {
            let h = [(1.0, 0.55), (2.0, 0.12), (3.0, 0.05)];
            tone(0.0, 880.0, 1.0, 4.5, &h, 2.0);
            tone(0.18, 1318.5, 1.2, 4.0, &h, 2.0);
        }
    }
    let peak = mono.iter().fold(0.0f32, |a, &b| a.max(b.abs())).max(1e-6);
    let data = mono
        .iter()
        .flat_map(|&v| {
            let s = (v / peak * 0.8 * i16::MAX as f32) as i16;
            [s, s]
        })
        .collect();
    Arc::new(Sound { rate: RATE, data })
}
