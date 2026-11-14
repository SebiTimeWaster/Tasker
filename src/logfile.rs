//! Tasker's own log file: `~/.local/state/tasker/tasker.log` (or `$XDG_STATE_HOME/tasker`)
//! on Linux, `%LOCALAPPDATA%\tasker\tasker.log` on Windows.
//!
//! Records start/exit, errors, reminder events, Rust panics (with backtrace) and, on Linux,
//! native crashes (fatal signals like SIGSEGV). Messages also go to stderr. At startup the
//! file is moved to `tasker.log.1` once it has grown past 1 MB.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_SIZE: u64 = 1024 * 1024;

static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn path() -> PathBuf {
    #[cfg(unix)]
    let base = dirs::state_dir().or_else(|| dirs::home_dir().map(|h| h.join(".local/state")));
    #[cfg(not(unix))]
    let base = dirs::data_local_dir();
    base.unwrap_or_else(std::env::temp_dir).join("tasker").join("tasker.log")
}

/// Open the log file and install the panic hook and fatal-signal handlers.
pub fn init() {
    let path = path();
    if let Some(dir) = path.parent() {
        crate::platform::create_private_dir(dir);
    }
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_SIZE) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
        #[cfg(unix)]
        signals::set_fd(&f);
        *FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let bt = std::backtrace::Backtrace::force_capture();
        write("PANIC", &format!("thread '{}' {info}\nbacktrace:\n{bt}", thread.name().unwrap_or("?")));
        default_hook(info);
    }));

    #[cfg(unix)]
    signals::install();
}

/// Append one line (or block) with a timestamp; also printed to stderr.
pub fn write(level: &str, msg: &str) {
    let line = format!("{} {level:5} {msg}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"));
    eprintln!("tasker: {level} {msg}");
    if let Some(f) = FILE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn info(msg: impl AsRef<str>) {
    write("INFO", msg.as_ref());
}

pub fn error(msg: impl AsRef<str>) {
    write("ERROR", msg.as_ref());
}

/// Native crashes: write a note to the log when a fatal signal arrives.
#[cfg(unix)]
mod signals {
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicI32, Ordering};

    /// Raw fd of the log file for the signal handler (which can't take locks).
    static FD: AtomicI32 = AtomicI32::new(-1);

    pub fn set_fd(f: &std::fs::File) {
        FD.store(f.as_raw_fd(), Ordering::SeqCst);
    }

    const FATAL: [(libc::c_int, &[u8]); 5] = [
        (libc::SIGSEGV, b"*** CRASH: fatal signal SIGSEGV (segmentation fault) - see the last lines above\n"),
        (libc::SIGABRT, b"*** CRASH: fatal signal SIGABRT (abort) - see the last lines above\n"),
        (libc::SIGBUS, b"*** CRASH: fatal signal SIGBUS (bus error) - see the last lines above\n"),
        (libc::SIGILL, b"*** CRASH: fatal signal SIGILL (illegal instruction) - see the last lines above\n"),
        (libc::SIGFPE, b"*** CRASH: fatal signal SIGFPE (arithmetic error) - see the last lines above\n"),
    ];

    extern "C" fn on_fatal_signal(sig: libc::c_int) {
        // Only async-signal-safe calls here: write() a fixed message, then re-raise with the
        // default action (SA_RESETHAND restored it), so the process still dies / dumps core.
        let fd = FD.load(Ordering::SeqCst);
        for (s, msg) in FATAL {
            if s == sig {
                unsafe {
                    if fd >= 0 {
                        libc::write(fd, msg.as_ptr().cast(), msg.len());
                    }
                    libc::write(2, msg.as_ptr().cast(), msg.len());
                }
            }
        }
        unsafe {
            libc::raise(sig);
        }
    }

    pub fn install() {
        for (sig, _) in FATAL {
            // SAFETY: plain sigaction setup; the handler only uses async-signal-safe calls.
            // SA_ONSTACK: Rust provides an alternate signal stack, so stack overflows are caught too.
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_fatal_signal as extern "C" fn(libc::c_int) as usize;
                sa.sa_flags = libc::SA_ONSTACK | libc::SA_RESETHAND;
                libc::sigemptyset(&mut sa.sa_mask);
                libc::sigaction(sig, &sa, std::ptr::null_mut());
            }
        }
    }
}
