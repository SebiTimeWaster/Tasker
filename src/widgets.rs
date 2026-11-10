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

/// A button in the look of the task buttons (white text, optionally colored).
pub fn task_button(ui: &mut Ui, text: &str, fill: Option<Color32>, enabled: bool) -> egui::Response {
    let mut btn = egui::Button::new(RichText::new(text).color(Color32::WHITE)).corner_radius(CornerRadius::same(4));
    if let Some(f) = fill {
        btn = btn.fill(f);
    }
    ui.add_enabled(enabled, btn)
}

/// The button that gives a task up (marks it as failed).
const GIVE_UP: &str = "I am bad and won't do it";

/// The state-dependent buttons of a task, shared by the task list and the notifications.
/// `give_up_wait` is the number of seconds until the give-up button can be clicked; it is
/// counted down in the button (0 = it can be clicked).
pub fn task_buttons(ui: &mut Ui, e: &Entry, give_up_wait: u64) -> Option<Action> {
    let mut out = None;
    let green = Color32::from_rgb(46, 125, 70);
    let mut b = |ui: &mut Ui, text: &str, fill: Option<Color32>, enabled: bool, a: Action| {
        if task_button(ui, text, fill, enabled).clicked() {
            out = Some(a);
        }
    };
    match (e.status, e.run_id) {
        (EntryStatus::Virtual, _) => {
            b(ui, "▶ Start task", Some(green), true, Action::Start { task_id: e.task_id, date: e.date });
            let give_up = Action::Fail { task_id: e.task_id, date: e.date };
            match give_up_wait {
                0 => b(ui, GIVE_UP, None, true, give_up),
                secs => b(ui, &format!("{GIVE_UP} ({secs})"), None, false, give_up),
            }
        }
        (EntryStatus::Running, Some(id)) => {
            b(ui, "⏸ Pause", None, true, Action::Pause(id));
            b(ui, "✔ Finish", Some(green), true, Action::Finish(id));
        }
        (EntryStatus::Paused, Some(id)) => {
            b(ui, "▶ Continue", Some(green), true, Action::Resume(id));
            b(ui, "✔ Finish", None, true, Action::Finish(id));
        }
        (EntryStatus::Done, Some(id)) => b(ui, "⟲ Resume", None, true, Action::Resume(id)),
        (EntryStatus::Failed, Some(id)) => b(ui, "▶ Do it after all", None, true, Action::Resume(id)),
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
