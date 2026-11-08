//! The panel / notification-area icon: what it shows and what its menu offers. Drawing it
//! is up to the platform (`platform::Tray`: D-Bus StatusNotifierItem on Linux, the
//! notification area on Windows); both use the model built here.

use crate::app::Action;
use crate::icon::IconState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMsg {
    ToggleWindow,
    ShowWindow,
    Quit,
    /// Pause/finish chosen in the panel icon's menu.
    Task(Action),
}

/// A running or paused task as shown in the panel icon's menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningTask {
    pub run_id: i64,
    pub title: String,
    pub paused: bool,
}

/// Blue when idle, yellow while a task is paused, green while one runs (green wins).
pub fn icon_state(running: &[RunningTask]) -> IconState {
    if running.iter().any(|t| !t.paused) {
        IconState::Running
    } else if !running.is_empty() {
        IconState::Paused
    } else {
        IconState::Idle
    }
}

/// The running and paused tasks, one per line.
pub fn tooltip(running: &[RunningTask]) -> String {
    running.iter().map(|t| format!("{} {}", if t.paused { "⏸" } else { "▶" }, t.title)).collect::<Vec<_>>().join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// A greyed-out line (a task's title).
    Heading(String),
    /// A clickable entry. `icon` is a freedesktop icon name (ignored where unsupported).
    Item { label: String, icon: &'static str, msg: UiMsg },
    Separator,
}

/// The icon's menu: per running/paused task its title, then Pause or Resume, and Finish
/// (running tasks first); then Show / hide and Quit. Labels are plain text; each platform
/// escapes its own mnemonic markers.
pub fn menu(running: &[RunningTask]) -> Vec<MenuEntry> {
    let mut items = Vec::new();
    let mut tasks: Vec<&RunningTask> = running.iter().collect();
    tasks.sort_by_key(|t| t.paused);
    for t in tasks {
        let id = t.run_id;
        let (mark, label, icon, action) = if t.paused {
            ("⏸", "Resume", "media-playback-start", Action::Resume(id))
        } else {
            ("▶", "Pause", "media-playback-pause", Action::Pause(id))
        };
        items.push(MenuEntry::Heading(format!("{mark} {}", short_title(&t.title))));
        items.push(MenuEntry::Item { label: format!("      {label}"), icon, msg: UiMsg::Task(action) });
        items.push(MenuEntry::Item { label: "      Finish".into(), icon: "emblem-default", msg: UiMsg::Task(Action::Finish(id)) });
        items.push(MenuEntry::Separator);
    }
    items.push(MenuEntry::Item { label: "Show / hide".into(), icon: "", msg: UiMsg::ToggleWindow });
    items.push(MenuEntry::Separator);
    items.push(MenuEntry::Item { label: "Quit".into(), icon: "application-exit", msg: UiMsg::Quit });
    items
}

/// Menu lines shouldn't get too long.
pub fn short_title(title: &str) -> String {
    let mut s: String = title.chars().take(48).collect();
    if title.chars().count() > 48 {
        s.push('…');
    }
    s
}

/// A menu message as a string id (for menu systems that identify entries by string).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn msg_id(msg: UiMsg) -> String {
    match msg {
        UiMsg::ToggleWindow => "toggle".into(),
        UiMsg::ShowWindow => "show".into(),
        UiMsg::Quit => "quit".into(),
        UiMsg::Task(Action::Pause(id)) => format!("pause:{id}"),
        UiMsg::Task(Action::Resume(id)) => format!("resume:{id}"),
        UiMsg::Task(Action::Finish(id)) => format!("finish:{id}"),
        UiMsg::Task(a) => format!("other:{a:?}"),
    }
}

/// The reverse of [`msg_id`] (only for messages the menu can produce).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_msg_id(id: &str) -> Option<UiMsg> {
    Some(match id {
        "toggle" => UiMsg::ToggleWindow,
        "show" => UiMsg::ShowWindow,
        "quit" => UiMsg::Quit,
        _ => {
            let (kind, n) = id.split_once(':')?;
            let n: i64 = n.parse().ok()?;
            UiMsg::Task(match kind {
                "pause" => Action::Pause(n),
                "resume" => Action::Resume(n),
                "finish" => Action::Finish(n),
                _ => return None,
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tasks(running: &[(i64, &str, bool)]) -> Vec<RunningTask> {
        running.iter().map(|&(run_id, t, paused)| RunningTask { run_id, title: t.into(), paused }).collect()
    }

    #[test]
    fn icon_color_follows_the_tasks() {
        assert_eq!(icon_state(&tasks(&[])), IconState::Idle);
        assert_eq!(icon_state(&tasks(&[(1, "a", true)])), IconState::Paused);
        assert_eq!(icon_state(&tasks(&[(1, "a", true), (2, "b", false)])), IconState::Running, "running wins");
    }

    #[test]
    fn menu_shows_tasks_first() {
        let labels: Vec<String> = menu(&tasks(&[(1, "Paused one", true), (2, "Busy work", false)]))
            .into_iter()
            .filter_map(|m| match m {
                MenuEntry::Heading(l) | MenuEntry::Item { label: l, .. } => Some(l),
                MenuEntry::Separator => None,
            })
            .collect();
        assert_eq!(labels, [
            "▶ Busy work",
            "      Pause",
            "      Finish",
            "⏸ Paused one",
            "      Resume",
            "      Finish",
            "Show / hide",
            "Quit"
        ]);
        assert_eq!(tooltip(&tasks(&[(1, "a", true), (2, "b", false)])), "⏸ a\n▶ b");
    }

    #[test]
    fn long_titles_are_shortened() {
        let l = short_title(&"x".repeat(60));
        assert_eq!(l.chars().count(), 49);
        assert!(l.ends_with('…'));
        assert_eq!(short_title("short"), "short");
    }

    #[test]
    fn menu_ids_round_trip() {
        for e in menu(&tasks(&[(7, "a", true), (8, "b", false)])) {
            if let MenuEntry::Item { msg, .. } = e {
                assert_eq!(parse_msg_id(&msg_id(msg)), Some(msg));
            }
        }
        assert_eq!(parse_msg_id("pause:x"), None);
        assert_eq!(parse_msg_id("nonsense"), None);
    }
}
