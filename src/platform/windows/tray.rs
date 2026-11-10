//! Notification-area icon (via the `tray-icon` crate). Left click shows / hides the
//! window, right click opens the menu from [`crate::tray::menu`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::icon::IconState;
use crate::tray::{self as model, MenuEntry, TrayTask, UiMsg};

/// Lives on the main (UI) thread, which runs the Windows message loop.
pub struct Tray(TrayIcon);

fn icon(state: IconState) -> Option<Icon> {
    // Windows scales it to the notification area's size (16 px at 100 %, 32 at 200 %).
    Icon::from_rgba(crate::icon::render(32, state), 32, 32).ok()
}

fn menu(running: &[TrayTask]) -> Menu {
    let menu = Menu::new();
    for e in model::menu(running) {
        // `&` marks a mnemonic in Windows menus; show it literally.
        let r = match e {
            MenuEntry::Heading(label) => menu.append(&MenuItem::new(label.replace('&', "&&"), false, None)),
            MenuEntry::Item { label, msg, .. } => {
                menu.append(&MenuItem::with_id(model::msg_id(msg), label.replace('&', "&&"), true, None))
            }
            MenuEntry::Separator => menu.append(&PredefinedMenuItem::separator()),
        };
        if let Err(e) = r {
            crate::logfile::error(format!("tray menu: {e}"));
        }
    }
    menu
}

impl Tray {
    /// Must be called on the main thread. `online` is set: Windows always has a
    /// notification area.
    pub fn spawn(tx: Sender<UiMsg>, ctx: egui::Context, online: Arc<AtomicBool>) -> Option<Tray> {
        let send = move |m: UiMsg| {
            let _ = tx.send(m);
            ctx.request_repaint();
        };
        let send2 = send.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                send(UiMsg::ToggleWindow);
            }
        }));
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if let Some(m) = model::parse_msg_id(&e.id.0) {
                send2(m);
            }
        }));
        let mut b =
            TrayIconBuilder::new().with_tooltip("Tasker").with_menu(Box::new(menu(&[]))).with_menu_on_left_click(false);
        if let Some(i) = icon(IconState::Idle) {
            b = b.with_icon(i);
        }
        match b.build() {
            Ok(t) => {
                online.store(true, Ordering::Relaxed);
                Some(Tray(t))
            }
            Err(e) => {
                online.store(false, Ordering::Relaxed);
                crate::logfile::error(format!("tray icon unavailable: {e}"));
                None
            }
        }
    }

    /// Update icon color, tooltip and menu.
    pub fn set_tasks(&self, running: Vec<TrayTask>) {
        let _ = self.0.set_icon(icon(model::icon_state(&running)));
        let tip = model::tooltip(&running);
        let _ = self.0.set_tooltip(Some(if tip.is_empty() { "Tasker".to_string() } else { format!("Tasker\n{tip}") }));
        self.0.set_menu(Some(Box::new(menu(&running))));
    }
}
