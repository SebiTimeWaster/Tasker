//! Sound output through ALSA's `default` PCM, which on modern desktops is routed to
//! PipeWire / PulseAudio. `libasound.so.2` is loaded at runtime, so building needs no
//! ALSA headers and a missing library only disables sound instead of crashing.

use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CStr};

type SndPcm = c_void;
pub struct Output {
    _lib: libloading::Library,
    open: unsafe extern "C" fn(*mut *mut SndPcm, *const c_char, c_int, c_int) -> c_int,
    set_params: unsafe extern "C" fn(*mut SndPcm, c_int, c_int, c_uint, c_uint, c_int, c_uint) -> c_int,
    writei: unsafe extern "C" fn(*mut SndPcm, *const c_void, c_ulong) -> c_long,
    recover: unsafe extern "C" fn(*mut SndPcm, c_int, c_int) -> c_int,
    drop_: unsafe extern "C" fn(*mut SndPcm) -> c_int,
    drain: unsafe extern "C" fn(*mut SndPcm) -> c_int,
    close: unsafe extern "C" fn(*mut SndPcm) -> c_int,
    strerror: unsafe extern "C" fn(c_int) -> *const c_char,
}

const SND_PCM_STREAM_PLAYBACK: c_int = 0;
const SND_PCM_FORMAT_S16_LE: c_int = 2;
const SND_PCM_ACCESS_RW_INTERLEAVED: c_int = 3;

impl Output {
    pub fn load() -> Result<Self, String> {
        unsafe {
            let lib = libloading::Library::new("libasound.so.2").map_err(|e| format!("cannot load libasound.so.2: {e}"))?;
            macro_rules! sym {
                ($name:literal) => {
                    *lib.get($name).map_err(|e| format!("libasound: {e}"))?
                };
            }
            Ok(Output {
                open: sym!(b"snd_pcm_open\0"),
                set_params: sym!(b"snd_pcm_set_params\0"),
                writei: sym!(b"snd_pcm_writei\0"),
                recover: sym!(b"snd_pcm_recover\0"),
                drop_: sym!(b"snd_pcm_drop\0"),
                drain: sym!(b"snd_pcm_drain\0"),
                close: sym!(b"snd_pcm_close\0"),
                strerror: sym!(b"snd_strerror\0"),
                _lib: lib,
            })
        }
    }

    fn err(&self, code: c_int) -> String {
        unsafe { CStr::from_ptr((self.strerror)(code)).to_string_lossy().into_owned() }
    }
}

pub struct Pcm<'a> {
    alsa: &'a Output,
    pcm: *mut SndPcm,
}

impl Output {
    /// Open the output for interleaved stereo i16 at `rate`.
    pub fn open(&self, rate: u32) -> Result<Pcm<'_>, String> {
        let alsa = self;
        let mut pcm = std::ptr::null_mut();
        // TASKER_ALSA_DEVICE overrides the output device (e.g. "pulse", "pipewire", "hw:0").
        let device = std::env::var("TASKER_ALSA_DEVICE").unwrap_or_else(|_| "default".into());
        let name = std::ffi::CString::new(device.clone()).map_err(|e| e.to_string())?;
        unsafe {
            let rc = (alsa.open)(&mut pcm, name.as_ptr(), SND_PCM_STREAM_PLAYBACK, 0);
            if rc < 0 {
                return Err(format!("No audio output available (ALSA device \"{device}\": {})", alsa.err(rc)));
            }
            let rc = (alsa.set_params)(pcm, SND_PCM_FORMAT_S16_LE, SND_PCM_ACCESS_RW_INTERLEAVED, 2, rate, 1, 100_000);
            if rc < 0 {
                (alsa.close)(pcm);
                return Err(format!("snd_pcm_set_params: {}", alsa.err(rc)));
            }
        }
        Ok(Pcm { alsa, pcm })
    }
}

impl Pcm<'_> {
    /// Blocking write of interleaved stereo frames.
    pub fn write(&mut self, buf: &[i16]) -> Result<(), String> {
        let mut off = 0;
        while off < buf.len() {
            let frames = ((buf.len() - off) / 2) as c_ulong;
            let n = unsafe { (self.alsa.writei)(self.pcm, buf[off..].as_ptr() as *const c_void, frames) };
            if n < 0 {
                let rc = unsafe { (self.alsa.recover)(self.pcm, n as c_int, 1) };
                if rc < 0 {
                    return Err(format!("snd_pcm_writei: {}", self.alsa.err(rc)));
                }
            } else {
                off += n as usize * 2;
            }
        }
        Ok(())
    }

    /// Stop: play what is queued (`drain`) or cut it off.
    pub fn finish(self, drain: bool) {
        unsafe {
            if drain {
                (self.alsa.drain)(self.pcm);
            } else {
                (self.alsa.drop_)(self.pcm);
            }
        }
        // close in Drop
    }
}

impl Drop for Pcm<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.alsa.close)(self.pcm);
        }
    }
}
