use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate, Timelike};
use egui::{
    Align, Color32, CornerRadius, CursorIcon, Frame, Id, Layout, Margin, Pos2, RichText, Sense, Stroke, Ui,
    Vec2, ViewportBuilder, ViewportCommand, ViewportId,
};

use crate::audio::{Audio, PlayRequest};
use crate::dialogs::{self, Editor, EditorResult, ListResult, SettingsDialog, SettingsResult};
use crate::model::*;
use crate::settings::{fmt_snooze, step_scale, Corner, Settings, WindowGeom, WINDOW_SIZE};
use crate::store::{NotifState, Store};
use crate::platform::{instance, Screen, Tray};
use crate::tray::{RunningTask, UiMsg};

pub const GRID_LINE: Color32 = Color32::from_gray(72);
const NOTIF_SIZE: Vec2 = Vec2::new(400.0, 158.0);
const NOTIF_MARGIN: f32 = 14.0;
const NOTIF_GAP: f32 = 10.0;
/// How long after its start time a reminder reaches full red.
const OVERDUE_FULL_RED_MIN: f32 = 60.0;
/// The "starts soon" announcement closes on its own after this long.
const AHEAD_SHOW_SECS: u64 = 10;

pub fn now_ts() -> i64 {
    Local::now().timestamp()
}

pub fn now_min() -> u32 {
    let t = Local::now().time();
    t.hour() * 60 + t.minute()
}

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// Something the user clicked on a task (in the list or in a notification).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start { task_id: i64, date: NaiveDate },
    Pause(i64),
    Finish(i64),
    Resume(i64),
    /// Add tracked time (seconds) for work done before the task was started.
    AddTime(i64, i64),
    /// Remove all added time again.
    ResetAddedTime(i64),
    Edit(i64),
    Stats(i64),
}

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

struct Notif {
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

pub struct TaskerApp {
    pub store: Store,
    pub settings: Settings,
    audio: Audio,
    screen: Option<Screen>,
    rx: Receiver<UiMsg>,
    tray: Option<Tray>,
    tray_online: Arc<AtomicBool>,

    pub selected: NaiveDate,
    pub month: NaiveDate,
    window_visible: bool,
    hide_on_first_frame: bool,
    quitting: bool,

    pub editor: Option<Editor>,
    pub stats_task: Option<i64>,
    settings_dialog: Option<SettingsDialog>,
    /// The task list (recurring and not yet started one-off tasks) is open.
    task_list: bool,
    pub error: Option<String>,

    notifs: Vec<Notif>,
    notif_seq: u64,
    /// Usable screen area in physical pixels, and when it was read.
    work_area: Option<(Instant, egui::Rect)>,
    sound_test_until: Option<Instant>,
    /// Running and paused tasks last sent to the panel icon (menu + icon color).
    tray_running: Vec<RunningTask>,
    /// A window move/resize was handed to the window manager, which swallows the mouse
    /// release; send egui a synthetic release so the next drag isn't ignored.
    release_pointer: bool,
    /// Main window geometry as last seen, as last saved, and when it last changed.
    win_geom: Option<WindowGeom>,
    win_saved: Option<WindowGeom>,
    win_changed: Instant,
    /// Pixels per point the window was last fitted to the screen for (0 = not yet), and a
    /// new value waiting one frame (the window rect lags one frame behind a zoom change).
    win_fit_ppp: f32,
    win_fit_pending: Option<f32>,
}

impl TaskerApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        store: Store,
        settings: Settings,
        window: Option<WindowGeom>,
        listener: Option<instance::Listener>,
        start_hidden: bool,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        ctx.set_visuals(egui::Visuals::dark());
        // Ctrl +/- are handled by us so the zoom ends up in the settings.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        ctx.set_zoom_factor(settings.ui_scale);
        ctx.global_style_mut(|s| {
            s.spacing.item_spacing = egui::vec2(8.0, 6.0);
            s.spacing.button_padding = egui::vec2(10.0, 4.0);
        });

        let (tx, rx) = mpsc::channel();
        let tray_online = Arc::new(AtomicBool::new(false));
        let tray = Tray::spawn(tx.clone(), ctx.clone(), tray_online.clone());

        // Single instance: a second launch asks us to show up.
        if let Some(listener) = listener {
            let ctx = ctx.clone();
            listener.spawn(move || {
                let _ = tx.send(UiMsg::ShowWindow);
                ctx.request_repaint();
            });
        }

        let today = today();
        TaskerApp {
            store,
            settings,
            audio: Audio::new(),
            screen: Screen::connect(),
            rx,
            tray,
            tray_online,
            selected: today,
            month: first_of_month(today),
            window_visible: true,
            hide_on_first_frame: start_hidden,
            quitting: false,
            editor: None,
            stats_task: None,
            settings_dialog: None,
            task_list: false,
            error: None,
            notifs: vec![],
            notif_seq: 0,
            work_area: None,
            sound_test_until: None,
            tray_running: Vec::new(),
            release_pointer: false,
            win_geom: window,
            win_saved: window,
            win_changed: Instant::now(),
            win_fit_ppp: 0.0,
            win_fit_pending: None,
        }
    }

    pub fn report<T>(&mut self, r: rusqlite::Result<T>) {
        if let Err(e) = r {
            crate::logfile::error(format!("database error: {e}"));
            self.error = Some(format!("Database error: {e}"));
        }
    }

    pub fn apply(&mut self, a: Action) {
        let now = now_ts();
        match a {
            Action::Start { task_id, date } => {
                let r = self.store.start(task_id, date, now);
                self.report(r);
            }
            Action::Pause(id) => {
                let r = self.store.pause(id, now);
                self.report(r);
            }
            Action::Finish(id) => {
                let r = self.store.finish(id, now);
                self.report(r);
            }
            Action::Resume(id) => {
                let r = self.store.resume(id, now);
                self.report(r);
            }
            Action::AddTime(id, secs) => {
                let r = self.store.add_time(id, secs);
                self.report(r);
            }
            Action::ResetAddedTime(id) => {
                let r = self.store.reset_added_time(id);
                self.report(r);
            }
            Action::Edit(task_id) => {
                if let Some(t) = self.store.task(task_id) {
                    self.editor = Some(Editor::from_task(t));
                }
            }
            Action::Stats(task_id) => self.stats_task = Some(task_id),
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.window_visible = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn has_tray(&self) -> bool {
        self.tray.is_some() && self.tray_online.load(Ordering::Relaxed)
    }

    /// Minimize to the panel icon (window disappears from the window list).
    /// Without a tray host there would be no way back, so minimize normally instead.
    fn hide_window(&mut self, ctx: &egui::Context) {
        if self.has_tray() {
            self.window_visible = false;
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        } else {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
    }

    /// Apply the UI scale (or the one previewed in the settings) and handle Ctrl +/-/0.
    fn update_zoom(&mut self, ctx: &egui::Context) {
        use egui::gui_zoom::kb_shortcuts as kb;
        let cur = self.settings_dialog.as_ref().map_or(self.settings.ui_scale, |d| d.ui_scale());
        let wanted = ctx.input_mut(|i| {
            if i.consume_shortcut(&kb::ZOOM_RESET) {
                Some(1.0)
            } else if i.consume_shortcut(&kb::ZOOM_IN) || i.consume_shortcut(&kb::ZOOM_IN_SECONDARY) {
                Some(step_scale(cur, true))
            } else if i.consume_shortcut(&kb::ZOOM_OUT) {
                Some(step_scale(cur, false))
            } else {
                None
            }
        });
        if let Some(f) = wanted.filter(|f| (f - cur).abs() > 0.001) {
            match &mut self.settings_dialog {
                Some(d) => d.set_ui_scale(f),
                None => {
                    self.settings.ui_scale = f;
                    let r = self.store.save_settings(&self.settings);
                    self.report(r);
                }
            }
        }
        let want = self.settings_dialog.as_ref().map_or(self.settings.ui_scale, |d| d.ui_scale());
        if (ctx.zoom_factor() - want).abs() > 0.001 {
            ctx.set_zoom_factor(want);
        }
    }

    /// Usable area of every monitor in points, the primary monitor first.
    fn monitor_areas(&self, ctx: &egui::Context) -> Vec<egui::Rect> {
        let ppp = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0) * ctx.zoom_factor();
        let px = self.screen.as_ref().map(|x| x.monitor_areas()).unwrap_or_default();
        if px.is_empty() {
            let size = ctx.input(|i| i.viewport().monitor_size).unwrap_or(egui::vec2(1920.0, 1080.0));
            return vec![egui::Rect::from_min_size(Pos2::ZERO, size)];
        }
        px.into_iter()
            .map(|(x, y, w, h)| egui::Rect::from_min_size(egui::pos2(x as f32, y as f32), egui::vec2(w as f32, h as f32)) / ppp)
            .collect()
    }

    /// Fit the main window to the screen at startup and after a UI scale change: the
    /// minimum size is 1280×800 at 100 % and grows with the UI scale (limited to the
    /// monitor); a window that doesn't fit is shrunk / moved, and one whose title bar is
    /// off-screen (e.g. a monitor was removed) is centered on the primary monitor.
    fn fit_window(&mut self, ctx: &egui::Context) {
        let (outer, maximized, native) = ctx.input(|i| {
            let v = i.viewport();
            (v.outer_rect, v.maximized.unwrap_or(false), v.native_pixels_per_point)
        });
        let (Some(rect), Some(native)) = (outer, native) else { return };
        let ppp = native * ctx.zoom_factor();
        if (ppp - self.win_fit_ppp).abs() < 1e-4 {
            self.win_fit_pending = None;
            return;
        }
        if self.win_fit_pending != Some(ppp) {
            self.win_fit_pending = Some(ppp);
            ctx.request_repaint();
            return;
        }
        let first = self.win_fit_ppp == 0.0;
        self.win_fit_ppp = ppp;
        self.win_fit_pending = None;

        let areas = self.monitor_areas(ctx);
        let overlap = |a: &egui::Rect| {
            let i = a.intersect(rect);
            if i.is_positive() { i.area() } else { 0.0 }
        };
        // The monitor showing most of the window, else the primary one.
        let area = areas
            .iter()
            .copied()
            .filter(|a| overlap(a) > 0.0)
            .max_by(|a, b| overlap(a).total_cmp(&overlap(b)))
            .unwrap_or(areas[0]);
        let min = egui::Vec2::from(WINDOW_SIZE).min(area.size());
        ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(min));
        if first && self.win_geom.is_some_and(|g| g.maximized) && !maximized {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
            return;
        }
        if maximized {
            return;
        }
        let size = rect.size().max(min).min(area.size());
        let title = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 36.0));
        let reachable = areas.iter().any(|a| {
            let i = a.intersect(title);
            i.width() >= 100.0_f32.min(rect.width()) && i.height() >= 10.0
        });
        let resized = (size - rect.size()).length() > 1.0;
        if resized {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        }
        if resized || !reachable {
            let pos = if reachable { rect.min } else { area.center() - size / 2.0 };
            let pos = egui::pos2(
                pos.x.clamp(area.left(), (area.right() - size.x).max(area.left())),
                pos.y.clamp(area.top(), (area.bottom() - size.y).max(area.top())),
            );
            if (pos - rect.min).length() > 1.0 {
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
            }
        }
        if resized || !reachable {
            crate::logfile::info(format!(
                "window fitted to screen: {:.0}x{:.0} at {:.0},{:.0} (points, scale {:.2})",
                size.x, size.y, rect.min.x, rect.min.y, ppp
            ));
        }
    }

    /// Remember the window's position, size and maximized state; saved once it has been
    /// stable for a second (not on every pixel of a drag) and on exit.
    fn track_window(&mut self, ctx: &egui::Context) {
        if self.window_visible && self.win_fit_ppp > 0.0 && self.win_fit_pending.is_none() {
            let (outer, inner, maximized, minimized) = ctx.input(|i| {
                let v = i.viewport();
                (v.outer_rect, v.inner_rect, v.maximized.unwrap_or(false), v.minimized.unwrap_or(false))
            });
            if let (Some(o), Some(i), false) = (outer, inner, minimized) {
                let z = ctx.zoom_factor();
                let mut g = self.win_geom.unwrap_or(WindowGeom { x: 0.0, y: 0.0, w: 0.0, h: 0.0, maximized });
                g.maximized = maximized;
                if !maximized || self.win_geom.is_none() {
                    g.x = (o.min.x * z).round();
                    g.y = (o.min.y * z).round();
                    g.w = (i.width() * z).round();
                    g.h = (i.height() * z).round();
                }
                if Some(g) != self.win_geom {
                    self.win_geom = Some(g);
                    self.win_changed = Instant::now();
                }
            }
        }
        if self.win_geom != self.win_saved && self.win_changed.elapsed() > Duration::from_secs(1) {
            self.save_window();
        }
    }

    fn save_window(&mut self) {
        if let Some(g) = self.win_geom.filter(|g| Some(*g) != self.win_saved) {
            let r = self.store.save_window(&g);
            self.report(r);
            self.win_saved = Some(g);
        }
    }

    /// Stop the settings dialog's sound test, if one is playing. A reminder sound that is
    /// due anyway is restarted by the next notification tick.
    fn stop_sound_test(&mut self) {
        if self.sound_test_until.take().is_some() {
            self.audio.stop();
        }
    }

    // ------------------------------------------------------------------ notifications

    fn tick_notifications(&mut self) {
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
        let want_sound = self
            .notifs
            .iter()
            .any(|n| n.closing_at.is_none() && self.notif_mode(n.kind) == NotifyMode::Sound);
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
                    .map(|(x, y, w, h)| egui::Rect::from_min_size(egui::pos2(x as f32, y as f32), egui::vec2(w as f32, h as f32)))
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
        let y = if top {
            area.top() + NOTIF_MARGIN + step
        } else {
            area.bottom() - NOTIF_MARGIN - NOTIF_SIZE.y - step
        };
        egui::pos2(x, y)
    }

    fn show_notifications(&mut self, ctx: &egui::Context) {
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
            if !n.hidden_from_taskbar && n.frames < 120 {
                if let Some(x) = &self.screen {
                    n.hidden_from_taskbar = x.hide_from_taskbar(&title);
                }
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
        let frame = Frame::new()
            .fill(bg)
            .stroke(Stroke::new(2.0, e.priority.color()))
            .inner_margin(Margin::symmetric(14, 10));
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
                    format!("Due {} · est. {} · {} priority", fmt_hm(e.start_min), fmt_duration_min(e.estimate_min), e.priority.label())
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
    fn announcement_ui(&self, ui: &mut Ui, e: &Entry, date: NaiveDate, expires_in: Option<Duration>) -> Option<NotifAction> {
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
                RichText::new(format!("Starts {when}, at {}", fmt_hm(e.start_min)))
                    .size(15.0)
                    .color(Color32::WHITE),
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

    // ------------------------------------------------------------------ window chrome

    /// Window edges: our window has no system decorations, so we provide resizing.
    fn handle_resize(&mut self, ctx: &egui::Context) -> bool {
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            return false;
        }
        let rect = ctx.viewport_rect();
        let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return false };
        let m = 6.0;
        let (l, r, t, b) =
            (p.x < rect.left() + m, p.x > rect.right() - m, p.y < rect.top() + m, p.y > rect.bottom() - m);
        use egui::ResizeDirection as D;
        let (dir, icon) = match (l, r, t, b) {
            (true, _, true, _) => (D::NorthWest, CursorIcon::ResizeNorthWest),
            (_, true, true, _) => (D::NorthEast, CursorIcon::ResizeNorthEast),
            (true, _, _, true) => (D::SouthWest, CursorIcon::ResizeSouthWest),
            (_, true, _, true) => (D::SouthEast, CursorIcon::ResizeSouthEast),
            (true, ..) => (D::West, CursorIcon::ResizeWest),
            (_, true, ..) => (D::East, CursorIcon::ResizeEast),
            (_, _, true, _) => (D::North, CursorIcon::ResizeNorth),
            (_, _, _, true) => (D::South, CursorIcon::ResizeSouth),
            _ => return false,
        };
        ctx.set_cursor_icon(icon);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
            self.release_pointer = true;
            return true;
        }
        false
    }

    fn title_bar(&mut self, ui: &mut Ui, resizing: bool) {
        let rect = ui.max_rect();
        let drag = ui.interact(rect, Id::unique("titlebar"), Sense::click_and_drag());
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        if drag.double_clicked() {
            ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        } else if drag.drag_started_by(egui::PointerButton::Primary) && !resizing {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
            self.release_pointer = true;
        }
        ui.horizontal_centered(|ui| {
            ui.add_space(6.0);
            let (r, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), Sense::hover());
            let tex = ui.ctx().data_mut(|d| d.get_temp::<egui::TextureHandle>(Id::unique("app-icon")));
            let tex = tex.unwrap_or_else(|| {
                let img = egui::ColorImage::from_rgba_unmultiplied([40, 40], &crate::icon::render(40, crate::icon::IconState::Idle));
                let t = ui.ctx().load_texture("app-icon", img, Default::default());
                ui.ctx().data_mut(|d| d.insert_temp(Id::unique("app-icon"), t.clone()));
                t
            });
            ui.painter().image(
                tex.id(),
                r,
                egui::Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            ui.label(RichText::new("Tasker").strong().size(15.0));
            ui.label(RichText::new(Local::now().format("%A, %d %B %Y  %H:%M").to_string()).weak());

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let btn = |ui: &mut Ui, s: &str, tip: &str| {
                    ui.add(egui::Button::new(RichText::new(s).size(15.0)).frame(false).min_size(egui::vec2(34.0, 26.0)))
                        .on_hover_text(tip)
                };
                if btn(ui, "❌", if self.settings.close_to_tray { "Close to tray" } else { "Quit" }).clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                }
                if btn(ui, if maximized { "🗗" } else { "🗖" }, "Maximize").clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }
                if btn(ui, "🗕", "Minimize to tray").clicked() {
                    let ctx = ui.ctx().clone();
                    self.hide_window(&ctx);
                }
                ui.add_space(8.0);
                if btn(ui, "⚙", "Settings").clicked() {
                    self.settings_dialog = Some(SettingsDialog::new(self.settings.clone()));
                }
                if btn(ui, "☰", "Task list").clicked() {
                    self.task_list = true;
                }
                if let Some(err) = &self.error {
                    let r = ui.label(RichText::new(format!("⚠ {err}")).color(Color32::from_rgb(255, 120, 100)));
                    if r.on_hover_text("Click to dismiss").clicked() {
                        self.error = None;
                    }
                }
            });
        });
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

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// The state-dependent buttons of a task, shared by the task list and the notifications.
pub fn task_buttons(ui: &mut Ui, e: &Entry) -> Option<Action> {
    let mut out = None;
    let green = Color32::from_rgb(46, 125, 70);
    let mut b = |ui: &mut Ui, text: &str, fill: Option<Color32>, a: Action| {
        let mut btn = egui::Button::new(RichText::new(text).color(Color32::WHITE)).corner_radius(CornerRadius::same(4));
        if let Some(f) = fill {
            btn = btn.fill(f);
        }
        if ui.add(btn).clicked() {
            out = Some(a);
        }
    };
    match (e.status, e.run_id) {
        (EntryStatus::Virtual, _) => b(ui, "▶ Start task", Some(green), Action::Start { task_id: e.task_id, date: e.date }),
        (EntryStatus::Running, Some(id)) => {
            b(ui, "⏸ Pause", None, Action::Pause(id));
            b(ui, "✔ Finish", Some(green), Action::Finish(id));
        }
        (EntryStatus::Paused, Some(id)) => {
            b(ui, "▶ Continue", Some(green), Action::Resume(id));
            b(ui, "✔ Finish", None, Action::Finish(id));
        }
        (EntryStatus::Done, Some(id)) => b(ui, "⟲ Resume", None, Action::Resume(id)),
        _ => {}
    }
    out
}

/// Next to the task buttons: how far the scheduled start is away ("starts in 25 min",
/// "12 min overdue") or how punctually it was started, plus when a snoozed reminder returns.
pub fn start_offset_labels(ui: &mut Ui, e: &Entry, now: i64, snoozed_until: Option<i64>, on_red: bool) {
    let gray = if on_red { Color32::from_gray(225) } else { Color32::from_gray(170) };
    let warn = if on_red { Color32::WHITE } else { Color32::from_rgb(235, 100, 90) };
    let (text, color) = match (e.status, e.first_start) {
        (EntryStatus::Virtual, _) => {
            let m = minutes_from_start(e.date, e.start_min, now);
            match m {
                ..0 => (format!("starts in {}", fmt_span_min(-m)), gray),
                0 => ("starts now".to_string(), Color32::from_rgb(235, 200, 90)),
                _ => (format!("{} overdue", fmt_span_min(m)), warn),
            }
        }
        (_, Some(t)) => {
            let m = minutes_from_start(e.date, e.start_min, t);
            match m {
                ..0 => (format!("started {} early", fmt_span_min(-m)), gray),
                0 => ("started on time".to_string(), gray),
                _ => (format!("started {} late", fmt_span_min(m)), gray),
            }
        }
        _ => return,
    };
    ui.label(RichText::new(text).color(color));
    if let Some(until) = snoozed_until.filter(|&t| t > now) {
        let at = chrono::DateTime::from_timestamp(until, 0).map(|t| t.with_timezone(&Local).format("%H:%M").to_string());
        let mins = (until - now + 59) / 60;
        ui.label(
            RichText::new(format!("⏰ snoozed, reminds again {} (in {})", at.unwrap_or_default(), fmt_span_min(mins)))
                .color(Color32::from_rgb(200, 180, 120)),
        )
        .on_hover_text("The reminder was snoozed");
    }
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

impl eframe::App for TaskerApp {
    fn on_exit(&mut self) {
        self.save_window();
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if std::mem::take(&mut self.release_pointer) {
            let pos = ctx.input(|i| i.pointer.latest_pos()).unwrap_or_default();
            raw_input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: ctx.input(|i| i.modifiers),
            });
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.hide_on_first_frame {
            self.hide_on_first_frame = false;
            self.hide_window(ctx);
        }
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                UiMsg::ToggleWindow if self.window_visible => self.hide_window(ctx),
                UiMsg::ToggleWindow | UiMsg::ShowWindow => self.show_window(ctx),
                UiMsg::Task(a) => {
                    // The menu may be stale: only act if the task is still in a matching state.
                    let ok = match a {
                        Action::Pause(id) => self.store.run(id).is_some_and(|r| r.status == RunStatus::Running),
                        Action::Resume(id) => self.store.run(id).is_some_and(|r| r.status == RunStatus::Paused),
                        Action::Finish(id) => self.store.run(id).is_some_and(|r| r.status != RunStatus::Done),
                        _ => false,
                    };
                    if ok {
                        crate::logfile::info(format!("panel menu: {a:?}"));
                        self.apply(a);
                    }
                }
                UiMsg::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
        let (close_requested, minimized) =
            ctx.input(|i| (i.viewport().close_requested(), i.viewport().minimized.unwrap_or(false)));
        if close_requested && self.settings.close_to_tray && !self.quitting && self.has_tray() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide_window(ctx);
        }
        // Minimized through the window manager (keyboard shortcut etc.): go to the tray instead.
        if minimized && self.window_visible && self.has_tray() {
            self.hide_window(ctx);
        }

        self.update_zoom(ctx);
        self.fit_window(ctx);
        self.track_window(ctx);
        self.tick_notifications();
        if !self.notifs.is_empty() && !self.window_visible {
            // The root window is hidden; ask eframe to still run `ui` so the
            // notification viewports get created and painted.
            ctx.send_viewport_cmd(ViewportCommand::RequestPaintWhileHidden);
        }

        let running: Vec<RunningTask> = self
            .store
            .runs
            .iter()
            .filter(|r| r.status != RunStatus::Done)
            .map(|r| RunningTask { run_id: r.id, title: r.title.clone(), paused: r.status == RunStatus::Paused })
            .collect();
        if running != self.tray_running {
            self.tray_running = running.clone();
            if let Some(t) = &self.tray {
                t.set_running(running);
            }
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let resizing = self.handle_resize(&ctx);

        let bar_frame = Frame::new().fill(Color32::from_gray(24)).inner_margin(Margin::symmetric(4, 2));
        egui::Panel::top("titlebar").exact_size(36.0).frame(bar_frame).show(ui, |ui| self.title_bar(ui, resizing));

        let side_frame = Frame::new().fill(Color32::from_gray(30)).inner_margin(Margin::same(10));
        egui::Panel::left("left").exact_size(320.0).resizable(false).frame(side_frame).show(ui, |ui| {
            crate::views::left_column(self, ui);
        });
        egui::CentralPanel::default()
            .frame(Frame::new().fill(Color32::from_gray(36)))
            .show(ui, |ui| crate::views::day_view(self, ui));

        // Thin outline, since there are no system decorations.
        ui.painter().rect_stroke(ctx.viewport_rect(), 0.0, Stroke::new(1.0, Color32::from_gray(60)), egui::StrokeKind::Inside);

        // ---- dialogs (the task list first: its editor opens on top of it)
        if self.task_list {
            match dialogs::task_list_ui(&ctx, &self.store) {
                ListResult::Open => {}
                ListResult::Close => self.task_list = false,
                ListResult::Edit(id) => self.apply(Action::Edit(id)),
            }
        }
        if let Some(ed) = &mut self.editor {
            match dialogs::editor_ui(&ctx, ed) {
                EditorResult::Open => {}
                EditorResult::Cancel => self.editor = None,
                EditorResult::Save(task) => {
                    let r = self.store.save_task(task);
                    self.report(r);
                    self.editor = None;
                }
                EditorResult::Delete(id) => {
                    let r = self.store.delete_task(id);
                    self.report(r);
                    self.editor = None;
                }
            }
        }
        if let Some(task_id) = self.stats_task {
            if !dialogs::stats_ui(&ctx, &self.store, task_id) {
                self.stats_task = None;
            }
        }
        if let Some(dlg) = &mut self.settings_dialog {
            let res = dialogs::settings_ui(&ctx, dlg, &self.audio);
            match res {
                SettingsResult::Open => {}
                SettingsResult::TestSound(s) => {
                    self.audio.play(PlayRequest::from_settings(&s, Some(Duration::from_secs_f32(s.ramp_secs.clamp(3.0, 20.0)))));
                    self.sound_test_until = Some(Instant::now() + Duration::from_secs_f32(s.ramp_secs.clamp(3.0, 20.0)));
                }
                SettingsResult::StopSound => self.stop_sound_test(),
                SettingsResult::Cancel => {
                    self.settings_dialog = None;
                    self.stop_sound_test();
                }
                SettingsResult::Save(s) => {
                    let r = self.store.save_settings(&s);
                    self.report(r);
                    self.settings = s;
                    self.settings_dialog = None;
                    self.work_area = None;
                    // Stop a sound test, or restart a playing reminder sound with the new
                    // settings (the next tick starts it again if a reminder wants sound).
                    self.sound_test_until = None;
                    self.audio.stop();
                }
            }
        }

        self.show_notifications(&ctx);
    }
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
        assert!(
            wanted_reminders(&s, tomorrow, 10 * 60, 9_000).is_empty(),
            "a one-off task isn't due on other days"
        );
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

    #[test]
    fn color_blend() {
        let (a, b) = (Color32::from_rgb(0, 100, 200), Color32::from_rgb(200, 100, 0));
        assert_eq!(lerp_color(a, b, 0.0), a);
        assert_eq!(lerp_color(a, b, 1.0), b);
        assert_eq!(lerp_color(a, b, 0.5), Color32::from_rgb(100, 100, 100));
    }
}
