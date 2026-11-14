//! Linux (X11 / XWayland) implementations; see [`crate::platform`].

mod sound;
mod tray;
mod x11;

pub use sound::Output;
pub use tray::Tray;
pub use x11::X11 as Screen;

pub fn session_info() -> String {
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| "-".into());
    format!("session {}, desktop {}, DISPLAY {}", env("XDG_SESSION_TYPE"), env("XDG_CURRENT_DESKTOP"), env("DISPLAY"))
}

/// Nothing to do: a terminal sees our output anyway.
pub fn attach_console() {}

/// The desktop's file chooser, through zenity (GNOME, Cinnamon, …) or kdialog (KDE).
fn file_dialog_cmd() -> Option<Vec<&'static str>> {
    let has =
        |b: &str| std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(b).is_file()));
    if has("zenity") {
        Some(vec![
            "zenity",
            "--file-selection",
            "--title=Choose notification sound",
            "--file-filter=Audio | *.wav *.mp3 *.ogg *.oga *.flac",
        ])
    } else if has("kdialog") {
        Some(vec!["kdialog", "--getopenfilename", ".", "*.wav *.mp3 *.ogg *.oga *.flac"])
    } else {
        None
    }
}

pub fn can_pick_file() -> bool {
    file_dialog_cmd().is_some()
}

/// Ask for a sound file; None if cancelled or no file chooser is available.
pub fn pick_sound_file() -> Option<String> {
    let cmd = file_dialog_cmd()?;
    let o = std::process::Command::new(cmd[0]).args(&cmd[1..]).output().ok()?;
    let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !p.is_empty()).then_some(p)
}

/// Single instance over a Unix socket: a second launch connects and asks us to show up.
pub mod instance {
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    /// In the user's runtime directory. Without one the socket goes to the temporary
    /// directory, which all users share: there its name says whose it is, so that two
    /// users on one machine each get their own.
    fn socket_path() -> PathBuf {
        let name = super::super::instance_name("sock");
        match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) => PathBuf::from(dir).join(name),
            // SAFETY: getuid has no preconditions and cannot fail.
            None => std::env::temp_dir().join(format!("u{}-{name}", unsafe { libc::getuid() })),
        }
    }

    /// Whether `path` exists and belongs to this user. In the shared temporary directory
    /// another user could have put a socket under our name first; connecting to it would
    /// make every start think Tasker is running already and quit.
    pub(super) fn is_mine(path: &Path) -> bool {
        // SAFETY: getuid has no preconditions and cannot fail.
        std::fs::symlink_metadata(path).is_ok_and(|m| m.uid() == unsafe { libc::getuid() })
    }

    /// If an instance is running, ask it to show its window and return true.
    pub fn ask_running_to_show() -> bool {
        let path = socket_path();
        if !is_mine(&path) {
            return false;
        }
        match UnixStream::connect(&path) {
            Ok(mut s) => {
                let _ = s.write_all(b"show\n");
                true
            }
            Err(_) => false,
        }
    }

    pub struct Listener(UnixListener);

    /// Become the running instance (None if the socket can't be created).
    pub fn listen() -> Option<Listener> {
        let path = socket_path();
        if is_mine(&path) {
            let _ = std::fs::remove_file(&path); // left over from a crash
        }
        match UnixListener::bind(&path) {
            Ok(l) => Some(Listener(l)),
            Err(e) => {
                crate::logfile::error(format!("single instance: cannot listen on {}: {e}", path.display()));
                None
            }
        }
    }

    impl Listener {
        /// Call `on_show` (on a background thread) whenever another launch asks.
        pub fn spawn(self, on_show: impl Fn() + Send + 'static) {
            std::thread::spawn(move || {
                for stream in self.0.incoming() {
                    if stream.is_ok() {
                        on_show();
                    }
                }
            });
        }
    }

    /// Remove the socket at exit.
    pub fn cleanup() {
        let _ = std::fs::remove_file(socket_path());
    }
}

#[cfg(test)]
mod tests {
    use super::instance::is_mine;
    use std::path::Path;

    #[test]
    fn only_own_sockets_are_trusted() {
        let path = std::env::temp_dir().join(format!("tasker-owner-{}", std::process::id()));
        assert!(!is_mine(&path), "missing");
        std::fs::write(&path, b"").unwrap();
        assert!(is_mine(&path));
        let _ = std::fs::remove_file(&path);
        // SAFETY: getuid has no preconditions and cannot fail.
        if unsafe { libc::getuid() } != 0 {
            assert!(!is_mine(Path::new("/")), "owned by root");
        }
    }
}
