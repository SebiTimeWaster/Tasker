//! Reminders: which ones should be open ([`wanted_reminders`]), and the small always-on-top
//! windows in a screen corner that show them.

use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate};
use egui::{Color32, Frame, Margin, Pos2, RichText, Ui, Vec2, ViewportBuilder, ViewportCommand, ViewportId};

use super::{TaskerApp, minute_of_day, now_ts};
use crate::action::Action;
use crate::audio::PlayRequest;
use crate::model::*;
use crate::settings::{Corner, fmt_snooze};
use crate::store::{NotifState, Store};
use crate::views::TIMELINE_BG;
use crate::widgets::{
    ACTION_TEXT, Act, HEADLINE, TEXT, action_button, action_height, lerp_color, preset_button, preset_row,
    start_offset_labels, task_buttons,
};

/// Width of a reminder window; its height follows its content (see `Notif::height`).
const NOTIF_W: f32 = 440.0;
/// Space between the reminders and from them to the edges of the work area.
const NOTIF_GAP: f32 = 15.0;
/// Space between a reminder's edges (the priority bar on the left) and its content.
const REMINDER_MARGIN_X: f32 = 14.0;
/// Space above and below a reminder's content.
const REMINDER_MARGIN_Y: f32 = 12.0;
/// Space between the lines of a reminder.
const REMINDER_LINE_GAP: f32 = 12.0;
/// Width of the bar in the priority color along a reminder's left edge.
const REMINDER_BAR_W: f32 = 10.0;
/// How long after its start time a reminder reaches full red.
const OVERDUE_FULL_RED_MIN: f32 = 60.0;
/// The "starts soon" announcement closes on its own after this long.
const AHEAD_SHOW_SECS: u64 = 10;
/// A due reminder has to be endured (sound included) for this long before its
/// "I am bad and won't do it" button can be clicked. Deliberately not a setting.
const GIVE_UP_WAIT_SECS: u64 = 10;

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
    /// A due reminder can only be snoozed or resolved by starting the task or giving it
    /// up; it is meant to stay annoying. Over-estimate reminders and announcements can be
    /// dismissed.
    fn dismissable(&self) -> bool {
        !matches!(self, NotifKind::Due { .. })
    }

    /// Window height to open with, before the content has been measured (which
    /// corrects it in the next frame): due reminders have the extra snooze row.
    fn height_guess(&self) -> f32 {
        if matches!(self, NotifKind::Due { .. }) { 182.0 } else { 150.0 }
    }
}

pub(super) struct Notif {
    kind: NotifKind,
    seq: u64,
    opened_at: Instant,
    /// After clicking a task button the notification stays for a second, then closes.
    closing_at: Option<Instant>,
    /// Meanwhile it shows the task as it was when clicked (the running task would need
    /// more room than the window has) and what was done instead of the buttons.
    frozen: Option<(Entry, &'static str)>,
    /// Closes on its own at this time (announcements).
    expires_at: Option<Instant>,
    initial_pos: Pos2,
    last_pos: Pos2,
    frames: u32,
    hidden_from_taskbar: bool,
    /// Where the window ended up has been written to the log.
    pos_logged: bool,
    /// UI scale the window was last sized for.
    zoom: f32,
    /// Height (points) the window was last sized to: what its content needed in the
    /// previous frame, at first a guess for its kind.
    height: f32,
}

enum NotifAction {
    Task(Action),
    Snooze(u32),
    Dismiss,
}

impl TaskerApp {
    pub(super) fn tick_notifications(&mut self) {
        // One reading of the clock for all three: around midnight, separate ones could
        // disagree about the day.
        let local = Local::now();
        let now = local.timestamp();
        let mut wanted = wanted_reminders(&self.store, local.date_naive(), minute_of_day(local.time()), now);

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
                    opened_at: instant,
                    closing_at: None,
                    frozen: None,
                    expires_at: matches!(k, NotifKind::Ahead { .. })
                        .then(|| instant + Duration::from_secs(AHEAD_SHOW_SECS)),
                    initial_pos: Pos2::ZERO,
                    last_pos: Pos2::ZERO,
                    frames: 0,
                    hidden_from_taskbar: false,
                    pos_logged: false,
                    zoom: 0.0,
                    height: k.height_guess(),
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
                self.store.entry(task_id, date, now)
            }
            NotifKind::Overrun { run_id } => {
                let r = self.store.run(run_id)?;
                Some(Entry::from_run(r, self.store.task(r.task_id), now))
            }
        }
    }

    /// Usable area of the primary monitor, in physical pixels (cached for a while).
    fn notif_area_px(&mut self, ctx: &egui::Context) -> egui::Rect {
        match self.work_area {
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
                // Log the monitor data whenever the area changes (and at the first reminder):
                // placement problems are hard to diagnose from a description of the screen.
                if self.work_area.is_none_or(|(_, old)| old != r) {
                    let raw = self.screen.as_ref().map_or("no screen info".into(), |x| x.describe());
                    crate::logfile::info(format!("reminder area {r:?} (physical pixels); {raw}"));
                }
                self.work_area = Some((Instant::now(), r));
                r
            }
        }
    }

    /// Position (in points) of a reminder window `height` points high, `step` points
    /// away from the corner (the windows before it, gaps included), in the work area
    /// `area_px` (physical pixels), for a window with `ppp` pixels per point.
    fn notif_pos(&self, area_px: egui::Rect, step: f32, height: f32, ppp: f32) -> Pos2 {
        let area = egui::Rect::from_min_max(area_px.min / ppp, area_px.max / ppp);
        let (left, top) = match self.settings.corner {
            Corner::TopLeft | Corner::BottomLeft => (true, matches!(self.settings.corner, Corner::TopLeft)),
            Corner::TopRight | Corner::BottomRight => (false, matches!(self.settings.corner, Corner::TopRight)),
        };
        let x = if left { area.left() + NOTIF_GAP } else { area.right() - NOTIF_GAP - NOTIF_W };
        let y = if top { area.top() + NOTIF_GAP + step } else { area.bottom() - NOTIF_GAP - height - step };
        egui::pos2(x, y)
    }

    pub(super) fn show_notifications(&mut self, ctx: &egui::Context) {
        if self.notifs.is_empty() {
            return;
        }
        // Positions are converted to physical pixels with the scale factor of the reminder
        // window itself, which differs from the main window's when that is on another
        // monitor with other scaling. Only that factor puts them exactly into the corner.
        let area = self.notif_area_px(ctx);
        let main_ppp = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0) * ctx.zoom_factor();
        let now = now_ts();
        let mut actions: Vec<(usize, NotifAction)> = Vec::new();
        // stacked from the corner: each one as high as its content
        let mut step = 0.0;
        for i in 0..self.notifs.len() {
            let kind = self.notifs[i].kind;
            let (entry, done) = match &self.notifs[i].frozen {
                Some((e, done)) => (e.clone(), Some(*done)),
                None => match self.notif_entry(kind, now) {
                    Some(e) => (e, None),
                    None => continue,
                },
            };
            // Before the window exists only the main window's scale is known: a guess,
            // corrected below once the window reports its own.
            let height = self.notifs[i].height;
            let guess = self.notif_pos(area, step, height, main_ppp);
            step += height + NOTIF_GAP;
            let n = &mut self.notifs[i];
            if n.frames == 0 {
                n.initial_pos = guess;
                n.last_pos = guess;
            }
            n.frames += 1;
            let zoom = ctx.zoom_factor();
            let resize = n.zoom != zoom;
            n.zoom = zoom;
            let (last_pos, frames) = (n.last_pos, n.frames);
            let title = format!("Tasker reminder #{}", n.seq);
            let builder = ViewportBuilder::default()
                .with_title(title.clone())
                .with_app_id("tasker-notification")
                .with_inner_size(Vec2::new(NOTIF_W, height))
                .with_position(n.initial_pos)
                .with_decorations(false)
                .with_resizable(false)
                .with_always_on_top()
                .with_active(false)
                .with_taskbar(false)
                .with_window_type(egui::X11WindowType::Utility);
            let expires_in = n.expires_at.map(|t| t.saturating_duration_since(Instant::now()));
            let give_up_wait = give_up_wait_secs(n.opened_at.elapsed());
            let id = ViewportId::from_hash_of(("tasker-notif", n.seq));
            let this = &*self;
            let step_here = step - height - NOTIF_GAP;
            let (action, pos, placed, needed) = ctx.show_viewport_immediate(id, builder, |ui, _class| {
                // this viewport's own pixels per point (UI scale included)
                let ppp = ui.ctx().pixels_per_point();
                let pos = this.notif_pos(area, step_here, height, ppp);
                let placed = ui.input(|i| i.viewport().outer_rect).map(|r| (r.min, pos, ppp));
                if resize {
                    ui.ctx().send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(NOTIF_W, height)));
                }
                if last_pos != pos || frames <= 3 || resize {
                    ui.ctx().send_viewport_cmd(ViewportCommand::OuterPosition(pos));
                }
                if ui.input(|i| i.viewport().close_requested()) {
                    if !kind.dismissable() {
                        // Alt+F4 & co. must not get rid of a due reminder either.
                        ui.ctx().send_viewport_cmd(ViewportCommand::CancelClose);
                    } else {
                        return (Some(NotifAction::Dismiss), pos, placed, height);
                    }
                }
                let (action, needed) = this.notification_ui(ui, kind, &entry, done, expires_in, give_up_wait, now);
                (action, pos, placed, needed)
            });
            let n = &mut self.notifs[i];
            // Content taller or shorter than the window: resize it (and move it and the
            // ones after it) in the next frame.
            if (needed - n.height).abs() > 0.5 {
                n.height = needed;
                n.zoom = 0.0;
                ctx.request_repaint();
            }
            if let Some(a) = action {
                if let NotifAction::Task(act) = a {
                    n.frozen = Some((entry, done_label(act)));
                }
                actions.push((i, a));
            }
            n.last_pos = pos;
            // Once it has settled: where the window actually is (the window manager may
            // have moved it) next to where it was sent.
            if !n.pos_logged
                && n.opened_at.elapsed() > Duration::from_secs(2)
                && let Some((at, want, ppp)) = placed
            {
                n.pos_logged = true;
                crate::logfile::info(format!(
                    "reminder #{} window at {:.0},{:.0}, wanted {:.0},{:.0} (physical pixels, scale {ppp:.2})",
                    n.seq,
                    at.x * ppp,
                    at.y * ppp,
                    want.x * ppp,
                    want.y * ppp
                ));
            }
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
                    self.apply_click(act);
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

    #[allow(clippy::too_many_arguments)]
    fn notification_ui(
        &self,
        ui: &mut Ui,
        kind: NotifKind,
        e: &Entry,
        done: Option<&str>,
        expires_in: Option<Duration>,
        give_up_wait: u64,
        now: i64,
    ) -> (Option<NotifAction>, f32) {
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
        // Only the lines saying how late it is get the red, which deepens with lateness.
        let t = (overdue_min / OVERDUE_FULL_RED_MIN).clamp(0.0, 1.0);
        let alarm = lerp_color(Color32::from_rgb(92, 36, 38), Color32::from_rgb(150, 28, 28), t);
        let mut out = None;
        let height = reminder_frame(ui, TIMELINE_BG, e.priority.color(), |ui| {
            header(ui, "⏰", &e.title);
            let info = match kind {
                NotifKind::Due { date, .. } => {
                    format!(
                        "Due {} · est. {} · {} priority",
                        due_time(date, e.start_min, Local::now().date_naive()),
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
            let info_line = |ui: &mut Ui| {
                ui.label(RichText::new(info).color(Color32::from_gray(225)));
            };
            // How late it is (due: "12 min overdue"; running: "started 5 min late", next
            // to the tracked time), so the button row below has room for its buttons.
            let late_line = |ui: &mut Ui| {
                ui.horizontal(|ui| {
                    if e.run_id.is_some() {
                        ui.label(
                            RichText::new(format!("⏱ {}", fmt_hms(e.tracked_secs)))
                                .monospace()
                                .size(TEXT)
                                .color(Color32::WHITE),
                        );
                    }
                    start_offset_labels(ui, e, now, None, true);
                });
            };
            if let NotifKind::Due { .. } = kind {
                info_line(ui);
                alarm_band(ui, alarm, late_line);
            } else {
                // over the estimate: "exceeded by …" is what the red is about
                alarm_band(ui, alarm, |ui| {
                    info_line(ui);
                    late_line(ui);
                });
            }
            ui.horizontal(|ui| {
                ui.set_min_height(action_height(ui));
                if let Some(done) = done {
                    ui.label(RichText::new(done).size(ACTION_TEXT).strong().color(Color32::WHITE));
                    return;
                }
                if let Some(a) = task_buttons(ui, e, give_up_wait) {
                    out = Some(NotifAction::Task(a));
                }
                // Over the estimate: no snoozing, just acknowledge it (it won't come back).
                if kind.dismissable() {
                    let tip = "Close this reminder; it won't be shown again for this task";
                    if action_button(ui, "✕ Dismiss", Act::Plain, true).on_hover_text(tip).clicked() {
                        out = Some(NotifAction::Dismiss);
                    }
                }
            });
            if let NotifKind::Due { .. } = kind {
                ui.add_enabled_ui(done.is_none(), |ui| {
                    preset_row(ui, |ui| {
                        ui.label(RichText::new("Snooze").color(Color32::from_gray(210)));
                        for &m in &self.settings.snooze_minutes {
                            if preset_button(ui, false, fmt_snooze(m)).clicked() {
                                out = Some(NotifAction::Snooze(m));
                            }
                        }
                    });
                });
            }
        });
        if done.is_none() {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        (out, height)
    }

    /// "Starts soon" announcement: what and when, a Dismiss button, and the countdown
    /// until it closes on its own.
    fn announcement_ui(
        &self,
        ui: &mut Ui,
        e: &Entry,
        date: NaiveDate,
        expires_in: Option<Duration>,
    ) -> (Option<NotifAction>, f32) {
        let mut out = None;
        let secs = (scheduled_at(date, e.start_min) - Local::now().naive_local()).num_seconds().max(0);
        let in_min = (secs + 59) / 60;
        let height = reminder_frame(ui, TIMELINE_BG, e.priority.color(), |ui| {
            header(ui, "⏳", &e.title);
            let when = if in_min <= 1 { "in 1 min".to_string() } else { format!("in {in_min} min") };
            ui.label(
                RichText::new(format!("Starts {when}, at {}", fmt_hm(e.start_min))).size(TEXT).color(Color32::WHITE),
            );
            ui.label(
                RichText::new(format!("est. {} · {} priority", fmt_duration_min(e.estimate_min), e.priority.label()))
                    .color(Color32::from_gray(210)),
            );
            ui.horizontal(|ui| {
                if action_button(ui, "✕ Dismiss", Act::Plain, true).clicked() {
                    out = Some(NotifAction::Dismiss);
                }
                if let Some(left) = expires_in {
                    let s = left.as_secs_f32().ceil() as u32;
                    ui.label(RichText::new(format!("closes in {s} s")).color(Color32::from_gray(170)));
                }
            });
        });
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        (out, height)
    }
}

/// What a reminder shows for the second between a task button's click and closing.
fn done_label(a: Action) -> &'static str {
    match a {
        Action::Start { .. } => "▶ Started",
        Action::Fail { .. } => "Given up",
        Action::Pause(_) => "⏸ Paused",
        Action::Finish(_) => "✔ Finished",
        Action::Resume(_) => "▶ Continued",
        _ => "✔ Done",
    }
}

/// The window contents of a reminder: background `fill`, a bar in the priority color
/// along the left edge, and `add` inside the margins. Returns the window height (points)
/// that fits the content.
fn reminder_frame(ui: &mut Ui, fill: Color32, bar: Color32, add: impl FnOnce(&mut Ui)) -> f32 {
    let margin = Margin {
        left: (REMINDER_BAR_W + REMINDER_MARGIN_X) as i8,
        right: REMINDER_MARGIN_X as i8,
        top: REMINDER_MARGIN_Y as i8,
        bottom: REMINDER_MARGIN_Y as i8,
    };
    let panel = egui::CentralPanel::default().frame(Frame::new().fill(fill).inner_margin(margin));
    panel
        .show(ui, |ui| {
            let r = ui.clip_rect();
            let bar_rect = egui::Rect::from_min_max(r.min, egui::pos2(r.left() + REMINDER_BAR_W, r.bottom()));
            ui.painter().rect_filled(bar_rect, 0.0, bar);
            // the same space between all lines
            ui.spacing_mut().item_spacing.y = REMINDER_LINE_GAP;
            let content = ui.vertical(add).response.rect;
            (content.bottom() + REMINDER_MARGIN_Y).ceil()
        })
        .inner
}

/// Paints `fill` behind what `add` shows, from one side of the reminder to the other
/// (from the priority bar to the right edge).
fn alarm_band(ui: &mut Ui, fill: Color32, add: impl FnOnce(&mut Ui)) {
    let slot = ui.painter().add(egui::Shape::Noop);
    let r = ui.vertical(add).response.rect;
    // the content area is inset by the margin (on the left: from the bar)
    let x = ui.max_rect().x_range();
    let reach = REMINDER_MARGIN_X;
    let band = egui::Rect::from_x_y_ranges(x.min - reach..=x.max + reach, r.top() - 3.0..=r.bottom() + 3.0);
    ui.painter().with_clip_rect(band).set(slot, egui::Shape::rect_filled(band, 0.0, fill));
}

/// Title row of a reminder: icon and title. A title too long for the window is cut off
/// with "…" (full title on hover) instead of widening the content past the window's
/// right edge. (No ✕ here: reminders that can be dismissed have a Dismiss button.)
fn header(ui: &mut Ui, icon: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon).size(HEADLINE));
        let text = RichText::new(title).size(HEADLINE).strong().color(Color32::WHITE);
        ui.add(egui::Label::new(text).truncate());
    });
}

/// Seconds until a due reminder's give-up button can be clicked, for a reminder that
/// has been open for `open_for`. Rounded up, so the button counts 10, 9, … 1.
fn give_up_wait_secs(open_for: Duration) -> u64 {
    Duration::from_secs(GIVE_UP_WAIT_SECS).saturating_sub(open_for).as_secs_f32().ceil() as u64
}

/// When a task was due, as seen from `today`: "09:00", "yesterday 23:50".
fn due_time(date: NaiveDate, start_min: u32, today: NaiveDate) -> String {
    let at = scheduled_at(date, start_min);
    match (at.date() - today).num_days() {
        0 => at.format("%H:%M").to_string(),
        -1 => at.format("yesterday %H:%M").to_string(),
        _ => at.format("%a %d.%m. %H:%M").to_string(),
    }
}

/// Which reminders should be open on `today` at minute-of-day `nm` (timestamp `now`):
/// - due: a not-yet-started occurrence whose start time has passed (if it has reminders),
/// - ahead: a not-yet-started occurrence starting within its "notify ahead" time,
/// - overrun: a running task that has used up its estimate (if it has reminders).
///
/// Midnight is no border for any of them: the occurrences of yesterday and tomorrow
/// count like today's. A task that was due yesterday and still isn't started keeps its
/// reminder (and a snoozed one comes back), and a task shortly after midnight is
/// announced before it.
///
/// Dismissed reminders and ones snoozed past `now` are left out.
fn wanted_reminders(store: &Store, today: NaiveDate, nm: u32, now: i64) -> Vec<NotifKind> {
    let mut wanted: Vec<NotifKind> = Vec::new();
    let nm = nm as i64;
    for date in [today.pred_opt(), Some(today), today.succ_opt()].into_iter().flatten() {
        for e in store.entries_for(date, now) {
            if e.status != EntryStatus::Virtual {
                continue;
            }
            // minutes after today's midnight: negative for yesterday, 24 h and more for tomorrow
            let start = e.start_on(today);
            if e.notify != NotifyMode::Off && start <= nm {
                wanted.push(NotifKind::Due { task_id: e.task_id, date });
            }
            if e.notify_ahead_min > 0 && nm < start && nm + e.notify_ahead_min as i64 >= start {
                wanted.push(NotifKind::Ahead { task_id: e.task_id, date });
            }
        }
    }
    for r in store.open_runs() {
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
        let next_week = day() + chrono::Duration::days(7);
        assert!(wanted_reminders(&s, next_week, 10 * 60, 9_000).is_empty(), "a one-off task isn't due a week later");
    }

    #[test]
    fn midnight_is_no_border() {
        let (yesterday, tomorrow) = (day().pred_opt().unwrap(), day().succ_opt().unwrap());
        // due at 23:50 and not started: still due after midnight, all of the next day
        let (mut s, id) = store_with(|t| t.start_min = 23 * 60 + 50);
        let due = NotifKind::Due { task_id: id, date: day() };
        assert!(wanted_reminders(&s, day(), 23 * 60 + 49, 0).is_empty());
        assert_eq!(wanted_reminders(&s, day(), 23 * 60 + 50, 0), [due]);
        assert_eq!(wanted_reminders(&s, tomorrow, 0, 0), [due], "it is the same reminder as before midnight");
        assert_eq!(wanted_reminders(&s, tomorrow, 23 * 60 + 59, 0), [due]);
        assert!(wanted_reminders(&s, tomorrow.succ_opt().unwrap(), 0, 0).is_empty(), "not for ever");
        // a snooze that ends after midnight comes back
        s.set_notif(&due.key(), NotifState { dismissed: false, snooze_until: Some(5_000) }).unwrap();
        assert!(wanted_reminders(&s, tomorrow, 5, 4_999).is_empty());
        assert_eq!(wanted_reminders(&s, tomorrow, 5, 5_000), [due]);
        // giving it up (or starting it) on the next day ends it
        s.fail(id, day(), 6_000).unwrap();
        assert!(wanted_reminders(&s, tomorrow, 5, 6_000).is_empty());

        // a task shortly after midnight is announced before midnight
        let (s, id) = store_with(|t| {
            t.start_min = 5;
            t.notify_ahead_min = 10;
        });
        let ahead = NotifKind::Ahead { task_id: id, date: day() };
        assert!(wanted_reminders(&s, yesterday, 23 * 60 + 54, 0).is_empty());
        assert_eq!(wanted_reminders(&s, yesterday, 23 * 60 + 55, 0), [ahead]);
        assert_eq!(wanted_reminders(&s, day(), 4, 0), [ahead]);
        assert_eq!(wanted_reminders(&s, day(), 5, 0), [NotifKind::Due { task_id: id, date: day() }]);

        // a daily task missed yesterday: yesterday's reminder stays next to today's
        let (s, id) = store_with(|t| {
            t.schedule = Schedule::Weekdays { mask: 0b111_1111, from: yesterday, every: WeekRepeat::Weekly }
        });
        let due = |date| NotifKind::Due { task_id: id, date };
        assert_eq!(wanted_reminders(&s, day(), 8 * 60, 0), [due(yesterday)]);
        assert_eq!(wanted_reminders(&s, day(), 9 * 60, 0), [due(yesterday), due(day())]);
    }

    #[test]
    fn give_up_button_waits_ten_seconds() {
        let wait = |ms| give_up_wait_secs(Duration::from_millis(ms));
        assert_eq!(wait(0), 10);
        assert_eq!(wait(1), 10, "counts 10, 9, … 1");
        assert_eq!(wait(1_000), 9);
        assert_eq!(wait(9_001), 1);
        assert_eq!(wait(10_000), 0, "clickable");
        assert_eq!(wait(60_000), 0);
        assert_eq!(due_time(day(), 9 * 60, day()), "09:00");
        assert_eq!(due_time(day().pred_opt().unwrap(), 23 * 60 + 50, day()), "yesterday 23:50");
        assert_eq!(due_time(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(), 9 * 60, day()), "Tue 03.11. 09:00");
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
