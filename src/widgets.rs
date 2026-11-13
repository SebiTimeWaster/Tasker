//! Task widgets shared by the day view and the reminder windows.

use chrono::Local;
use egui::{Color32, CornerRadius, RichText, Ui};

use crate::action::Action;
use crate::model::*;

/// Background of a task that was given up, in the day view and the statistics.
pub const FAILED_RED: Color32 = Color32::from_rgb(96, 24, 24);

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

// ---------------------------------------------------------------------------- text sizes

/// Text size of headlines: dialog titles, reminder titles, the month in the calendars,
/// the weekday names in the week list.
pub const HEADLINE: f32 = 18.0;
/// Text size of everything else (the global body, button and small text styles).
pub const TEXT: f32 = 16.0;

// ---------------------------------------------------------------------------- buttons
//
// Tasker has two kinds of buttons, each with one look everywhere:
// - action buttons do something (Start, Pause, Save, Cancel, Dismiss, ✏, …) or pick one
//   of several options (priority, corner, …): rectangular, white text, the main action
//   of a group filled with a color ([`action_button`]), a chosen option highlighted
//   ([`choice_button`]), icon-only ones the same ([`icon_button`]);
// - preset buttons fill in or apply a ready-made value (time and duration presets,
//   snooze times, "+10 min"): smaller rounded pills, highlighted while their value is the
//   current one ([`preset_button`]).
//
// They are drawn here rather than with `egui::Button`, whose padding is the same above
// and below the text: text needs a bit more room below it to look centered.

/// Fill of the main action of a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// No fill.
    Plain,
    /// Green: start / finish / continue work.
    Go,
    /// Blue: confirm a dialog (Save) or create something.
    Primary,
    /// Red: delete.
    Danger,
}

pub const GO_GREEN: Color32 = Color32::from_rgb(46, 125, 70);
pub const PRIMARY_BLUE: Color32 = Color32::from_rgb(52, 92, 160);
const DANGER_RED: Color32 = Color32::from_rgb(150, 40, 40);
/// Text size of action (and choice and icon) buttons.
pub const ACTION_TEXT: f32 = 16.0;
/// Padding around the text of action buttons (more below than above, see above).
pub const ACTION_PAD: egui::Margin = egui::Margin { left: 10, right: 10, top: 4, bottom: 7 };
/// The same height for egui's own controls (number fields), whose padding is symmetric.
pub const ACTION_PAD_SYM: egui::Vec2 = egui::vec2(10.0, 5.5);
const PRESET_TEXT: f32 = 14.0;
const PRESET_PAD: egui::Margin = egui::Margin { left: 8, right: 8, top: 2, bottom: 4 };

/// Height of an action button.
pub fn action_height(ui: &Ui) -> f32 {
    ui.fonts_mut(|f| f.row_height(&egui::FontId::proportional(ACTION_TEXT))) + ACTION_PAD.sum().y
}

struct Look {
    pad: egui::Margin,
    radius: u8,
    /// A fill instead of the normal button background.
    fill: Option<Color32>,
    /// Highlighted as the chosen option (None: not a choice).
    selected: Option<bool>,
    enabled: bool,
    min_size: egui::Vec2,
}

fn paint_button(ui: &mut Ui, text: RichText, look: Look) -> egui::Response {
    let label = text.text().to_string();
    let galley = egui::WidgetText::from(text).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let size = (galley.size() + look.pad.sum()).max(look.min_size);
    let Look { pad, radius, fill, selected, enabled, .. } = look;
    ui.scope(|ui| {
        if !enabled {
            ui.disable();
        }
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::labeled(egui::Role::Button, enabled, &label);
            info.selected = selected;
            info
        });
        if ui.is_rect_visible(rect) {
            let on = selected.unwrap_or(false);
            let v = ui.style().interact_selectable(&response, on);
            let bg = fill.filter(|_| !on).unwrap_or(v.weak_bg_fill);
            let frame = rect.expand(v.expansion);
            ui.painter().rect(frame, CornerRadius::same(radius), bg, v.bg_stroke, egui::StrokeKind::Inside);
            // Centered horizontally; vertically centered with the extra room below the text.
            let extra = (size - galley.size() - pad.sum()) / 2.0;
            let pos = egui::pos2(rect.center().x - galley.size().x / 2.0, rect.top() + pad.top as f32 + extra.y);
            ui.painter().galley(pos, galley, v.text_color());
        }
        response
    })
    .inner
}

/// An action button (see above). Disabled buttons are shown dimmed.
pub fn action_button(ui: &mut Ui, text: impl Into<String>, kind: Act, enabled: bool) -> egui::Response {
    let fill = match kind {
        Act::Plain => None,
        Act::Go => Some(GO_GREEN),
        Act::Primary => Some(PRIMARY_BLUE),
        Act::Danger => Some(DANGER_RED),
    };
    let text = RichText::new(text).size(ACTION_TEXT).color(Color32::WHITE);
    paint_button(
        ui,
        text,
        Look { pad: ACTION_PAD, radius: 4, fill, selected: None, enabled, min_size: egui::Vec2::ZERO },
    )
}

/// A button showing only an icon: an action button, square.
pub fn icon_button(ui: &mut Ui, icon: &str) -> egui::Response {
    let h = action_height(ui);
    let text = RichText::new(icon).size(ACTION_TEXT).color(Color32::WHITE);
    let min_size = egui::vec2(h, h);
    paint_button(ui, text, Look { pad: ACTION_PAD, radius: 4, fill: None, selected: None, enabled: true, min_size })
}

/// An action button for one of several options; `on`: this option is the chosen one.
pub fn choice_button(ui: &mut Ui, on: bool, text: impl Into<String>) -> egui::Response {
    choice_button_ext(ui, on, text, Color32::WHITE, egui::Vec2::ZERO)
}

/// [`choice_button`] with a text color (while not chosen) and a minimum size.
pub fn choice_button_ext(
    ui: &mut Ui,
    on: bool,
    text: impl Into<String>,
    color: Color32,
    min_size: egui::Vec2,
) -> egui::Response {
    let text = RichText::new(text).size(ACTION_TEXT).color(if on { Color32::WHITE } else { color });
    paint_button(ui, text, Look { pad: ACTION_PAD, radius: 4, fill: None, selected: Some(on), enabled: true, min_size })
}

/// A preset button (see above); `on`: its value is the current one.
pub fn preset_button(ui: &mut Ui, on: bool, text: impl Into<String>) -> egui::Response {
    let text = RichText::new(text).size(PRESET_TEXT);
    let text = if on { text.color(Color32::WHITE) } else { text.color(Color32::from_gray(200)) };
    let look =
        Look { pad: PRESET_PAD, radius: 11, fill: None, selected: Some(on), enabled: true, min_size: egui::Vec2::ZERO };
    paint_button(ui, text, look)
}

/// A row of preset buttons with the spacing that suits them.
pub fn preset_row<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        add(ui)
    })
    .inner
}

/// The button that gives a task up (marks it as failed).
const GIVE_UP: &str = "I am bad and won't do it";

/// The state-dependent buttons of a task, shared by the task list and the notifications.
/// `give_up_wait` is the number of seconds until the give-up button can be clicked; it is
/// counted down in the button (0 = it can be clicked).
pub fn task_buttons(ui: &mut Ui, e: &Entry, give_up_wait: u64) -> Option<Action> {
    let mut out = None;
    let mut b = |ui: &mut Ui, text: &str, kind: Act, enabled: bool, a: Action| {
        if action_button(ui, text, kind, enabled).clicked() {
            out = Some(a);
        }
    };
    match (e.status, e.run_id) {
        (EntryStatus::Virtual, _) => {
            b(ui, "▶ Start task", Act::Go, true, Action::Start { task_id: e.task_id, date: e.date });
            let give_up = Action::Fail { task_id: e.task_id, date: e.date };
            match give_up_wait {
                0 => b(ui, GIVE_UP, Act::Plain, true, give_up),
                secs => b(ui, &format!("{GIVE_UP} ({secs})"), Act::Plain, false, give_up),
            }
        }
        (EntryStatus::Running, Some(id)) => {
            b(ui, "⏸ Pause", Act::Plain, true, Action::Pause(id));
            b(ui, "✔ Finish", Act::Go, true, Action::Finish(id));
        }
        (EntryStatus::Paused, Some(id)) => {
            b(ui, "▶ Continue", Act::Go, true, Action::Resume(id));
            b(ui, "✔ Finish", Act::Plain, true, Action::Finish(id));
        }
        (EntryStatus::Done, Some(id)) => b(ui, "⟲ Resume", Act::Plain, true, Action::Resume(id)),
        (EntryStatus::Failed, Some(id)) => b(ui, "▶ Do it after all", Act::Plain, true, Action::Resume(id)),
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
        let at =
            chrono::DateTime::from_timestamp(until, 0).map(|t| t.with_timezone(&Local).format("%H:%M").to_string());
        let mins = (until - now + 59) / 60;
        ui.label(
            RichText::new(format!("⏰ snoozed, reminds again {} (in {})", at.unwrap_or_default(), fmt_span_min(mins)))
                .color(Color32::from_rgb(200, 180, 120)),
        )
        .on_hover_text("The reminder was snoozed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_blend() {
        let (a, b) = (Color32::from_rgb(0, 100, 200), Color32::from_rgb(200, 100, 0));
        assert_eq!(lerp_color(a, b, 0.0), a);
        assert_eq!(lerp_color(a, b, 1.0), b);
        assert_eq!(lerp_color(a, b, 0.5), Color32::from_rgb(100, 100, 100));
    }
}
