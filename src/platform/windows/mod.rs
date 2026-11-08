//! Windows implementations; see [`crate::platform`].

mod sound;
mod tray;

pub use sound::Output;
pub use tray::Tray;

use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};

use super::Area;

pub fn session_info() -> String {
    "Windows".into()
}

/// Show `println!` output when started from a console (the exe has no console of its own).
pub fn attach_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: plain Win32 call; failure (no parent console) is harmless.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

pub fn can_pick_file() -> bool {
    true
}

/// The standard Windows "Open" dialog; None if cancelled.
pub fn pick_sound_file() -> Option<String> {
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    // Pairs of description and pattern, each NUL-terminated, plus a final NUL.
    let filter = wide("Audio (WAV, MP3, OGG, FLAC)\0*.wav;*.mp3;*.ogg;*.oga;*.flac\0All files\0*.*\0");
    let title = wide("Choose notification sound");
    let mut file = [0u16; 2048];
    // SAFETY: every pointer refers to a buffer that outlives the (blocking) call.
    let ok = unsafe {
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.nFilterIndex = 1;
        ofn.lpstrFile = file.as_mut_ptr();
        ofn.nMaxFile = file.len() as u32;
        ofn.lpstrTitle = title.as_ptr();
        ofn.Flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
        GetOpenFileNameW(&mut ofn) != 0
    };
    let n = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    (ok && n > 0).then(|| String::from_utf16_lossy(&file[..n]))
}

/// Monitor work areas (Windows reports them directly, taskbar excluded).
pub struct Screen;

impl Screen {
    pub fn connect() -> Option<Self> {
        Some(Screen)
    }

    /// Primary monitor's usable area.
    pub fn work_area(&self) -> Option<Area> {
        self.monitor_areas().into_iter().next()
    }

    /// Every monitor's usable area in physical pixels (winit makes the process
    /// per-monitor DPI aware); primary first.
    pub fn monitor_areas(&self) -> Vec<Area> {
        unsafe extern "system" fn add(mon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> i32 {
            // SAFETY: `data` is the Vec passed below, alive for the whole enumeration.
            let list = unsafe { &mut *(data as *mut Vec<(bool, Area)>) };
            let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if unsafe { GetMonitorInfoW(mon, &mut mi) } != 0 {
                let r = mi.rcWork;
                const MONITORINFOF_PRIMARY: u32 = 1;
                list.push((mi.dwFlags & MONITORINFOF_PRIMARY != 0, (r.left, r.top, r.right - r.left, r.bottom - r.top)));
            }
            1 // continue
        }
        let mut list: Vec<(bool, Area)> = Vec::new();
        // SAFETY: the callback only touches `list`, which outlives the call.
        unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(add), &mut list as *mut _ as LPARAM) };
        list.sort_by_key(|(primary, _)| !primary);
        list.into_iter().map(|(_, a)| a).collect()
    }

    /// Reminder windows are created without a taskbar entry (`with_taskbar(false)`),
    /// which Windows honors directly.
    pub fn hide_from_taskbar(&self, _title: &str) -> bool {
        true
    }
}

/// Single instance over a localhost TCP port (written to a file in %TEMP%). A second
/// launch connects, says hello and asks us to show up; the handshake makes sure a stale
/// port file pointing at some other program isn't mistaken for a running Tasker.
pub mod instance {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::time::Duration;

    const REQUEST: &str = "tasker show";
    const REPLY: &str = "tasker ok";

    fn port_file() -> PathBuf {
        std::env::temp_dir().join(super::super::instance_name("port"))
    }

    /// If an instance is running, ask it to show its window and return true.
    pub fn ask_running_to_show() -> bool {
        let Some(port) = std::fs::read_to_string(port_file()).ok().and_then(|s| s.trim().parse::<u16>().ok()) else {
            return false;
        };
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else { return false };
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        if writeln!(s, "{REQUEST}").is_err() {
            return false;
        }
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).is_ok() && line.trim() == REPLY
    }

    pub struct Listener(TcpListener);

    /// Become the running instance (None if no port can be opened).
    pub fn listen() -> Option<Listener> {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
        let port = l.local_addr().ok()?.port();
        std::fs::write(port_file(), port.to_string()).ok()?;
        Some(Listener(l))
    }

    impl Listener {
        /// Call `on_show` (on a background thread) whenever another launch asks.
        pub fn spawn(self, on_show: impl Fn() + Send + 'static) {
            std::thread::spawn(move || {
                for stream in self.0.incoming() {
                    let Ok(mut s) = stream else { continue };
                    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut line = String::new();
                    let Ok(reader) = s.try_clone() else { continue };
                    if BufReader::new(reader).read_line(&mut line).is_ok() && line.trim() == REQUEST {
                        let _ = writeln!(s, "{REPLY}");
                        on_show();
                    }
                }
            });
        }
    }

    /// Remove the port file at exit.
    pub fn cleanup() {
        let _ = std::fs::remove_file(port_file());
    }
}
