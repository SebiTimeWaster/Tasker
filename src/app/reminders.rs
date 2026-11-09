//! Reminders: which ones should be open ([`wanted_reminders`]), and the small always-on-top
//! windows in a screen corner that show them.

use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate};
use egui::{
    Align, Color32, Frame, Layout, Margin, Pos2, RichText, Stroke, Ui, Vec2, ViewportBuilder, ViewportCommand,
    ViewportId,
};

use super::{TaskerApp, now_min, now_ts, today};
use crate::action::Action;
use crate::audio::PlayRequest;
use crate::model::*;
use crate::settings::{Corner, fmt_snooze};
use crate::store::{NotifState, Store};
use crate::widgets::{lerp_color, start_offset_labels, task_buttons};

const NOTIF_SIZE: Vec2 = Vec2::new(400.0, 158.0);
const NOTIF_MARGIN: f32 = 14.0;
const NOTIF_GAP: f32 = 10.0;
/// How long after its start time a reminder reaches full red.
const OVERDUE_FULL_RED_MIN: f32 = 60.0;
/// The "starts soon" announcement closes on its own after this long.
const AHEAD_SHOW_SECS: u64 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotifKind {
    /// A virtual task's start time has passed and it hasn't been started.
    Due { task_id: i64, date: NaiveDate },
    /// A running task has exceeded its estimate.
    Overrun { run_id: i64 },
    /// A task starts in a few minutes (the task's "notify ahead" time): a short, silent
    /// announcement that closes on its own.
    Ahead { task_id: i64, date: NaiveDate },
}

impl NotifKind {
    fn key(&self) -> String {
        match self {
            NotifKind::Due { task_id, date } => format!("due:{task_id}:{date}"),
            NotifKind::Overrun { run_id } => format!("over:{run_id}"),
            NotifKind::Ahead { task_id, date } => format!("ahead:{task_id}:{date}"),
        }
    }
    /// A due reminder can only be snoozed or resolved by starting the task; it is meant
    /// to stay annoying. Over-estimate reminders and announcements can be dismissed.
    fn dismissable(&self) -> bool {
        !matches!(self, NotifKind::Due { .. })
    }
}

pub(super) struct Notif {
    kind: NotifKind,
    seq: u64,
    /// After clicking a task button the notification stays for a second, then closes.
    closing_at: Option<Instant>,
    /// Closes on its own at this time (announcements).
    expires_at: Option<Instant>,
    initial_pos: Pos2,
    last_pos: Pos2,
    frames: u32,
    hidden_from_taskbar: bool,
    /// UI scale the window was last sized for.
    zoom: f32,
}

enum NotifAction {
    Task(Action),
    Snooze(u32),
    Dismiss,
}

impl TaskerApp {
    pub(super) fn tick_notifications(&mut self) {
        let now = now_ts();
        let mut wanted = wanted_reminders(&self.store, today(), now_min(), now);

        let instant = Instant::now();
        // Announcements that timed out count as dismissed (they don't come back).
        let expired: Vec<String> =
            self.notifs.iter().filter(|n| n.expires_at.is_some_and(|t| instant >= t)).map(|n| n.kind.key()).collect();
        for key in &expired {
            let r = self.store.set_notif(key, NotifState { dismissed: true, snooze_until: None });
            self.report(r);
        }
        wanted.retain(|k| !expired.contains(&k.key()));
        self.notifs.retain(|n| {
            let keep = match n.closing_at {
                Some(t) => instant < t,
                None => wanted.contains(&n.kind),
            };
            if !keep {
                crate::logfile::info(format!("reminder #{} closed ({})", n.seq, n.kind.key()));
            }
            keep
        });
        for k in wanted {
            if !self.notifs.iter().any(|n| n.kind == k) {
                self.notif_seq += 1;
                let title = self.notif_entry(k, now).map(|e| e.title).unwrap_or_default();
                crate::logfile::info(format!(
                    "reminder #{} opening ({}, \"{title}\", {} open)",
                    self.notif_seq,
                    k.key(),
                    self.notifs.len() + 1
                ));
                self.notifs.push(Notif {
                    kind: k,
                    seq: self.notif_seq,
                    closing_at: None,
                    expires_at: matches!(k, NotifKind::Ahead { .. })
                        .then(|| instant + Duration::from_secs(AHEAD_SHOW_SECS)),
                    initial_pos: Pos2::ZERO,
                    last_pos: Pos2::ZERO,
                    frames: 0,
                    hidden_from_taskbar: false,
                    zoom: 0.0,
                });
            }
        }

        // Sound: loop while any open (not closing) notification wants sound.
        if let Some(until) = self.sound_test_until {
            if instant < until {
                return;
            }
            self.sound_test_until = None;
            self.audio.stop();
        }
        let want_sound =
            self.notifs.iter().any(|n| n.closing_at.is_none() && self.notif_mode(n.kind) == NotifyMode::Sound);
        if want_sound && !self.audio.is_playing() {
            self.audio.play(PlayRequest::from_settings(&self.settings, None));
        } else if !want_sound && self.audio.is_playing() {
            self.audio.stop();
        }
    }

    fn notif_mode(&self, kind: NotifKind) -> NotifyMode {
        match kind {
            NotifKind::Due { task_id, .. } => self.store.task(task_id).map_or(NotifyMode::Off, |t| t.notify),
            NotifKind::Overrun { run_id } => self.store.run(run_id).map_or(NotifyMode::Off, |r| r.notify),
            // Announcements never play a sound.
            NotifKind::Ahead { .. } => NotifyMode::Silent,
        }
    }

    fn notif_entry(&self, kind: NotifKind, now: i64) -> Option<Entry> {
        match kind {
            NotifKind::Due { task_id, date } | NotifKind::Ahead { task_id, date } => {
                self.store.entries_for(date, now).into_iter().find(|e| e.task_id == task_id)
            }
            NotifKind::Overrun { run_id } => {
                let r = self.store.run(run_id)?;
                Some(Entry::from_run(r, self.store.task(r.task_id), now))
            }
        }
    }

    /// Usable area of the primary monitor, in egui points.
    fn notif_area(&mut self, ctx: &egui::Context) -> egui::Rect {
        let ppp = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0) * ctx.zoom_factor();
        // Cached in physical pixels: the UI scale may change while it's cached.
        let px = match self.work_area {
            Some((at, r)) if at.elapsed() < Duration::from_secs(10) => r,
            _ => {
                let r = self
                    .screen
                    .as_ref()
                    .and_then(|x| x.work_area())
                    .map(|(x, y, w, h)| {
                        egui::Rect::from_min_size(egui::pos2(x as f32, y as f32), egui::vec2(w as f32, h as f32))
                    })
                    .unwrap_or_else(|| {
                        let size = ctx.input(|i| i.viewport().monitor_size).unwrap_or(egui::vec2(1920.0, 1080.0));
                        egui::Rect::from_min_size(Pos2::ZERO, size * ctx.pixels_per_point())
                    });
                self.work_area = Some((Instant::now(), r));
                r
            }
        };
        egui::Rect::from_min_max(px.min / ppp, px.max / ppp)
    }

    fn notif_pos(&self, area: egui::Rect, index: usize) -> Pos2 {
        let step = (NOTIF_SIZE.y + NOTIF_GAP) * index as f32;
        let (left, top) = match self.settings.corner {
            Corner::TopLeft | Corner::BottomLeft => (true, matches!(self.settings.corner, Corner::TopLeft)),
            Corner::TopRight | Corner::BottomRight => (false, matches!(self.settings.corner, Corner::TopRight)),
        };
        let x = if left { area.left() + NOTIF_MARGIN } else { area.right() - NOTIF_MARGIN - NOTIF_SIZE.x };
        let y = if top { area.top() + NOTIF_MARGIN + step } else { area.bottom() - NOTIF_MARGIN - NOTIF_SIZE.y - step };
        egui::pos2(x, y)
    }

    pub(super) fn show_notifications(&mut self, ctx: &egui::Context) {
        if self.notifs.is_empty() {
            return;
        }
        let area = self.notif_area(ctx);
        let now = now_ts();
        let mut actions: Vec<(usize, NotifAction)> = Vec::new();
        for i in 0..self.notifs.len() {
            let pos = self.notif_pos(area, i);
            let kind = self.notifs[i].kind;
            let Some(entry) = self.notif_entry(kind, now) else { continue };
            let n = &mut self.notifs[i];
            if n.frames == 0 {
                n.initial_pos = pos;
                n.last_pos = pos;
            }
            n.frames += 1;
            let zoom = ctx.zoom_factor();
            let resize = n.zoom != zoom;
            n.zoom = zoom;
            let move_to = (n.last_pos != pos || n.frames <= 3 || resize).then_some(pos);
            n.last_pos = pos;
            let title = format!("Tasker reminder #{}", n.seq);
            let builder = ViewportBuilder::default()
                .with_title(title.clone())
                .with_app_id("tasker-notification")
                .with_inner_size(NOTIF_SIZE)
                .with_position(n.initial_pos)
                .with_decorations(false)
                .with_resizable(false)
                .with_always_on_top()
                .with_active(false)
                .with_taskbar(false)
                .with_window_type(egui::X11WindowType::Utility);
            let closing = n.closing_at.is_some();
            let expires_in = n.expires_at.map(|t| t.saturating_duration_since(Instant::now()));
            let id = ViewportId::from_hash_of(("tasker-notif", n.seq));
            let this = &*self;
            let action = ctx.show_viewport_immediate(id, builder, |ui, _class| {
                if resize {
                    ui.ctx().send_viewport_cmd(ViewportCommand::InnerSize(NOTIF_SIZE));
                }
                if let Some(p) = move_to {
                    ui.ctx().send_viewport_cmd(ViewportCommand::OuterPosition(p));
                }
                if ui.input(|i| i.viewport().close_requested()) {
                    if !kind.dismissable() {
                        // Alt+F4 & co. must not get rid of a due reminder either.
                        ui.ctx().send_viewport_cmd(ViewportCommand::CancelClose);
                    } else {
                        return Some(NotifAction::Dismiss);
                    }
                }
                this.notification_ui(ui, kind, &entry, closing, expires_in, now)
            });
            if let Some(a) = action {
                actions.push((i, a));
            }
            let n = &mut self.notifs[i];
            if !n.hidden_from_taskbar
                && n.frames < 120
                && let Some(x) = &self.screen
            {
                n.hidden_from_taskbar = x.hide_from_taskbar(&title);
            }
        }

        let mut remove = Vec::new();
        for (i, a) in actions {
            let key = self.notifs[i].kind.key();
            let what = match &a {
                NotifAction::Task(act) => format!("{act:?}"),
                NotifAction::Snooze(m) => format!("snooze {m} min"),
                NotifAction::Dismiss => "dismiss".into(),
            };
            crate::logfile::info(format!("reminder #{} ({key}): {what}", self.notifs[i].seq));
            match a {
                NotifAction::Task(act) => {
                    self.apply(act);
                    let r = self.store.set_notif(&key, NotifState { dismissed: true, snooze_until: None });
                    self.report(r);
                    self.notifs[i].closing_at = Some(Instant::now() + Duration::from_secs(1));
                }
                NotifAction::Snooze(min) => {
                    let until = now + min as i64 * 60;
                    let r = self.store.set_notif(&key, NotifState { dismissed: false, snooze_until: Some(until) });
                    self.report(r);
                    remove.push(i);
                }
                NotifAction::Dismiss => {
                    let r = self.store.set_notif(&key, NotifState { dismissed: true, snooze_until: None });
                    self.report(r);
                    remove.push(i);
                }
            }
        }
        for i in remove.into_iter().rev() {
            self.notifs.remove(i);
        }
        if !self.notifs.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn notification_ui(
        &self,
        ui: &mut Ui,
        kind: NotifKind,
        e: &Entry,
        closing: bool,
        expires_in: Option<Duration>,
        now: i64,
    ) -> Option<NotifAction> {
        if let NotifKind::Ahead { date, .. } = kind {
            return self.announcement_ui(ui, e, date, expires_in);
        }
        let overdue_min = match kind {
            NotifKind::Due { date, .. } => {
                (Local::now().naive_local() - scheduled_at(date, e.start_min)).num_seconds() as f32 / 60.0
            }
            NotifKind::Overrun { .. } => (e.tracked_secs as f32 / 60.0) - e.estimate_min as f32,
            NotifKind::Ahead { .. } => 0.0,
        };
        let t = (overdue_min / OVERDUE_FULL_RED_MIN).clamp(0.0, 1.0);
        let bg = lerp_color(Color32::from_rgb(38, 40, 46), Color32::from_rgb(150, 28, 28), t);
        let mut out = None;
        let frame =
            Frame::new().fill(bg).stroke(Stroke::new(2.0, e.priority.color())).inner_margin(Margin::symmetric(14, 10));
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("⏰").size(18.0));
                ui.label(RichText::new(&e.title).size(17.0).strong().color(Color32::WHITE));
                if kind.dismissable() {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(egui::Button::new("❌").frame(false)).on_hover_text("Dismiss").clicked() {
                            out = Some(NotifAction::Dismiss);
                        }
                    });
                }
            });
            let info = match kind {
                NotifKind::Due { .. } => {
                    format!(
                        "Due {} · est. {} · {} priority",
                        fmt_hm(e.start_min),
                        fmt_duration_min(e.estimate_min),
                        e.priority.label()
                    )
                }
                NotifKind::Overrun { .. } => format!(
                    "Estimate of {} exceeded by {}",
                    fmt_duration_min(e.estimate_min),
                    fmt_duration_min(overdue_min.max(0.0) as u32)
                ),
                NotifKind::Ahead { .. } => String::new(),
            };
            ui.label(RichText::new(info).color(Color32::from_gray(225)));
            if e.run_id.is_some() {
                ui.label(
                    RichText::new(format!("⏱ {}", fmt_hms(e.tracked_secs)))
                        .monospace()
                        .size(15.0)
                        .color(Color32::WHITE),
                );
            }
            ui.add_space(2.0);
            ui.add_enabled_ui(!closing, |ui| {
                ui.horizontal(|ui| {
                    if let Some(a) = task_buttons(ui, e) {
                        out = Some(NotifAction::Task(a));
                    }
                    start_offset_labels(ui, e, now, None, true);
                });
                match kind {
                    NotifKind::Due { .. } => {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(RichText::new("Snooze").small().color(Color32::from_gray(210)));
                            for &m in &self.settings.snooze_minutes {
                                if ui.small_button(fmt_snooze(m)).clicked() {
                                    out = Some(NotifAction::Snooze(m));
                                }
                            }
                        });
                    }
                    // Over the estimate: no snoozing, just acknowledge it (it won't come back).
                    NotifKind::Overrun { .. } | NotifKind::Ahead { .. } => {
                        let tip = "Close this reminder; it won't be shown again for this task";
                        if ui.small_button("✕ Dismiss").on_hover_text(tip).clicked() {
                            out = Some(NotifAction::Dismiss);
                        }
                    }
                }
            });
        });
        if !closing {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        out
    }

    /// "Starts soon" announcement: what and when, a Dismiss button, and the countdown
    /// until it closes on its own.
    fn announcement_ui(
        &self,
        ui: &mut Ui,
        e: &Entry,
        date: NaiveDate,
        expires_in: Option<Duration>,
    ) -> Option<NotifAction> {
        let mut out = None;
        let frame = Frame::new()
            .fill(Color32::from_rgb(34, 44, 58))
            .stroke(Stroke::new(2.0, e.priority.color()))
            .inner_margin(Margin::symmetric(14, 10));
        let secs = (scheduled_at(date, e.start_min) - Local::now().naive_local()).num_seconds().max(0);
        let in_min = (secs + 59) / 60;
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("⏳").size(18.0));
                ui.label(RichText::new(&e.title).size(17.0).strong().color(Color32::WHITE));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(egui::Button::new("❌").frame(false)).on_hover_text("Dismiss").clicked() {
                        out = Some(NotifAction::Dismiss);
                    }
                });
            });
            let when = if in_min <= 1 { "in 1 min".to_string() } else { format!("in {in_min} min") };
            ui.label(
                RichText::new(format!("Starts {when}, at {}", fmt_hm(e.start_min))).size(15.0).color(Color32::WHITE),
            );
            ui.label(
                RichText::new(format!("est. {} · {} priority", fmt_duration_min(e.estimate_min), e.priority.label()))
                    .color(Color32::from_gray(210)),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.small_button("✕ Dismiss").clicked() {
                    out = Some(NotifAction::Dismiss);
                }
                if let Some(left) = expires_in {
                    let s = left.as_secs_f32().ceil() as u32;
                    ui.label(RichText::new(format!("closes in {s} s")).color(Color32::from_gray(170)));
                }
            });
        });
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        out
    }
}

/// Which reminders should be open on `today` at minute-of-day `nm` (timestamp `now`):
/// - due: a not-yet-started occurrence whose start time has passed (if it has reminders),
/// - ahead: a not-yet-started occurrence starting within its "notify ahead" time,
/// - overrun: a running task that has used up its estimate (if it has reminders).
///
/// Dismissed reminders and ones snoozed past `now` are left out.
fn wanted_reminders(store: &Store, today: NaiveDate, nm: u32, now: i64) -> Vec<NotifKind> {
    let mut wanted: Vec<NotifKind> = Vec::new();
    for e in store.entries_for(today, now) {
        if e.status != EntryStatus::Virtual {
            continue;
        }
        if e.notify != NotifyMode::Off && e.start_min <= nm {
            wanted.push(NotifKind::Due { task_id: e.task_id, date: today });
        }
        if e.notify_ahead_min > 0 && nm < e.start_min && nm + e.notify_ahead_min >= e.start_min {
            wanted.push(NotifKind::Ahead { task_id: e.task_id, date: today });
        }
    }
    for r in &store.runs {
        if r.status == RunStatus::Running
            && r.notify != NotifyMode::Off
            && r.estimate_min > 0
            && r.tracked_secs(now) >= r.estimate_min as i64 * 60
        {
            wanted.push(NotifKind::Overrun { run_id: r.id });
        }
    }
    wanted.retain(|k| {
        let st = store.notif(&k.key());
        !st.dismissed && st.snooze_until.is_none_or(|t| t <= now)
    });
    wanted
}

/// Snooze time of the reminder belonging to this entry, if any.
pub fn snoozed_until(store: &Store, e: &Entry) -> Option<i64> {
    let key = match (e.status, e.run_id) {
        (EntryStatus::Virtual, _) => NotifKind::Due { task_id: e.task_id, date: e.date }.key(),
        (EntryStatus::Running, Some(id)) => NotifKind::Overrun { run_id: id }.key(),
        _ => return None,
    };
    let st = store.notif(&key);
    if st.dismissed { None } else { st.snooze_until }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 6).unwrap()
    }

    /// A store with one task today at 09:00 (30 min estimate).
    fn store_with(f: impl FnOnce(&mut Task)) -> (Store, i64) {
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        let mut t = test_task("Standup", Schedule::Date { date: day(), repeat: Repeat::Never }, 9 * 60, 30);
        f(&mut t);
        let id = s.save_task(t).unwrap();
        (s, id)
    }

    #[test]
    fn due_reminder_from_start_time_until_started() {
        let (mut s, id) = store_with(|_| {});
        let due = NotifKind::Due { task_id: id, date: day() };
        assert!(wanted_reminders(&s, day(), 8 * 60 + 59, 0).is_empty(), "not before the start");
        assert_eq!(wanted_reminders(&s, day(), 9 * 60, 0), [due]);
        assert_eq!(wanted_reminders(&s, day(), 15 * 60, 0), [due], "stays while overdue");
        s.start(id, day(), 100).unwrap();
        assert!(wanted_reminders(&s, day(), 15 * 60, 200).is_empty(), "gone once started");

        let (s, _) = store_with(|t| t.notify = NotifyMode::Off);
        assert!(wanted_reminders(&s, day(), 10 * 60, 0).is_empty(), "no reminder when switched off");
    }

    #[test]
    fn announcement_window() {
        let (s, id) = store_with(|t| {
            t.notify_ahead_min = 10;
            t.notify = NotifyMode::Off; // announcements don't depend on the reminder setting
        });
        let ahead = NotifKind::Ahead { task_id: id, date: day() };
        assert!(wanted_reminders(&s, day(), 8 * 60 + 49, 0).is_empty());
        assert_eq!(wanted_reminders(&s, day(), 8 * 60 + 50, 0), [ahead]);
        assert_eq!(wanted_reminders(&s, day(), 8 * 60 + 59, 0), [ahead]);
        assert!(wanted_reminders(&s, day(), 9 * 60, 0).is_empty(), "ends at the start time");
    }

    #[test]
    fn overrun_after_the_estimate() {
        let (mut s, id) = store_with(|_| {});
        let run = s.start(id, day(), 1_000).unwrap();
        let over = NotifKind::Overrun { run_id: run };
        assert!(wanted_reminders(&s, day(), 10 * 60, 1_000 + 29 * 60).is_empty());
        assert_eq!(wanted_reminders(&s, day(), 10 * 60, 1_000 + 30 * 60), [over]);
        s.pause(run, 1_000 + 31 * 60).unwrap();
        assert!(wanted_reminders(&s, day(), 10 * 60, 9_999).is_empty(), "not while paused");
        // added earlier work counts towards the estimate too
        let (mut s, id) = store_with(|_| {});
        let run = s.start(id, day(), 1_000).unwrap();
        s.add_time(run, 25 * 60).unwrap();
        assert_eq!(wanted_reminders(&s, day(), 10 * 60, 1_000 + 5 * 60), [NotifKind::Overrun { run_id: run }]);
    }

    #[test]
    fn dismissed_and_snoozed_reminders_stay_closed() {
        let (mut s, id) = store_with(|_| {});
        let due = NotifKind::Due { task_id: id, date: day() };
        s.set_notif(&due.key(), NotifState { dismissed: false, snooze_until: Some(5_000) }).unwrap();
        assert!(wanted_reminders(&s, day(), 10 * 60, 4_999).is_empty(), "snoozed");
        assert_eq!(wanted_reminders(&s, day(), 10 * 60, 5_000), [due], "back after the snooze");
        s.set_notif(&due.key(), NotifState { dismissed: true, snooze_until: None }).unwrap();
        assert!(wanted_reminders(&s, day(), 10 * 60, 9_000).is_empty(), "dismissed");
        let tomorrow = day().succ_opt().unwrap();
        assert!(wanted_reminders(&s, tomorrow, 10 * 60, 9_000).is_empty(), "a one-off task isn't due on other days");
    }

    #[test]
    fn only_due_reminders_cant_be_dismissed() {
        assert!(!NotifKind::Due { task_id: 1, date: day() }.dismissable());
        assert!(NotifKind::Overrun { run_id: 1 }.dismissable());
        assert!(NotifKind::Ahead { task_id: 1, date: day() }.dismissable());
    }

    #[test]
    fn reminder_keys_are_stable() {
        // Stored in the database: changing them would bring back dismissed reminders.
        assert_eq!(NotifKind::Due { task_id: 3, date: day() }.key(), "due:3:2026-11-06");
        assert_eq!(NotifKind::Ahead { task_id: 3, date: day() }.key(), "ahead:3:2026-11-06");
        assert_eq!(NotifKind::Overrun { run_id: 7 }.key(), "over:7");
    }
}
