//! Everything that differs between Linux and Windows. Each OS module offers the same
//! names:
//!
//! - `Screen`: `connect()`, `work_area()` / `monitor_areas()` (usable area of each monitor
//!   in physical pixels, primary first), `describe()` (monitor data for the log) and
//!   `hide_from_taskbar(title)` for reminders.
//! - `Tray`: the panel / notification-area icon (`spawn`, `set_tasks`); its contents
//!   come from [`crate::tray`].
//! - `instance`: single-instance handling: `ask_running_to_show()`, `listen()`,
//!   `Listener::spawn`, `cleanup()`.
//! - `Output` / `Pcm`: sound output (interleaved stereo i16): `Output::load()`,
//!   `open(rate)`, `Pcm::write` (blocking), `Pcm::finish(drain)`.
//! - `session_info()`: a description of the desktop session for the log.
//! - `attach_console()`: make `println!` visible when started from a terminal.
//! - `can_pick_file()` / `pick_sound_file()`: a native "open file" dialog (blocking, so
//!   call it from a background thread).

#[cfg(unix)]
mod linux;
#[cfg(unix)]
pub use linux::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// Create `dir` (and its parents) for Tasker's own files. On Unix only this user may
/// open it: tasks, notes and the log are nobody else's business, and home directories
/// are often readable by everyone. An existing directory is made private too.
pub fn create_private_dir(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
}

/// x, y, width, height in physical pixels.
pub type Area = (i32, i32, i32, i32);

/// Name for the single-instance endpoint: one per database (handy for testing with
/// `TASKER_DB`), so test instances don't bring up the real one.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
fn instance_name(ext: &str) -> String {
    match std::env::var("TASKER_DB") {
        Ok(db) => format!("tasker-{:x}.{ext}", db.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64))),
        Err(_) => format!("tasker.{ext}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn private_dirs() {
        let dir = std::env::temp_dir().join(format!("tasker-private-{}", std::process::id())).join("tasker");
        super::create_private_dir(&dir);
        assert!(dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            super::create_private_dir(&dir);
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "an existing directory is made private");
        }
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}
