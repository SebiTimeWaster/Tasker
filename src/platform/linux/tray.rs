//! Panel icon via the StatusNotifierItem D-Bus protocol (KDE, Cinnamon/Mint via
//! xapp-sn-watcher, XFCE, GNOME with the AppIndicator extension, …).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::tray::{self as model, MenuEntry, RunningTask, UiMsg};

struct TaskerTray {
    tx: Sender<UiMsg>,
    ctx: egui::Context,
    running: Vec<RunningTask>,
    /// Whether a panel/tray host is currently showing our icon.
    online: Arc<AtomicBool>,
}

impl TaskerTray {
    fn send(&self, m: UiMsg) {
        let _ = self.tx.send(m);
        self.ctx.request_repaint();
    }
}

impl ksni::Tray for TaskerTray {
    fn id(&self) -> String {
        "tasker".into()
    }
    fn title(&self) -> String {
        "Tasker".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let state = model::icon_state(&self.running);
        [22, 32, 48, 64]
            .into_iter()
            .map(|s| {
                let rgba = crate::icon::render(s, state);
                // RGBA -> ARGB (network byte order)
                let data = rgba.chunks_exact(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
                ksni::Icon { width: s as i32, height: s as i32, data }
            })
            .collect()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: "Tasker".into(), description: model::tooltip(&self.running), ..Default::default() }
    }
    fn watcher_online(&self) {
        crate::logfile::info("panel tray host available");
        self.online.store(true, Ordering::Relaxed);
    }
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        crate::logfile::info(format!("no panel tray host available ({reason:?}); minimizing normally instead"));
        self.online.store(false, Ordering::Relaxed);
        true // keep the service alive; the host may appear later
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(UiMsg::ToggleWindow);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        model::menu(&self.running)
            .into_iter()
            .map(|e| match e {
                MenuEntry::Heading(label) => {
                    StandardItem { label: escape(&label), enabled: false, ..Default::default() }.into()
                }
                MenuEntry::Item { label, icon, msg } => StandardItem {
                    label: escape(&label),
                    icon_name: icon.into(),
                    activate: Box::new(move |t: &mut Self| t.send(msg)),
                    ..Default::default()
                }
                .into(),
                MenuEntry::Separator => ksni::MenuItem::Separator,
            })
            .collect()
    }
}

/// `_` marks a mnemonic in D-Bus menus; show it literally.
fn escape(label: &str) -> String {
    label.replace('_', "__")
}

pub struct Tray(ksni::blocking::Handle<TaskerTray>);

impl Tray {
    /// Register the icon. `online` tracks whether a panel currently shows it.
    pub fn spawn(tx: Sender<UiMsg>, ctx: egui::Context, online: Arc<AtomicBool>) -> Option<Tray> {
        use ksni::blocking::TrayMethods;
        online.store(true, Ordering::Relaxed);
        let tray = TaskerTray { tx, ctx, running: Vec::new(), online: online.clone() };
        match tray.assume_sni_available(true).spawn() {
            Ok(h) => Some(Tray(h)),
            Err(e) => {
                online.store(false, Ordering::Relaxed);
                crate::logfile::error(format!("tray icon unavailable: {e}"));
                None
            }
        }
    }

    /// Update icon color, tooltip and menu.
    pub fn set_running(&self, running: Vec<RunningTask>) {
        self.0.update(|t| t.running = running);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dbus_menu_escapes_underscores() {
        use ksni::Tray as _;
        let t = TaskerTray {
            tx: std::sync::mpsc::channel().0,
            ctx: egui::Context::default(),
            running: vec![RunningTask { run_id: 1, title: "Busy_work".into(), paused: false }],
            online: Arc::new(AtomicBool::new(true)),
        };
        let labels: Vec<String> = t
            .menu()
            .into_iter()
            .filter_map(|m| match m {
                ksni::MenuItem::Standard(s) => Some(s.label),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["▶ Busy__work", "      Pause", "      Finish", "Show / hide", "Quit"]);
    }
}
