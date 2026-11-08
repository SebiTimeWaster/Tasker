//! Sound output through the classic waveOut API (winmm.dll, present on every Windows),
//! using the default output device. A few small buffers are kept in flight; `write`
//! blocks until one is free, like a blocking ALSA write.

use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Media::Audio::{
    waveOutClose, waveOutGetErrorTextW, waveOutOpen, waveOutPrepareHeader, waveOutReset, waveOutUnprepareHeader,
    waveOutWrite, CALLBACK_EVENT, HWAVEOUT, WAVEFORMATEX, WAVEHDR, WAVE_FORMAT_PCM, WAVE_MAPPER, WHDR_DONE,
};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// Buffers in flight and their length: 8 × 20 ms keeps latency low without underruns.
const BUFFERS: usize = 8;
const BUFFER_MS: u32 = 20;
const MMSYSERR_NOERROR: u32 = 0;

pub struct Output;

impl Output {
    pub fn load() -> Result<Self, String> {
        Ok(Output)
    }

    /// Open the default device for interleaved stereo i16 at `rate`.
    pub fn open(&self, rate: u32) -> Result<Pcm, String> {
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        // SAFETY: plain Win32 calls; handles are closed in Drop (or here on failure).
        unsafe {
            let event = CreateEventW(std::ptr::null(), 0, 0, std::ptr::null());
            if event.is_null() {
                return Err("CreateEvent failed".into());
            }
            let mut hwo: HWAVEOUT = std::ptr::null_mut();
            let rc = waveOutOpen(&mut hwo, WAVE_MAPPER, &fmt, event as usize, 0, CALLBACK_EVENT);
            if rc != MMSYSERR_NOERROR {
                CloseHandle(event);
                return Err(format!("No audio output available ({})", error_text(rc)));
            }
            let samples = (rate * BUFFER_MS / 1000) as usize * 2;
            let bufs = (0..BUFFERS)
                .map(|_| Buffer { hdr: Box::new(Header(std::mem::zeroed())), data: vec![0; samples], queued: false })
                .collect();
            Ok(Pcm { hwo, event, bufs })
        }
    }
}

/// `WAVEHDR` is a packed struct; this wrapper keeps `dwFlags` 4-byte aligned so the
/// driver's updates can be read with a volatile load. Boxed: it must not move while queued.
#[repr(C, align(8))]
struct Header(WAVEHDR);

struct Buffer {
    hdr: Box<Header>,
    data: Vec<i16>,
    /// Handed to the driver (prepared) and not yet unprepared.
    queued: bool,
}

impl Buffer {
    fn done(&self) -> bool {
        // SAFETY: aligned (see Header) pointer into our own allocation; the driver
        // sets WHDR_DONE from another thread, hence volatile.
        let flags = unsafe { std::ptr::read_volatile(std::ptr::addr_of!(self.hdr.0.dwFlags)) };
        flags & WHDR_DONE != 0
    }
}

pub struct Pcm {
    hwo: HWAVEOUT,
    event: HANDLE,
    bufs: Vec<Buffer>,
}

// The handles are only used from the audio thread that opened them.
unsafe impl Send for Pcm {}

const HDR_SIZE: u32 = std::mem::size_of::<WAVEHDR>() as u32;

impl Pcm {
    /// Blocking write of interleaved stereo frames.
    pub fn write(&mut self, buf: &[i16]) -> Result<(), String> {
        let cap = self.bufs[0].data.len();
        for chunk in buf.chunks(cap) {
            let i = self.free_buffer()?;
            let hwo = self.hwo;
            let b = &mut self.bufs[i];
            b.data[..chunk.len()].copy_from_slice(chunk);
            let mut hdr: WAVEHDR = unsafe { std::mem::zeroed() };
            hdr.lpData = b.data.as_mut_ptr().cast();
            hdr.dwBufferLength = (chunk.len() * 2) as u32;
            b.hdr.0 = hdr;
            // SAFETY: header and data live in `self` (boxed / heap) until unprepared.
            unsafe {
                let rc = waveOutPrepareHeader(hwo, &mut b.hdr.0, HDR_SIZE);
                if rc != MMSYSERR_NOERROR {
                    return Err(format!("waveOutPrepareHeader: {}", error_text(rc)));
                }
                b.queued = true;
                let rc = waveOutWrite(hwo, &mut b.hdr.0, HDR_SIZE);
                if rc != MMSYSERR_NOERROR {
                    return Err(format!("waveOutWrite: {}", error_text(rc)));
                }
            }
        }
        Ok(())
    }

    /// Index of a buffer the driver is done with (unprepared), waiting if all are busy.
    fn free_buffer(&mut self) -> Result<usize, String> {
        let started = Instant::now();
        loop {
            if let Some(i) = self.bufs.iter().position(|b| !b.queued || b.done()) {
                self.unprepare(i);
                return Ok(i);
            }
            if started.elapsed() > Duration::from_secs(2) {
                return Err("audio device stopped playing".into());
            }
            // Signalled whenever a buffer finishes; the timeout guards against a missed one.
            unsafe { WaitForSingleObject(self.event, 50) };
        }
    }

    fn unprepare(&mut self, i: usize) {
        let b = &mut self.bufs[i];
        if b.queued {
            // SAFETY: the buffer is done (or the device was reset), so it may be unprepared.
            unsafe { waveOutUnprepareHeader(self.hwo, &mut b.hdr.0, HDR_SIZE) };
            b.queued = false;
        }
    }

    /// Stop: play what is queued (`drain`) or cut it off.
    pub fn finish(self, drain: bool) {
        if drain {
            let started = Instant::now();
            while self.bufs.iter().any(|b| b.queued && !b.done()) && started.elapsed() < Duration::from_secs(2) {
                unsafe { WaitForSingleObject(self.event, 50) };
            }
        }
        // reset + close in Drop
    }
}

impl Drop for Pcm {
    fn drop(&mut self) {
        // SAFETY: reset marks every queued buffer done, so all can be unprepared before closing.
        unsafe { waveOutReset(self.hwo) };
        for i in 0..self.bufs.len() {
            self.unprepare(i);
        }
        unsafe {
            waveOutClose(self.hwo);
            CloseHandle(self.event);
        }
    }
}

fn error_text(rc: u32) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: the buffer length is passed along.
    if unsafe { waveOutGetErrorTextW(rc, buf.as_mut_ptr(), buf.len() as u32) } == MMSYSERR_NOERROR {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..n])
    } else {
        format!("error {rc}")
    }
}
