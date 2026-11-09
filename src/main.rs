// No console window on Windows (the log file has everything; see `attach_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod action;
mod app;
mod audio;
mod dialogs;
mod icon;
mod logfile;
mod model;
mod platform;
mod settings;
mod store;
mod tray;
mod views;
mod widgets;

use std::path::PathBuf;
use std::sync::Arc;

use platform::instance;
use settings::WINDOW_SIZE;

fn db_path() -> PathBuf {
    if let Some(p) = std::env::var_os("TASKER_DB") {
        return PathBuf::from(p);
    }
    let dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("tasker");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("tasker.db")
}

fn main() -> eframe::Result {
    // Notifications must be placed in a screen corner, which Wayland doesn't allow
    // clients to do — so prefer X11 (XWayland on Wayland sessions). TASKER_WAYLAND=1 opts out.
    if std::env::var_os("TASKER_WAYLAND").is_none() && std::env::var_os("DISPLAY").is_some() {
        // SAFETY: no other threads exist yet.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    }

    let minimized_flag = std::env::args().any(|a| a == "--minimized");
    logfile::init();

    if std::env::args().any(|a| a == "--test-sound") {
        platform::attach_console();
        // Diagnostic: play the configured reminder sound (with its ramp) for a few seconds.
        let settings = store::Store::open(&db_path()).map(|s| s.load_settings()).unwrap_or_default();
        let mut audio = audio::Audio::new();
        let secs = settings.ramp_secs.clamp(3.0, 10.0);
        println!("Playing {:?} for {secs:.0} s …", settings.sound);
        audio.play(audio::PlayRequest::from_settings(&settings, Some(std::time::Duration::from_secs_f32(secs))));
        std::thread::sleep(std::time::Duration::from_secs_f32(secs + 0.7));
        return match audio.error() {
            Some(e) => {
                logfile::error(format!("sound test failed: {e}"));
                std::process::exit(1)
            }
            None => {
                println!("OK");
                Ok(())
            }
        };
    }

    // Single instance: if one is already running, ask it to show its window and exit.
    if instance::ask_running_to_show() {
        logfile::info("already running; asked the running instance to show its window");
        return Ok(());
    }
    let listener = instance::listen();

    let store = match store::Store::open(&db_path()) {
        Ok(s) => s,
        Err(e) => {
            logfile::error(format!("cannot open database {}: {e}", db_path().display()));
            std::process::exit(1);
        }
    };
    logfile::info(format!(
        "start: version {}, pid {}, {}, db {}",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        platform::session_info(),
        db_path().display()
    ));
    let settings = store.load_settings();
    let start_hidden = minimized_flag || settings.start_minimized;

    let icon = egui::IconData { rgba: icon::render(64, icon::IconState::Idle), width: 64, height: 64 };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Tasker")
        .with_app_id("tasker")
        .with_decorations(false)
        .with_icon(Arc::new(icon));
    // The window is created before the app sets the UI scale, so these are logical pixels.
    // The minimum size is set by the app once it knows the screen size.
    let window = store.load_window();
    viewport = match window {
        Some(g) => viewport.with_inner_size([g.w, g.h]).with_position([g.x, g.y]).with_maximized(g.maximized),
        None => viewport.with_inner_size([WINDOW_SIZE[0] * settings.ui_scale, WINDOW_SIZE[1] * settings.ui_scale]),
    };
    let options = eframe::NativeOptions {
        viewport,
        // Return from run_native (instead of exiting) so we can clean up the instance socket.
        run_and_return: true,
        ..Default::default()
    };
    let res = eframe::run_native(
        "Tasker",
        options,
        Box::new(move |cc| Ok(Box::new(app::TaskerApp::new(cc, store, settings, window, listener, start_hidden)))),
    );
    instance::cleanup();
    match &res {
        Ok(()) => logfile::info("exit"),
        Err(e) => logfile::error(format!("exit with error: {e}")),
    }
    res
}
