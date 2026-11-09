//! The application: its state, what a click on a task does, and the frame loop. The
//! main window's chrome and geometry are in [`window`], the reminders in [`reminders`].

mod reminders;
mod window;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate, Timelike};
use egui::{Color32, Frame, Margin, Stroke, Ui, ViewportCommand};

use crate::action::Action;
use crate::audio::{Audio, PlayRequest};
use crate::dialogs::{self, Editor, EditorResult, ListResult, SettingsDialog, SettingsResult};
use crate::model::*;
use crate::platform::{Screen, Tray, instance};
use crate::settings::{Settings, WindowGeom};
use crate::store::Store;
use crate::tray::{RunningTask, UiMsg};

use reminders::Notif;
pub use reminders::snoozed_until;

pub const GRID_LINE: Color32 = Color32::from_gray(72);

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

    /// Stop the settings dialog's sound test, if one is playing. A reminder sound that is
    /// due anyway is restarted by the next notification tick.
    fn stop_sound_test(&mut self) {
        if self.sound_test_until.take().is_some() {
            self.audio.stop();
        }
    }
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
        ui.painter().rect_stroke(
            ctx.viewport_rect(),
            0.0,
            Stroke::new(1.0, Color32::from_gray(60)),
            egui::StrokeKind::Inside,
        );

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
        if let Some(task_id) = self.stats_task
            && !dialogs::stats_ui(&ctx, &self.store, task_id)
        {
            self.stats_task = None;
        }
        if let Some(dlg) = &mut self.settings_dialog {
            let res = dialogs::settings_ui(&ctx, dlg, &self.audio);
            match res {
                SettingsResult::Open => {}
                SettingsResult::TestSound(s) => {
                    self.audio.play(PlayRequest::from_settings(
                        &s,
                        Some(Duration::from_secs_f32(s.ramp_secs.clamp(3.0, 20.0))),
                    ));
                    self.sound_test_until =
                        Some(Instant::now() + Duration::from_secs_f32(s.ramp_secs.clamp(3.0, 20.0)));
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
