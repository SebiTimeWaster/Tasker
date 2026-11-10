//! The panel / notification-area icon: what it shows and what its menu offers. Drawing it
//! is up to the platform (`platform::Tray`: D-Bus StatusNotifierItem on Linux, the
//! notification area on Windows); both use the model built here.

use crate::action::Action;
use crate::icon::IconState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMsg {
    ToggleWindow,
    ShowWindow,
    Quit,
    /// Pause, finish, undo … chosen in the panel icon's menu.
    Task(Action),
}

/// What a task in the panel icon's menu is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrayState {
    Running,
    Paused,
    /// Finished: only listed while that can still be undone.
    Done,
    /// Given up: only listed while that can still be undone.
    Failed,
}

/// A task as shown in the panel icon's menu: every running or paused one, plus the ones
/// that were started or given up a moment ago (`undo`: the menu offers to take that back).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayTask {
    pub run_id: i64,
    pub title: String,
    pub state: TrayState,
    pub undo: bool,
}

/// Blue when idle, yellow while a task is paused, green while one runs (green wins).
pub fn icon_state(tasks: &[TrayTask]) -> IconState {
    if tasks.iter().any(|t| t.state == TrayState::Running) {
        IconState::Running
    } else if tasks.iter().any(|t| t.state == TrayState::Paused) {
        IconState::Paused
    } else {
        IconState::Idle
    }
}

/// The running and paused tasks, one per line.
pub fn tooltip(tasks: &[TrayTask]) -> String {
    let line = |t: &TrayTask| match t.state {
        TrayState::Running => Some(format!("▶ {}", t.title)),
        TrayState::Paused => Some(format!("⏸ {}", t.title)),
        TrayState::Done | TrayState::Failed => None,
    };
    tasks.iter().filter_map(line).collect::<Vec<_>>().join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// A greyed-out line (a task's title).
    Heading(String),
    /// A clickable entry. `icon` is a freedesktop icon name (ignored where unsupported).
    Item {
        label: String,
        icon: &'static str,
        msg: UiMsg,
    },
    Separator,
}

/// The icon's menu: per task its title, then Pause or Resume and Finish while it is open,
/// and Undo while its start or give-up can be taken back (running tasks first); then
/// Show / hide and Quit. Labels are plain text; each platform escapes its own mnemonic
/// markers.
pub fn menu(tasks: &[TrayTask]) -> Vec<MenuEntry> {
    let mut items = Vec::new();
    let mut tasks: Vec<&TrayTask> = tasks.iter().collect();
    tasks.sort_by_key(|t| t.state);
    for t in tasks {
        let id = t.run_id;
        let mark = match t.state {
            TrayState::Running => "▶",
            TrayState::Paused => "⏸",
            TrayState::Done => "✔",
            TrayState::Failed => "✖",
        };
        items.push(MenuEntry::Heading(format!("{mark} {}", short_title(&t.title))));
        let mut item = |label: &str, icon: &'static str, action: Action| {
            items.push(MenuEntry::Item { label: format!("      {label}"), icon, msg: UiMsg::Task(action) });
        };
        match t.state {
            TrayState::Running => item("Pause", "media-playback-pause", Action::Pause(id)),
            TrayState::Paused => item("Resume", "media-playback-start", Action::Resume(id)),
            TrayState::Done | TrayState::Failed => {}
        }
        if matches!(t.state, TrayState::Running | TrayState::Paused) {
            item("Finish", "emblem-default", Action::Finish(id));
        }
        if t.undo {
            let label = if t.state == TrayState::Failed { "Undo give-up" } else { "Undo start" };
            item(label, "edit-undo", Action::Undo(id));
        }
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
        UiMsg::Task(Action::Undo(id)) => format!("undo:{id}"),
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
                "undo" => Action::Undo(n),
                _ => return None,
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Running or paused tasks that can't be undone any more.
    fn tasks(running: &[(i64, &str, bool)]) -> Vec<TrayTask> {
        let state = |paused| if paused { TrayState::Paused } else { TrayState::Running };
        running.iter().map(|&(run_id, t, paused)| task(run_id, t, state(paused), false)).collect()
    }

    fn task(run_id: i64, title: &str, state: TrayState, undo: bool) -> TrayTask {
        TrayTask { run_id, title: title.into(), state, undo }
    }

    #[test]
    fn menu_offers_undo_while_possible() {
        let tasks = [
            task(1, "Given up", TrayState::Failed, true),
            task(2, "Old", TrayState::Paused, false),
            task(3, "Just started", TrayState::Running, true),
            task(4, "Quick one", TrayState::Done, true),
        ];
        let lines: Vec<(String, Option<UiMsg>)> = menu(&tasks)
            .into_iter()
            .filter_map(|m| match m {
                MenuEntry::Heading(l) => Some((l, None)),
                MenuEntry::Item { label, msg, .. } => Some((label.trim().to_string(), Some(msg))),
                MenuEntry::Separator => None,
            })
            .collect();
        let item = |label: &str, a: Action| (label.to_string(), Some(UiMsg::Task(a)));
        let heading = |l: &str| (l.to_string(), None);
        assert_eq!(
            lines[..10],
            [
                heading("▶ Just started"),
                item("Pause", Action::Pause(3)),
                item("Finish", Action::Finish(3)),
                item("Undo start", Action::Undo(3)),
                heading("⏸ Old"),
                item("Resume", Action::Resume(2)),
                item("Finish", Action::Finish(2)),
                heading("✔ Quick one"),
                item("Undo start", Action::Undo(4)),
                heading("✖ Given up"),
            ]
        );
        assert_eq!(lines[10], item("Undo give-up", Action::Undo(1)));
        // tasks that are only listed for their Undo don't count as running
        assert_eq!(icon_state(&tasks[..1]), IconState::Idle);
        assert_eq!(tooltip(&tasks), "⏸ Old\n▶ Just started");
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
        assert_eq!(
            labels,
            [
                "▶ Busy work",
                "      Pause",
                "      Finish",
                "⏸ Paused one",
                "      Resume",
                "      Finish",
                "Show / hide",
                "Quit"
            ]
        );
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
        let mut tasks = tasks(&[(7, "a", true), (8, "b", false)]);
        tasks[0].undo = true;
        tasks.push(task(9, "c", TrayState::Failed, true));
        for e in menu(&tasks) {
            if let MenuEntry::Item { msg, .. } = e {
                assert_eq!(parse_msg_id(&msg_id(msg)), Some(msg));
            }
        }
        assert_eq!(parse_msg_id("pause:x"), None);
        assert_eq!(parse_msg_id("nonsense"), None);
    }
}
