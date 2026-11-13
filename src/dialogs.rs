//! Modal dialogs: task editor, statistics overlay, app settings.

use std::sync::{Arc, Mutex};

use chrono::{Duration, Local, NaiveDate, Timelike};
use egui::{Align2, Color32, FontId, Id, Modal, Rect, RichText, Sense, Stroke, TextStyle, Ui};

use crate::app::GRID_LINE;
use crate::audio::Audio;
use crate::model::*;
use crate::platform;
use crate::settings::{
    Corner, DEFAULT_START_PRESETS, DEFAULT_TIME_PRESETS, MAX_DURATION_MIN, Settings, SoundChoice, UI_SCALE_PCT,
};
use crate::store::Store;
use crate::views::{month_grid, month_header};
use crate::widgets::{
    ACTION_PAD, ACTION_PAD_SYM, ACTION_TEXT, Act, FAILED_RED, HEADLINE, RADIO_GAP, TEXT, action_button, action_height,
    choice_button_ext, preset_button, preset_row, radio, radio_row, tab_bar,
};

/// Dragging a number field changes it by one step per this many points of mouse travel.
pub const DRAG_POINTS_PER_STEP: f64 = 8.0;

// ============================================================================ editor

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Date,
    Weekdays,
    MonthDays,
}

pub struct Editor {
    task_id: Option<i64>,
    created_at: i64,
    title: String,
    notes: String,
    mode: Mode,
    date: NaiveDate,
    /// Month shown by the date calendar.
    date_month: NaiveDate,
    repeat: Repeat,
    wd_mask: u8,
    wd_every: WeekRepeat,
    md_mask: u32,
    /// The start date as picked; the schedule begins at the first matching day on or after it.
    from: NaiveDate,
    /// Month shown by the "Starting" calendar.
    from_month: NaiveDate,
    /// Effective start in the previous frame (to follow it when it jumps to another month).
    last_from: NaiveDate,
    start_h: u32,
    start_m: u32,
    est_h: u32,
    est_m: u32,
    priority: Priority,
    notify: NotifyMode,
    /// Minutes before the start for the announcement (0 = off).
    notify_ahead: u32,
    error: Option<String>,
    confirm_delete: bool,
    focus_title: bool,
    /// Tallest the schedule column has been, so switching modes doesn't make the dialog jump.
    schedule_h: f32,
}

pub enum EditorResult {
    Open,
    Cancel,
    Save(Task),
    Delete(i64),
}

impl Editor {
    pub fn new(date: NaiveDate) -> Self {
        let now = Local::now();
        let start_h = if date == now.date_naive() { (now.hour() + 1).min(23) } else { 9 };
        Editor {
            task_id: None,
            created_at: now.timestamp(),
            title: String::new(),
            notes: String::new(),
            mode: Mode::Date,
            date,
            date_month: first_of_month(date),
            repeat: Repeat::Never,
            wd_mask: 0,
            wd_every: WeekRepeat::Weekly,
            md_mask: 0,
            from: date,
            from_month: first_of_month(date),
            last_from: date,
            start_h,
            start_m: 0,
            est_h: 0,
            est_m: 30,
            priority: Priority::Normal,
            notify: NotifyMode::Sound,
            notify_ahead: 0,
            error: None,
            confirm_delete: false,
            focus_title: true,
            schedule_h: 0.0,
        }
    }

    pub fn from_task(t: &Task) -> Self {
        let first = t.schedule.first_date();
        let mut ed = Editor::new(first);
        ed.task_id = Some(t.id);
        ed.created_at = t.created_at;
        ed.title = t.title.clone();
        ed.notes = t.notes.clone();
        ed.start_h = t.start_min / 60;
        ed.start_m = t.start_min % 60;
        ed.est_h = t.estimate_min / 60;
        ed.est_m = t.estimate_min % 60;
        ed.priority = t.priority;
        ed.notify = t.notify;
        ed.notify_ahead = t.notify_ahead_min;
        ed.focus_title = false;
        match t.schedule.clone().normalized() {
            Schedule::Date { date, repeat } => {
                ed.mode = Mode::Date;
                ed.date = date;
                ed.repeat = repeat;
            }
            Schedule::Weekdays { mask, from, every } => {
                ed.mode = Mode::Weekdays;
                ed.wd_mask = mask;
                ed.wd_every = every;
                ed.from = from;
            }
            Schedule::MonthDays { mask, from } => {
                ed.mode = Mode::MonthDays;
                ed.md_mask = mask;
                ed.from = from;
            }
        }
        ed
    }

    /// The recurring schedule as it would be if it started on `from` (None without days selected).
    fn recurring_schedule(&self, from: NaiveDate) -> Option<Schedule> {
        match self.mode {
            Mode::Weekdays if self.wd_mask != 0 => {
                Some(Schedule::Weekdays { mask: self.wd_mask, from, every: self.wd_every })
            }
            Mode::MonthDays if self.md_mask != 0 => Some(Schedule::MonthDays { mask: self.md_mask, from }),
            _ => None,
        }
    }

    /// Whether the recurring schedule could start on `d` (it would be an occurrence).
    fn can_start_on(&self, d: NaiveDate) -> bool {
        self.recurring_schedule(d).is_none_or(|s| s.occurs_on(d))
    }

    /// First day on or after the picked start date that matches the selected days.
    fn effective_from(&self) -> NaiveDate {
        let mut d = self.from;
        for _ in 0..800 {
            if self.can_start_on(d) {
                return d;
            }
            d += Duration::days(1);
        }
        self.from
    }

    fn to_task(&self) -> Result<Task, String> {
        if self.title.trim().is_empty() {
            return Err("Please enter a title.".into());
        }
        let schedule = match self.mode {
            Mode::Date => Schedule::Date { date: self.date, repeat: self.repeat },
            Mode::Weekdays if self.wd_mask == 0 => return Err("Select at least one weekday.".into()),
            Mode::MonthDays if self.md_mask == 0 => return Err("Select at least one day of the month.".into()),
            _ => self.recurring_schedule(self.effective_from()).unwrap(),
        };
        Ok(Task {
            id: self.task_id.unwrap_or(0),
            title: self.title.trim().to_string(),
            notes: self.notes.clone(),
            schedule,
            start_min: self.start_h * 60 + self.start_m,
            estimate_min: (self.est_h * 60 + self.est_m).min(MAX_ESTIMATE_MIN),
            priority: self.priority,
            notify: self.notify,
            notify_ahead_min: self.notify_ahead,
            created_at: self.created_at,
            deleted: false,
        })
    }
}

fn two_digit(v: f64, _: std::ops::RangeInclusive<usize>) -> String {
    format!("{:02}", v as i64)
}

/// Text size of every control in the editor (number and text fields too): the action
/// button size, so all of them have the same height.
const CTRL_SIZE: f32 = ACTION_TEXT;
/// Inner padding of the controls (the same for buttons and text fields).
const CTRL_PAD: egui::Margin = ACTION_PAD;

/// Height of one control: a line of control text plus the padding.
fn ctrl_h(ui: &Ui) -> f32 {
    action_height(ui)
}

/// A line of preset buttons below a field.
fn presets(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(2.0);
    preset_row(ui, add);
}

fn chip(ui: &mut Ui, on: bool, text: impl Into<String>) -> bool {
    preset_button(ui, on, text).clicked()
}

/// Always-open month calendar.
fn calendar(
    ui: &mut Ui,
    salt: &str,
    month: &mut NaiveDate,
    value: &mut NaiveDate,
    enabled: Option<&dyn Fn(NaiveDate) -> bool>,
) {
    ui.vertical(|ui| {
        ui.set_width(CAL_W);
        let mut sel = *value;
        month_header(ui, month, &mut sel, false);
        month_grid(ui, salt, *month, &mut sel, None, enabled, 30.0);
        if sel != *value {
            *value = sel;
            if first_of_month(sel) != *month {
                *month = first_of_month(sel);
            }
        }
    });
}

const CAL_W: f32 = 380.0;
const DAY_GAP: f32 = 4.0;
/// Width of one day button, so a row of 7 is exactly as wide as the calendar.
const DAY_BTN_W: f32 = (CAL_W - 6.0 * DAY_GAP) / 7.0;
/// Width of the text fields in the editor's left column.
const LEFT_FIELD_W: f32 = 440.0;

/// The frame of the dialogs: a popup with 15 px between its border and the content.
fn dialog_frame(ctx: &egui::Context) -> egui::Frame {
    egui::Frame::popup(&ctx.global_style()).inner_margin(15)
}

/// Space between the sections of a dialog.
const SECTION_GAP: f32 = 15.0;
/// Space between a section's title and its controls.
const SECTION_TITLE_GAP: f32 = 10.0;

/// The sections of a dialog (or of a column of one): each one's title above its controls,
/// followed by a divider to the right edge.
#[derive(Default)]
struct Sections {
    /// The titles; their dividers are drawn once the width of the dialog is known (`finish`).
    titles: Vec<Rect>,
}

impl Sections {
    fn add(&mut self, ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
        if !self.titles.is_empty() {
            ui.add_space(SECTION_GAP - ui.spacing().item_spacing.y);
        }
        self.titles.push(ui.label(label).rect);
        ui.add_space(SECTION_TITLE_GAP - ui.spacing().item_spacing.y);
        ui.scope(add);
    }

    fn finish(self, ui: &mut Ui) {
        let right = ui.min_rect().right();
        let gap = ui.spacing().item_spacing.x;
        for t in self.titles {
            ui.painter().hline(t.right() + gap..=right, t.center().y.round() + 0.5, Stroke::new(1.0, GRID_LINE));
        }
    }
}

/// The longest "notify ahead" time: 23 h 59 min.
const MAX_NOTIFY_AHEAD_MIN: u32 = 23 * 60 + 59;

/// Width of the hour and minute fields.
const TIME_FIELD_W: f32 = 70.0;
/// Width of the other number fields (the values of the sliders).
const VALUE_FIELD_W: f32 = 130.0;

/// Adds a number field for `value` within `range`, `width` wide, its value centered (while
/// typing too), looking like an input field ([`input_style`]). It changes by `step` per
/// `DRAG_POINTS_PER_STEP` points of dragging and per press of ↑ / ↓ while it has the
/// focus. `look` adds the rest (formatting, suffix, …).
fn number_field<N: egui::emath::Numeric>(
    ui: &mut Ui,
    value: &mut N,
    range: std::ops::RangeInclusive<N>,
    step: f64,
    width: f32,
    look: impl FnOnce(egui::DragValue<'_>) -> egui::DragValue<'_>,
) -> egui::Response {
    let size = egui::vec2(width, ctrl_h(ui));
    // A number field being typed into aligns its text like the layout it is in.
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Center), |ui| {
        input_style(ui);
        // egui changes a focused field by its drag speed per ↑ / ↓, which rounds away to
        // nothing for whole numbers: handle them here (before the field sees them).
        let id = ui.next_auto_id(); // the field's
        let mut arrows = 0.0;
        if ui.memory(|m| m.has_focus(id)) {
            use egui::{Key, Modifiers};
            arrows = ui.input_mut(|i| {
                i.count_and_consume_key(Modifiers::NONE, Key::ArrowUp) as f64
                    - i.count_and_consume_key(Modifiers::NONE, Key::ArrowDown) as f64
            });
            if arrows != 0.0 {
                let (lo, hi) = (range.start().to_f64(), range.end().to_f64());
                *value = N::from_f64((value.to_f64() + arrows * step).clamp(lo, hi));
            }
        }
        let field = egui::DragValue::new(value).range(range).speed(step / DRAG_POINTS_PER_STEP);
        let mut r = ui.add(look(field).min_size(size));
        if arrows != 0.0 {
            r.mark_changed();
        }
        r
    })
    .inner
}

/// What a pair of hour and minute fields stands for.
#[derive(Clone, Copy, PartialEq)]
enum TimeKind {
    /// A time of day: "[hh] : [mm]".
    OfDay,
    /// A length of time: "[h] h [mm] min".
    Span,
}

/// Hours (up to `max_h`) and minutes as two number fields, for a time of day or a length
/// of time (`kind`).
fn hm_fields(ui: &mut Ui, h: &mut u32, m: &mut u32, max_h: u32, kind: TimeKind) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let a = number_field(ui, h, 0..=max_h, 1.0, TIME_FIELD_W, |f| {
            if kind == TimeKind::OfDay { f.custom_formatter(two_digit) } else { f }
        });
        ui.label(if kind == TimeKind::OfDay { ":" } else { "h" });
        let b = number_field(ui, m, 0..=59, 1.0, TIME_FIELD_W, |f| f.custom_formatter(two_digit));
        if kind == TimeKind::Span {
            ui.label("min");
        }
        a | b
    })
    .inner
}

/// [`hm_fields`] for a value in minutes.
fn minutes_fields(ui: &mut Ui, v: &mut u32, max_h: u32, kind: TimeKind) -> egui::Response {
    let (mut h, mut m) = (*v / 60, *v % 60);
    let r = hm_fields(ui, &mut h, &mut m, max_h, kind);
    *v = h * 60 + m;
    r
}

/// Width of the settings dialog's content.
const SETTINGS_W: f32 = 550.0;

/// Width of every slider in the dialogs.
const SLIDER_W: f32 = 300.0;

/// A slider for 0–100 % with its value in a number field next to it, then `label`.
fn percent_slider(ui: &mut Ui, v: &mut f32, label: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = SLIDER_W;
        ui.add(egui::Slider::new(&mut *v, 0.0..=1.0).show_value(false));
        number_field(ui, v, 0.0..=1.0, 0.01, VALUE_FIELD_W, |f| {
            f.custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                .custom_parser(|t| t.trim().trim_end_matches('%').trim().parse::<f64>().ok().map(|p| p / 100.0))
        });
        ui.label(label);
    });
}

/// Makes the input fields added to `ui` look like ones: the dark background of text
/// fields and a thin border (in the divider grey, lighter on hover, highlighted while in use).
fn input_style(ui: &mut Ui) {
    let v = ui.visuals_mut();
    let bg = v.extreme_bg_color;
    let in_use = v.selection.stroke.color;
    let w = &mut v.widgets;
    for (state, border) in
        [(&mut w.inactive, GRID_LINE), (&mut w.hovered, Color32::from_gray(110)), (&mut w.active, in_use)]
    {
        state.weak_bg_fill = bg;
        state.bg_fill = bg;
        state.bg_stroke = Stroke::new(1.0, border);
        state.expansion = 0.0;
    }
}

fn details_column(ui: &mut Ui, ed: &mut Editor, settings: &Settings) {
    let field_w = LEFT_FIELD_W;
    let mut sec = Sections::default();
    sec.add(ui, "Title", |ui| {
        input_style(ui);
        let r = ui.add(
            egui::TextEdit::singleline(&mut ed.title)
                .desired_width(field_w)
                .margin(CTRL_PAD)
                .hint_text("What needs doing?"),
        );
        if ed.focus_title {
            r.request_focus();
            ed.focus_title = false;
        }
    });

    sec.add(ui, "Notes", |ui| {
        input_style(ui);
        ui.add(egui::TextEdit::multiline(&mut ed.notes).desired_rows(3).desired_width(field_w).margin(CTRL_PAD));
    });

    sec.add(ui, "Start time", |ui| {
        hm_fields(ui, &mut ed.start_h, &mut ed.start_m, 23, TimeKind::OfDay);
        presets(ui, |ui| {
            for &t in &settings.start_presets {
                if chip(ui, ed.start_h * 60 + ed.start_m == t, fmt_hm(t)) {
                    ed.start_h = t / 60;
                    ed.start_m = t % 60;
                }
            }
        });
    });

    sec.add(ui, "Estimate", |ui| {
        hm_fields(ui, &mut ed.est_h, &mut ed.est_m, MAX_ESTIMATE_MIN / 60, TimeKind::Span);
        presets(ui, |ui| {
            for &m in &settings.time_presets {
                if chip(ui, ed.est_h * 60 + ed.est_m == m, fmt_duration_min(m)) {
                    ed.est_h = m / 60;
                    ed.est_m = m % 60;
                }
            }
        });
    });

    sec.add(ui, "Priority", |ui| {
        radio_row(ui, |ui| {
            for p in Priority::ALL {
                if radio(ui, ed.priority == p, RichText::new(p.label()).color(p.color())).clicked() {
                    ed.priority = p;
                }
            }
        });
    });

    sec.add(ui, "Reminder", |ui| {
        radio_row(ui, |ui| {
            for n in NotifyMode::ALL {
                if radio(ui, ed.notify == n, format!("{} {}", n.icon(), n.label())).clicked() {
                    ed.notify = n;
                }
            }
        });
    });

    sec.add(ui, "Notify ahead", |ui| {
        ui.horizontal(|ui| {
            minutes_fields(ui, &mut ed.notify_ahead, MAX_NOTIFY_AHEAD_MIN / 60, TimeKind::Span)
                .on_hover_text("A short silent notice this long before the start; closes after 10 s");
            let note = if ed.notify_ahead > 0 { "before the start" } else { "(off)" };
            ui.label(RichText::new(note).weak());
        });
        presets(ui, |ui| {
            // "Off" always comes first; the others are set in the settings
            for m in std::iter::once(0).chain(settings.time_presets.iter().copied()) {
                let label = if m == 0 { "Off".to_string() } else { fmt_duration_min(m) };
                // "24 h" is as far ahead as it goes: 23:59
                let m = m.min(MAX_NOTIFY_AHEAD_MIN);
                if chip(ui, ed.notify_ahead == m, label) {
                    ed.notify_ahead = m;
                }
            }
        });
    });
    sec.finish(ui);
}

fn schedule_column(ui: &mut Ui, ed: &mut Editor) {
    // Follow the effective start date when changing the days moves it to another month.
    let eff = ed.effective_from();
    if eff != ed.last_from {
        ed.last_from = eff;
        ed.from_month = first_of_month(eff);
    }
    let mut sec = Sections::default();
    // what the task is active on: tabs, since this changes the whole rest of the column
    let tabs = [(Mode::Date, "On a date"), (Mode::Weekdays, "Weekdays"), (Mode::MonthDays, "Month days")];
    tab_bar(ui, &mut ed.mode, &tabs, CAL_W);
    ui.add_space(SECTION_GAP - ui.spacing().item_spacing.y);

    match ed.mode {
        Mode::Date => {
            sec.add(ui, "Date", |ui| calendar(ui, "date-cal", &mut ed.date_month, &mut ed.date, None));
            sec.add(ui, "Repeat", |ui| {
                radio_row(ui, |ui| {
                    for r in Repeat::CHOICES {
                        if radio(ui, ed.repeat == r, r.label()).clicked() {
                            ed.repeat = r;
                        }
                    }
                });
            });
        }
        Mode::Weekdays => {
            sec.add(ui, "Days", |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = DAY_GAP;
                    ui.spacing_mut().button_padding.x = 2.0;
                    for (i, name) in WEEKDAY_SHORT.iter().enumerate() {
                        let on = ed.wd_mask & (1 << i) != 0;
                        if choice_button_ext(ui, on, *name, Color32::WHITE, egui::vec2(DAY_BTN_W, 0.0)).clicked() {
                            ed.wd_mask ^= 1 << i;
                        }
                    }
                });
                presets(ui, |ui| {
                    for (label, mask) in [("Mon–Fri", 0b001_1111), ("Weekend", 0b110_0000), ("Every day", 0b111_1111)]
                    {
                        if chip(ui, ed.wd_mask == mask, label) {
                            ed.wd_mask = mask;
                        }
                    }
                    if chip(ui, false, "Clear") {
                        ed.wd_mask = 0;
                    }
                });
            });
        }
        Mode::MonthDays => {
            sec.add(ui, "Days", |ui| {
                let h = ctrl_h(ui);
                egui::Grid::new("md-grid").spacing([DAY_GAP, DAY_GAP]).show(ui, |ui| {
                    ui.spacing_mut().button_padding.x = 2.0;
                    for d in 1..=31u32 {
                        let on = ed.md_mask & (1 << d) != 0;
                        if choice_button_ext(ui, on, d.to_string(), Color32::WHITE, egui::vec2(DAY_BTN_W, h)).clicked()
                        {
                            ed.md_mask ^= 1 << d;
                        }
                        if d % 7 == 0 {
                            ui.end_row();
                        }
                    }
                });
                presets(ui, |ui| {
                    for (label, mask) in [("1st", 1u32 << 1), ("1st + 15th", (1 << 1) | (1 << 15))] {
                        if chip(ui, ed.md_mask == mask, label) {
                            ed.md_mask = mask;
                        }
                    }
                    if chip(ui, false, "Clear") {
                        ed.md_mask = 0;
                    }
                });
            });
        }
    }

    if ed.mode != Mode::Date {
        sec.add(ui, "Starting", |ui| {
            let mut shown = eff;
            let mut month = ed.from_month;
            let check = |d: NaiveDate| ed.can_start_on(d);
            calendar(ui, "from-cal", &mut month, &mut shown, Some(&check));
            ed.from_month = month;
            if shown != eff {
                ed.from = shown;
                ed.last_from = shown;
            }
        });
    }
    // Repeat comes after the calendar, like in the date view.
    match ed.mode {
        Mode::Date => {}
        Mode::Weekdays => {
            sec.add(ui, "Repeat", |ui| {
                egui::Grid::new("wd-every").spacing([RADIO_GAP, ui.spacing().item_spacing.y]).show(ui, |ui| {
                    for (i, r) in WeekRepeat::ALL.into_iter().enumerate() {
                        if radio(ui, ed.wd_every == r, r.label()).clicked() {
                            ed.wd_every = r;
                        }
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });
                if ed.wd_every == WeekRepeat::BiWeekly {
                    ui.label(RichText::new("Every 2 weeks, counted from the starting day's week.").weak());
                }
            });
        }
        Mode::MonthDays => {
            sec.add(ui, "Repeat", |ui| {
                ui.label(RichText::new("Every month. Months without a selected day skip it.").weak());
            });
        }
    }
    sec.finish(ui);
}

pub fn editor_ui(ctx: &egui::Context, ed: &mut Editor, settings: &Settings) -> EditorResult {
    let mut result = EditorResult::Open;
    // Anchored at the top so a taller schedule only extends the dialog downwards.
    let top = ((ctx.content_rect().height() - 700.0) / 2.0).max(10.0);
    let area = Modal::default_area(Id::unique("task-editor")).anchor(Align2::CENTER_TOP, egui::vec2(0.0, top));
    let modal = Modal::new(Id::unique("task-editor")).area(area).frame(dialog_frame(ctx)).show(ctx, |ui| {
        ui.heading(if ed.task_id.is_some() { "Edit task" } else { "New task" });
        if ed.task_id.is_some() {
            ui.label(
                RichText::new("Changes apply to the task plan. Occurrences that were already started keep their data.")
                    .small()
                    .weak(),
            );
        }
        ui.add_space(8.0);
        let cols = ui.horizontal_top(|ui| {
            // every control in the columns (fields included) uses the control font
            ui.spacing_mut().button_padding = ACTION_PAD_SYM;
            for style in [TextStyle::Body, TextStyle::Button, TextStyle::Monospace] {
                ui.style_mut().text_styles.insert(style, FontId::proportional(CTRL_SIZE));
            }
            let left = ui.vertical(|ui| details_column(ui, ed, settings)).response.rect;
            ui.add_space(28.0);
            ui.vertical(|ui| {
                ui.set_min_height(ed.schedule_h);
                schedule_column(ui, ed);
                ed.schedule_h = ed.schedule_h.max(ui.min_rect().height());
            });
            left.right() + 14.0
        });
        let r = cols.response.rect;
        ui.painter().vline(cols.inner, r.y_range(), Stroke::new(1.0, GRID_LINE));

        if let Some(err) = &ed.error {
            ui.colored_label(Color32::from_rgb(240, 110, 100), err);
        }
        ui.add_space(SECTION_GAP - ui.spacing().item_spacing.y);
        ui.separator();
        ui.horizontal(|ui| {
            let save = action_button(ui, "Save", Act::Primary, true).on_hover_text("Ctrl+Enter");
            if save.clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl) {
                match ed.to_task() {
                    Ok(t) => result = EditorResult::Save(t),
                    Err(e) => ed.error = Some(e),
                }
            }
            if action_button(ui, "Cancel", Act::Plain, true).clicked() {
                result = EditorResult::Cancel;
            }
            if let Some(id) = ed.task_id {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ed.confirm_delete {
                        if action_button(ui, "Really delete", Act::Danger, true).clicked() {
                            result = EditorResult::Delete(id);
                        }
                        ui.label(RichText::new("History is kept.").weak());
                    } else if action_button(ui, "🗑 Delete task", Act::Plain, true).clicked() {
                        ed.confirm_delete = true;
                    }
                });
            }
        });
    });
    if modal.should_close() && matches!(result, EditorResult::Open) {
        result = EditorResult::Cancel;
    }
    result
}

// ============================================================================ statistics

/// The statistics show at most this many (most recent) occurrences.
const STATS_MAX_POINTS: usize = 30;

struct Point {
    date: NaiveDate,
    /// None: not worked on (missed or given up).
    tracked_min: Option<f32>,
    estimate_min: u32,
    delay_min: Option<i64>,
    done: bool,
    /// Given up ("I am bad and won't do it") instead of just missed.
    failed: bool,
}

/// The occurrences of `task` up to `today` (timestamp `now`), oldest first.
fn stats_points(store: &Store, task: &Task, today: NaiveDate, now: i64) -> Vec<Point> {
    let runs: Vec<&Run> = store.runs_of(task.id).collect();
    let start =
        runs.iter().map(|r| r.date).chain([task.schedule.first_date()]).min().unwrap().max(today - Duration::days(400));
    let mut pts = Vec::new();
    let mut d = start;
    while d <= today {
        let run = runs.iter().find(|r| r.date == d);
        if run.is_some() || task.schedule.occurs_on(d) {
            let p = match run {
                Some(r) => {
                    // Same rounding as the "started 5 min late" labels.
                    let delay = r.first_start().map(|s| minutes_from_start(d, r.start_min, s));
                    let failed = r.status == RunStatus::Failed;
                    Point {
                        date: d,
                        tracked_min: (!failed).then(|| r.tracked_secs(now) as f32 / 60.0),
                        estimate_min: r.estimate_min,
                        delay_min: delay,
                        done: r.status == RunStatus::Done,
                        failed,
                    }
                }
                None => Point {
                    date: d,
                    tracked_min: None,
                    estimate_min: task.estimate_min,
                    delay_min: None,
                    done: false,
                    failed: false,
                },
            };
            // today's not-yet-started occurrence isn't "missed" yet
            if !(d == today && run.is_none()) {
                pts.push(p);
            }
        }
        d += Duration::days(1);
    }
    let skip = pts.len().saturating_sub(STATS_MAX_POINTS);
    pts.into_iter().skip(skip).collect()
}

pub enum ListResult {
    Open,
    Close,
    Edit(i64),
}

/// One task in the task list: priority bar, title, an info line and an Edit button.
/// Returns true when Edit was clicked.
fn task_list_row(ui: &mut Ui, t: &Task, info: &str, info_color: Color32) -> bool {
    let mut edit = false;
    egui::Frame::new()
        .fill(Color32::from_gray(40))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (bar, _) = ui.allocate_exact_size(egui::vec2(4.0, 38.0), Sense::hover());
                ui.painter().rect_filled(bar, 2.0, t.priority.color());
                ui.vertical(|ui| {
                    ui.label(RichText::new(&t.title).strong().size(HEADLINE));
                    ui.label(RichText::new(info).color(info_color));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    edit = action_button(ui, "✏ Edit", Act::Plain, true).clicked();
                });
            });
        });
    ui.add_space(4.0);
    edit
}

/// All tasks in two lists: recurring tasks, and one-off tasks that haven't been started
/// yet (started ones are done with planning). One line each, with an edit button.
pub fn task_list_ui(ctx: &egui::Context, store: &Store) -> ListResult {
    let mut res = ListResult::Open;
    let mut recurring: Vec<&Task> = store.tasks().filter(|t| t.schedule.is_recurring()).collect();
    recurring.sort_by_key(|t| (t.start_min, t.title.to_lowercase()));
    let started = |t: &Task| store.runs_of(t.id).next().is_some();
    let mut one_off: Vec<&Task> = store.tasks().filter(|t| !t.schedule.is_recurring() && !started(t)).collect();
    one_off.sort_by_key(|t| (t.schedule.first_date(), t.start_min, t.title.to_lowercase()));
    let today = Local::now().date_naive();
    let screen = ctx.content_rect().size();
    let span = |t: &Task| {
        format!("{}   ·   est. {}", fmt_time_span(t.start_min, t.estimate_min, 0), fmt_duration_min(t.estimate_min))
    };
    let section = |ui: &mut Ui, title: &str, n: usize| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong().size(HEADLINE));
            ui.label(RichText::new(n.to_string()).weak());
        });
        ui.add_space(2.0);
    };
    let modal = Modal::new(Id::unique("task-list")).frame(dialog_frame(ctx)).show(ctx, |ui| {
        ui.set_width(640.0_f32.min(screen.x - 60.0).max(200.0));
        ui.horizontal(|ui| {
            ui.heading("☰ Tasks");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if action_button(ui, "Close", Act::Plain, true).clicked() {
                    res = ListResult::Close;
                }
            });
        });
        ui.add_space(6.0);
        // The modal is sized from the previous frame; ask for the list's full height (as
        // measured last frame) so it doesn't stay at its first, too small, size.
        let h_id = Id::unique("task-list-height");
        let max_h = (screen.y - 160.0).max(80.0);
        let last_h = ui.ctx().data(|d| d.get_temp::<f32>(h_id)).unwrap_or(0.0);
        let list =
            egui::ScrollArea::vertical().max_height(max_h).min_scrolled_height(last_h.min(max_h)).show(ui, |ui| {
                section(ui, "⟳ Recurring", recurring.len());
                if recurring.is_empty() {
                    ui.label(RichText::new("No recurring tasks.").weak());
                }
                for t in &recurring {
                    let mut info = format!("{}   ·   {}", t.schedule.describe(), span(t));
                    let first = t.schedule.first_date();
                    if first > today {
                        info.push_str(&format!("   ·   from {}", first.format("%d.%m.%Y")));
                    }
                    if task_list_row(ui, t, &info, Color32::from_gray(170)) {
                        res = ListResult::Edit(t.id);
                    }
                }
                ui.add_space(10.0);
                section(ui, "One-off, not started", one_off.len());
                if one_off.is_empty() {
                    ui.label(RichText::new("No open one-off tasks.").weak());
                }
                for t in &one_off {
                    let date = t.schedule.first_date();
                    let mut info = format!("{}   ·   {}", date.format("%a %d.%m.%Y"), span(t));
                    let color = if date < today {
                        info.push_str("   ·   (past)");
                        Color32::from_rgb(220, 150, 90)
                    } else {
                        Color32::from_gray(170)
                    };
                    if task_list_row(ui, t, &info, color) {
                        res = ListResult::Edit(t.id);
                    }
                }
            });
        ui.ctx().data_mut(|d| d.insert_temp(h_id, list.content_size.y));
    });
    if modal.should_close() && matches!(res, ListResult::Open) {
        res = ListResult::Close;
    }
    res
}

/// Returns false when closed.
pub fn stats_ui(ctx: &egui::Context, store: &Store, task_id: i64) -> bool {
    let Some(task) = store.task(task_id) else { return false };
    let now = Local::now();
    let pts = stats_points(store, task, now.date_naive(), now.timestamp());
    let mut open = true;
    let modal = Modal::new(Id::unique("stats")).frame(dialog_frame(ctx)).show(ctx, |ui| {
        ui.set_width(680.0);
        ui.horizontal(|ui| {
            ui.heading(format!("📊 {}", task.title));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if action_button(ui, "Close", Act::Plain, true).clicked() {
                    open = false;
                }
            });
        });
        ui.label(
            RichText::new(format!(
                "{} at {} · estimate {}",
                task.schedule.describe(),
                fmt_hm(task.start_min),
                fmt_duration_min(task.estimate_min)
            ))
            .weak(),
        );
        ui.add_space(6.0);

        if pts.is_empty() {
            ui.label("No past occurrences yet.");
            return;
        }
        // ---- numbers
        let n = pts.len();
        let started: Vec<&Point> = pts.iter().filter(|p| p.tracked_min.is_some()).collect();
        let done = pts.iter().filter(|p| p.done).count();
        let failed = pts.iter().filter(|p| p.failed).count();
        let avg = |v: &[f32]| if v.is_empty() { 0.0 } else { v.iter().sum::<f32>() / v.len() as f32 };
        let tracked: Vec<f32> = started.iter().filter_map(|p| p.tracked_min).collect();
        let delays: Vec<i64> = started.iter().filter_map(|p| p.delay_min).collect();
        let on_time = delays.iter().filter(|&&d| d <= 5).count();
        let over = started.iter().filter(|p| p.tracked_min.unwrap_or(0.0) > p.estimate_min as f32).count();
        egui::Grid::new("stats-grid").num_columns(4).spacing([24.0, 4.0]).show(ui, |ui| {
            let kv = |ui: &mut Ui, k: &str, v: String| {
                ui.label(RichText::new(k).weak());
                ui.label(RichText::new(v).strong());
            };
            kv(ui, "Occurrences", if n >= STATS_MAX_POINTS { format!("last {n}") } else { n.to_string() });
            kv(
                ui,
                "Started / done",
                format!("{} / {}  ({:.0}% done)", started.len(), done, done as f32 * 100.0 / n as f32),
            );
            ui.end_row();
            kv(ui, "Avg. tracked", fmt_duration_min(avg(&tracked).round() as u32));
            kv(ui, "Over estimate", format!("{over} of {}", started.len()));
            ui.end_row();
            kv(
                ui,
                "Started on time",
                if delays.is_empty() {
                    "–".into()
                } else {
                    format!("{on_time} of {} (≤ 5 min late)", delays.len())
                },
            );
            let avg_delay =
                if delays.is_empty() { 0.0 } else { delays.iter().sum::<i64>() as f32 / delays.len() as f32 };
            kv(ui, "Avg. start delay", if delays.is_empty() { "–".into() } else { format!("{avg_delay:+.0} min") });
            ui.end_row();
            kv(ui, "Total tracked", fmt_duration_min(tracked.iter().sum::<f32>().round() as u32));
            kv(ui, "Given up / missed", format!("{failed} / {}", n - started.len() - failed));
            ui.end_row();
        });
        ui.add_space(10.0);

        // ---- bar chart
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 240.0), Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, Color32::from_gray(28));
        let plot = Rect::from_min_max(rect.min + egui::vec2(44.0, 12.0), rect.max - egui::vec2(10.0, 34.0));
        let max_v = pts
            .iter()
            .map(|p| p.tracked_min.unwrap_or(0.0).max(p.estimate_min as f32))
            .fold(task.estimate_min.max(1) as f32, f32::max)
            * 1.15;
        let y_of = |v: f32| plot.bottom() - plot.height() * (v / max_v);
        for k in 0..=4 {
            let v = max_v * k as f32 / 4.0;
            let y = y_of(v);
            p.hline(plot.x_range(), y, Stroke::new(1.0, Color32::from_gray(48)));
            p.text(
                egui::pos2(plot.left() - 6.0, y),
                Align2::RIGHT_CENTER,
                format!("{:.0}m", v),
                FontId::proportional(10.0),
                Color32::from_gray(150),
            );
        }
        let slot = plot.width() / n as f32;
        let bw = (slot * 0.62).min(36.0);
        let mut est_line: Vec<egui::Pos2> = Vec::new();
        let changed_estimate = pts.iter().any(|p| p.estimate_min != task.estimate_min);
        let label_every = ((n as f32 * 44.0 / plot.width()).ceil() as usize).max(1);
        for (i, pt) in pts.iter().enumerate() {
            let cx = plot.left() + slot * (i as f32 + 0.5);
            let ey = y_of(pt.estimate_min as f32);
            est_line.push(egui::pos2(cx - slot / 2.0, ey));
            est_line.push(egui::pos2(cx + slot / 2.0, ey));
            match pt.tracked_min {
                Some(v) => {
                    let col = if v <= pt.estimate_min as f32 {
                        Color32::from_rgb(80, 160, 105)
                    } else {
                        Color32::from_rgb(220, 130, 60)
                    };
                    let bar = Rect::from_min_max(
                        egui::pos2(cx - bw / 2.0, y_of(v).min(plot.bottom() - 1.0)),
                        egui::pos2(cx + bw / 2.0, plot.bottom()),
                    );
                    p.rect_filled(bar, 2.0, if pt.done { col } else { col.gamma_multiply(0.6) });
                }
                // not worked on: a cross, on dark red if it was given up
                None => {
                    let mark = Rect::from_center_size(egui::pos2(cx, plot.bottom() - 8.0), egui::vec2(bw, 16.0));
                    if pt.failed {
                        p.rect_filled(mark, 2.0, FAILED_RED);
                    }
                    let col = if pt.failed { Color32::from_rgb(240, 150, 140) } else { Color32::from_rgb(210, 80, 80) };
                    p.text(mark.center(), Align2::CENTER_CENTER, "✕", FontId::proportional(12.0), col);
                }
            }
            // punctuality dot
            let dot = match pt.delay_min {
                Some(d) if d <= 5 => Color32::from_rgb(90, 190, 110),
                Some(d) if d <= 30 => Color32::from_rgb(225, 190, 70),
                Some(_) => Color32::from_rgb(225, 100, 70),
                None => Color32::from_gray(70),
            };
            p.circle_filled(egui::pos2(cx, plot.bottom() + 7.0), 3.0, dot);
            if i % label_every == 0 {
                p.text(
                    egui::pos2(cx, plot.bottom() + 14.0),
                    Align2::CENTER_TOP,
                    pt.date.format("%d.%m.").to_string(),
                    FontId::proportional(10.0),
                    Color32::from_gray(150),
                );
            }
            let hover = Rect::from_x_y_ranges(cx - slot / 2.0..=cx + slot / 2.0, plot.y_range());
            let tip = match pt.tracked_min {
                Some(v) => format!(
                    "{}\ntracked {} / est. {}{}\nstarted {}\n{}",
                    pt.date.format("%a %d.%m.%Y"),
                    fmt_duration_min(v.round() as u32),
                    fmt_duration_min(pt.estimate_min),
                    if pt.estimate_min != task.estimate_min {
                        format!(" (at the time; now {})", fmt_duration_min(task.estimate_min))
                    } else {
                        String::new()
                    },
                    pt.delay_min
                        .map(|d| if d <= 0 { format!("{} min early", -d) } else { format!("{d} min late") })
                        .unwrap_or_default(),
                    if pt.done { "done" } else { "not finished" }
                ),
                None if pt.failed => format!("{}\ngiven up (marked as failed)", pt.date.format("%a %d.%m.%Y")),
                None => format!("{}\nmissed (not started)", pt.date.format("%a %d.%m.%Y")),
            };
            ui.interact(hover, Id::unique(("stat-bar", i)), Sense::hover()).on_hover_text(tip);
        }
        // Started occurrences keep the estimate they had when started (dashed, only where it
        // differs); the solid line is the task's current estimate.
        let cur_col = Color32::from_rgb(200, 200, 230);
        let old_col = Color32::from_rgb(170, 140, 220);
        if changed_estimate {
            p.add(egui::Shape::dashed_line(&est_line, Stroke::new(1.5, old_col), 6.0, 4.0));
        }
        p.hline(plot.x_range(), y_of(task.estimate_min as f32), Stroke::new(1.5, cur_col));
        // As wide as the chart: entries that don't fit move to the next line as a whole.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            let text_color = ui.visuals().text_color();
            // a color square (if any) and its text, as one piece
            let entry = |ui: &mut Ui, square: Option<Color32>, t: &str, color: Color32| {
                let g = ui.fonts_mut(|f| f.layout_no_wrap(t.to_string(), FontId::proportional(TEXT), color));
                let pad = if square.is_some() { 16.0 } else { 0.0 };
                let (r, _) = ui.allocate_exact_size(egui::vec2(pad + g.size().x, g.size().y), Sense::hover());
                if let Some(c) = square {
                    let sq = Rect::from_center_size(egui::pos2(r.left() + 5.0, r.center().y), egui::vec2(10.0, 10.0));
                    ui.painter().rect_filled(sq, 2.0, c);
                }
                ui.painter().galley(egui::pos2(r.left() + pad, r.top()), g, color);
            };
            entry(ui, Some(Color32::from_rgb(80, 160, 105)), "within estimate", text_color);
            entry(ui, Some(Color32::from_rgb(220, 130, 60)), "over estimate", text_color);
            entry(ui, Some(cur_col), "— current estimate", text_color);
            if changed_estimate {
                entry(ui, Some(old_col), "- - estimate when started", text_color);
            }
            let weak = ui.visuals().weak_text_color();
            entry(ui, None, "✕ missed", weak);
            entry(ui, None, "● on time / late (dot below bar)", weak);
        });
    });
    if modal.should_close() {
        open = false;
    }
    open
}

// ============================================================================ settings

pub struct SettingsDialog {
    draft: Settings,
    browse_result: Arc<Mutex<Option<String>>>,
    /// The values in the fields for adding a preset.
    new_start: u32,
    new_time: u32,
    /// UI scale slider value in percent (applied to `draft` on release).
    scale_pct: u32,
}

impl SettingsDialog {
    pub fn new(s: Settings) -> Self {
        let scale_pct = (s.ui_scale * 100.0).round() as u32;
        SettingsDialog {
            draft: s,
            browse_result: Arc::new(Mutex::new(None)),
            new_start: 10 * 60,
            new_time: 20,
            scale_pct,
        }
    }
    /// The UI scale being previewed.
    pub fn ui_scale(&self) -> f32 {
        self.draft.ui_scale
    }
    pub fn set_ui_scale(&mut self, f: f32) {
        self.draft.ui_scale = f;
        self.scale_pct = (f * 100.0).round() as u32;
    }
}

/// Settings for a list of preset values (minutes, up to `max`): a pill per value that
/// removes it, hour and minute fields for a new one (`new`, a time of day or a length of
/// time: `kind`) with an Add button, and a button that restores the `defaults`. `label`
/// is how a value is shown.
fn preset_list(
    ui: &mut Ui,
    list: &mut Vec<u32>,
    new: &mut u32,
    (max, kind): (u32, TimeKind),
    defaults: &[u32],
    label: impl Fn(u32) -> String,
) {
    preset_row(ui, |ui| {
        let mut remove = None;
        for (i, &v) in list.iter().enumerate() {
            if preset_button(ui, false, format!("{}  ✕", label(v))).on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
        }
        if let Some(i) = remove {
            list.remove(i);
        }
        if list.is_empty() {
            ui.label(RichText::new("none").weak());
        }
    });
    ui.horizontal(|ui| {
        // as big as the Add button next to it
        for style in [TextStyle::Button, TextStyle::Body] {
            ui.style_mut().text_styles.insert(style, FontId::proportional(ACTION_TEXT));
        }
        ui.spacing_mut().button_padding = ACTION_PAD_SYM;
        minutes_fields(ui, new, max / 60, kind);
        *new = (*new).min(max);
        if action_button(ui, "Add", Act::Plain, true).clicked() && !list.contains(new) {
            list.push(*new);
            list.sort_unstable();
        }
        if preset_button(ui, list == defaults, "Defaults").clicked() {
            *list = defaults.to_vec();
        }
    });
}

pub enum SettingsResult {
    Open,
    Cancel,
    Save(Settings),
    TestSound(Settings),
    StopSound,
}

pub fn settings_ui(ctx: &egui::Context, dlg: &mut SettingsDialog, audio: &Audio) -> SettingsResult {
    let mut result = SettingsResult::Open;
    if let Some(path) = dlg.browse_result.lock().ok().and_then(|mut r| r.take()) {
        dlg.draft.sound_file = path;
        dlg.draft.sound = SoundChoice::File;
    }
    let s = &mut dlg.draft;
    // At large UI scales the dialog may not fit the window: scroll the settings, keep
    // Save / Cancel visible.
    let screen = ctx.content_rect().size();
    let modal = Modal::new(Id::unique("settings")).frame(dialog_frame(ctx)).show(ctx, |ui| {
        ui.heading("⚙ Settings");
        ui.add_space(6.0);
        // A fixed width (longer preset lists wrap); only the height is limited (vertical scrolling).
        egui::ScrollArea::vertical().max_height((screen.y - 150.0).max(80.0)).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width(SETTINGS_W);
                let mut sections = Sections::default();
                sections.add(ui, "Notification corner", |ui| {
                    let corners = [
                        (Corner::TopLeft, "↖ Top left"),
                        (Corner::TopRight, "↗ Top right"),
                        (Corner::BottomLeft, "↙ Bottom left"),
                        (Corner::BottomRight, "↘ Bottom right"),
                    ];
                    egui::Grid::new("corner").spacing([RADIO_GAP, ui.spacing().item_spacing.y]).show(ui, |ui| {
                        for (i, (c, label)) in corners.into_iter().enumerate() {
                            if radio(ui, s.corner == c, label).clicked() {
                                s.corner = c;
                            }
                            if i % 2 == 1 {
                                ui.end_row();
                            }
                        }
                    });
                });

                sections.add(ui, "Start time presets", |ui| {
                    let of_day = (24 * 60 - 1, TimeKind::OfDay);
                    preset_list(ui, &mut s.start_presets, &mut dlg.new_start, of_day, &DEFAULT_START_PRESETS, fmt_hm);
                });

                sections.add(ui, "Time presets", |ui| {
                    preset_list(ui, &mut s.time_presets, &mut dlg.new_time, (MAX_DURATION_MIN, TimeKind::Span), &DEFAULT_TIME_PRESETS, fmt_duration_min);
                });

                sections.add(ui, "Sound", |ui| {
                    egui::ComboBox::from_id_salt("sound").selected_text(s.sound.label()).width(200.0).show_ui(ui, |ui| {
                        for c in SoundChoice::ALL {
                            ui.selectable_value(&mut s.sound, c, c.label());
                        }
                    });
                    if s.sound == SoundChoice::File {
                        ui.horizontal(|ui| {
                            ui.scope(|ui| {
                                input_style(ui);
                                ui.add(egui::TextEdit::singleline(&mut s.sound_file).desired_width(300.0).hint_text("path of a sound file"));
                            });
                            if platform::can_pick_file() && action_button(ui, "Browse…", Act::Plain, true).clicked() {
                                let out = dlg.browse_result.clone();
                                let ctx = ctx.clone();
                                std::thread::spawn(move || {
                                    if let Some(p) = platform::pick_sound_file() {
                                        if let Ok(mut r) = out.lock() {
                                            *r = Some(p);
                                        }
                                        ctx.request_repaint();
                                    }
                                });
                            }
                        });
                        ui.label(RichText::new("WAV, MP3, OGG/Vorbis or FLAC").weak());
                    }
                });

                sections.add(ui, "Volume ramp", |ui| {
                    percent_slider(ui, &mut s.start_volume, "start volume");
                    percent_slider(ui, &mut s.end_volume, "end volume");
                    ui.horizontal(|ui| {
                        ui.spacing_mut().slider_width = SLIDER_W;
                        ui.add(egui::Slider::new(&mut s.ramp_secs, 0.0..=300.0).show_value(false));
                        number_field(ui, &mut s.ramp_secs, 0.0..=300.0, 1.0, VALUE_FIELD_W, |f| f.max_decimals(0).suffix(" s"));
                        ui.label("ramp time");
                    });
                    ui.horizontal(|ui| {
                        if action_button(ui, "▶ Test", Act::Plain, true).clicked() {
                            result = SettingsResult::TestSound(s.clone());
                        }
                        if action_button(ui, "■ Stop", Act::Plain, true).clicked() {
                            result = SettingsResult::StopSound;
                        }
                    });
                    if let Some(err) = audio.error() {
                        ui.colored_label(Color32::from_rgb(240, 130, 100), format!("⚠ {err}"));
                    }
                });

                sections.add(ui, "UI scale", |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().slider_width = SLIDER_W;
                        let slider = ui.add(egui::Slider::new(&mut dlg.scale_pct, UI_SCALE_PCT).logarithmic(true).show_value(false));
                        let r = slider | number_field(ui, &mut dlg.scale_pct, UI_SCALE_PCT, 1.0, VALUE_FIELD_W, |f| f.suffix(" %"));
                        // Rescaling while dragging would move the slider away under the mouse,
                        // so the preview is applied on release (or after typing a value).
                        if (r.changed() && !r.dragged()) || r.drag_stopped() {
                            s.ui_scale = dlg.scale_pct as f32 / 100.0;
                        }
                    });
                    preset_row(ui, |ui| {
                        for pct in [80u32, 100, 125, 150, 200] {
                            if preset_button(ui, dlg.scale_pct == pct, format!("{pct} %")).clicked() {
                                dlg.scale_pct = pct;
                                s.ui_scale = pct as f32 / 100.0;
                            }
                        }
                    });
                    ui.label(
                        RichText::new("Applied when you release the slider; click the number to type a value.\nKeyboard Shortcuts: Ctrl + / Ctrl − / Ctrl 0.")
                            .weak(),
                    );
                });

                sections.add(ui, "Window", |ui| {
                    ui.checkbox(&mut s.close_to_tray, "Close button minimizes to the panel instead of quitting");
                    ui.checkbox(&mut s.start_minimized, "Start minimized to the panel (or start with --minimized)");
                });
                sections.finish(ui);
            });
        });
        ui.add_space(SECTION_GAP - ui.spacing().item_spacing.y);
        ui.separator();
        ui.horizontal(|ui| {
            if action_button(ui, "Save", Act::Primary, true).clicked() {
                result = SettingsResult::Save(s.clone());
            }
            if action_button(ui, "Cancel", Act::Plain, true).clicked() {
                result = SettingsResult::Cancel;
            }
        });
    });
    if modal.should_close() && matches!(result, SettingsResult::Open) {
        result = SettingsResult::Cancel;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }

    fn editor(title: &str) -> Editor {
        let mut ed = Editor::new(d(11, 6));
        ed.title = title.into();
        ed
    }

    #[test]
    fn editor_validation() {
        let mut ed = editor("   ");
        assert!(ed.to_task().is_err(), "blank title");
        ed.title = "  Gym  ".into();
        let t = ed.to_task().unwrap();
        assert_eq!(t.title, "Gym", "title is trimmed");
        assert_eq!(t.schedule, Schedule::Date { date: d(11, 6), repeat: Repeat::Never });
        ed.mode = Mode::Weekdays;
        assert!(ed.to_task().is_err(), "weekdays without a day");
        ed.mode = Mode::MonthDays;
        assert!(ed.to_task().is_err(), "month days without a day");
    }

    #[test]
    fn start_date_snaps_to_the_first_matching_day() {
        let mut ed = editor("Standup");
        ed.mode = Mode::Weekdays;
        ed.wd_mask = 0b001_1111; // Mon–Fri
        ed.from = d(11, 7); // a Saturday
        assert!(!ed.can_start_on(d(11, 7)));
        assert!(ed.can_start_on(d(11, 9)));
        assert_eq!(ed.effective_from(), d(11, 9), "the next Monday");
        let t = ed.to_task().unwrap();
        assert_eq!(t.schedule, Schedule::Weekdays { mask: 0b001_1111, from: d(11, 9), every: WeekRepeat::Weekly });

        ed.mode = Mode::MonthDays;
        ed.md_mask = 1 << 31;
        ed.from = d(11, 1);
        assert_eq!(ed.effective_from(), d(12, 31), "November has no 31st");
    }

    #[test]
    fn editing_keeps_everything() {
        // 30 Jan 2026 is the last Friday of January (saved tasks always start on an occurrence)
        let mut t = test_task(
            "Retro",
            Schedule::Weekdays { mask: 1 << 4, from: d(1, 30), every: WeekRepeat::LastInMonth },
            15 * 60 + 30,
            75,
        );
        t.id = 42;
        t.notes = "bring cake".into();
        t.priority = Priority::Urgent;
        t.notify = NotifyMode::Silent;
        t.notify_ahead_min = 15;
        t.created_at = 123;
        let back = Editor::from_task(&t).to_task().unwrap();
        assert_eq!(back.id, 42);
        assert_eq!(back.schedule, t.schedule);
        assert_eq!((back.start_min, back.estimate_min), (t.start_min, t.estimate_min));
        assert_eq!((back.priority, back.notify, back.notify_ahead_min), (t.priority, t.notify, 15));
        assert_eq!((back.notes.as_str(), back.created_at), ("bring cake", 123));
    }

    #[test]
    fn statistics_points() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        let from = today - Duration::days(4);
        let id = s
            .save_task(test_task(
                "Mail",
                Schedule::Weekdays { mask: 0b111_1111, from, every: WeekRepeat::Weekly },
                8 * 60,
                30,
            ))
            .unwrap();
        // day 0 missed; day 1 started 5 min late and worked 20 min; day 2 missed; day 3 given
        // up; today not started
        let late = scheduled_at(from + Duration::days(1), 8 * 60 + 5).and_local_timezone(Local).unwrap().timestamp();
        let run = s.start(id, from + Duration::days(1), late).unwrap();
        s.finish(run, late + 20 * 60).unwrap();
        s.fail(id, from + Duration::days(3), late).unwrap();
        let pts = stats_points(&s, s.task(id).unwrap(), today, late + 30 * 60);
        assert_eq!(
            pts.iter().map(|p| p.date).collect::<Vec<_>>(),
            (0..4).map(|i| from + Duration::days(i)).collect::<Vec<_>>(),
            "today isn't missed yet"
        );
        assert_eq!(pts[1].tracked_min, Some(20.0));
        assert_eq!(pts[1].delay_min, Some(5));
        assert!(pts[1].done);
        assert!(pts[0].tracked_min.is_none() && pts[2].tracked_min.is_none(), "missed days");
        assert!(!pts[0].failed && !pts[2].failed, "missed isn't given up");
        assert!(pts[3].failed && pts[3].tracked_min.is_none() && !pts[3].done, "given up");
    }
}
