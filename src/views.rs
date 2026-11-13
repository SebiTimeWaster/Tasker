//! The three main columns: calendar + week list, 24h timeline, task list — and the
//! connector lines that join task rows to their time span on the timeline.

use std::sync::Arc;

use chrono::{Datelike, Duration, NaiveDate};
use egui::epaint::Mesh;
use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align, Align2, Color32, FontId, Frame, Galley, Id, Layout, Margin, Pos2, Rect, RichText, ScrollArea, Sense, Stroke,
    Ui, UiBuilder,
};

use crate::action::Action;
use crate::app::{GRID_LINE, TaskerApp, now_min, now_ts, snoozed_until, today};
use crate::dialogs::Editor;
use crate::model::*;
use crate::widgets::{
    ACTION_TEXT, Act, FAILED_RED, HEADLINE, PRIMARY_BLUE, TEXT, action_button, action_height, icon_button,
    preset_button, preset_row, start_offset_labels, task_buttons,
};

const ACCENT: Color32 = Color32::from_rgb(90, 140, 210);

pub fn elided(ui: &Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping::truncate_at_width(width.max(10.0));
    ui.fonts_mut(|f| f.layout_job(job))
}

// ============================================================================ left column

pub fn left_column(app: &mut TaskerApp, ui: &mut Ui) {
    let store = &app.store;
    let counts = |d: NaiveDate| store.count_shown_on(d);
    let mut month = app.month;
    let mut selected = app.selected;
    month_header(ui, &mut month, &mut selected, true);
    month_grid(ui, "main-cal", month, &mut selected, Some(&counts), None, 34.0);
    if selected != app.selected {
        app.selected = selected;
        // jump the month if a day of an adjacent month was clicked
        month = first_of_month(selected);
    }
    app.month = month;

    ui.add_space(8.0);
    let avail = ui.available_rect_before_wrap();
    let btn_h = 40.0;
    let week_rect = Rect::from_min_max(avail.min, egui::pos2(avail.max.x, avail.max.y - btn_h - 10.0));
    let btn_rect = Rect::from_min_max(egui::pos2(avail.min.x, avail.max.y - btn_h), avail.max);
    week_list(app, ui, week_rect);

    let btn = egui::Button::new(RichText::new("+  Add task").size(ACTION_TEXT).strong().color(Color32::WHITE))
        .fill(PRIMARY_BLUE)
        .corner_radius(4);
    if ui.place(btn_rect, btn).clicked() {
        app.editor = Some(Editor::new(app.selected));
    }
}

/// "◀ November 2026 ▶  [Today]". Returns true if changed.
pub fn month_header(ui: &mut Ui, month: &mut NaiveDate, selected: &mut NaiveDate, today_button: bool) {
    ui.horizontal(|ui| {
        if action_button(ui, "◀", Act::Plain, true).clicked() {
            *month = add_months(*month, -1);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if today_button && action_button(ui, "Today", Act::Plain, true).clicked() {
                *selected = today();
                *month = first_of_month(*selected);
            }
            if action_button(ui, "▶", Act::Plain, true).clicked() {
                *month = add_months(*month, 1);
            }
            let title = month.format("%B %Y").to_string();
            ui.add_sized([ui.available_width(), 26.0], egui::Label::new(RichText::new(title).size(HEADLINE).strong()));
        });
    });
}

/// A month grid (Mon-first, 6 weeks). Clicking a day sets `selected`.
/// Days for which `enabled` returns false are dimmed and can't be clicked.
pub fn month_grid(
    ui: &mut Ui,
    id_salt: &str,
    month: NaiveDate,
    selected: &mut NaiveDate,
    counts: Option<&dyn Fn(NaiveDate) -> usize>,
    enabled: Option<&dyn Fn(NaiveDate) -> bool>,
    cell_h: f32,
) {
    let width = ui.available_width();
    let cell_w = width / 7.0;
    let (head, _) = ui.allocate_exact_size(egui::vec2(width, 22.0), Sense::hover());
    for (i, name) in WEEKDAY_SHORT.iter().enumerate() {
        let c = egui::pos2(head.left() + cell_w * (i as f32 + 0.5), head.center().y);
        let col = if i >= 5 { Color32::from_gray(140) } else { Color32::from_gray(170) };
        ui.painter().text(c, Align2::CENTER_CENTER, &name[..2], FontId::proportional(TEXT), col);
    }
    let (grid, _) = ui.allocate_exact_size(egui::vec2(width, cell_h * 6.0), Sense::hover());
    let start = monday_of(month);
    let today = today();
    for i in 0..42 {
        let d = start + Duration::days(i);
        let (row, col) = (i / 7, i % 7);
        let r = Rect::from_min_size(
            egui::pos2(grid.left() + col as f32 * cell_w, grid.top() + row as f32 * cell_h),
            egui::vec2(cell_w, cell_h),
        )
        .shrink(1.5);
        let on = enabled.is_none_or(|f| f(d));
        let resp = ui.interact(r, Id::unique((id_salt, i)), if on { Sense::click() } else { Sense::hover() });
        let in_month = d.month() == month.month();
        let p = ui.painter();
        if d == *selected {
            p.rect_filled(r, 5.0, ACCENT);
        } else if resp.hovered() && on {
            p.rect_filled(r, 5.0, Color32::from_gray(60));
        }
        if d == today {
            p.rect_stroke(r, 5.0, Stroke::new(1.5, Color32::from_rgb(240, 200, 90)), egui::StrokeKind::Inside);
        }
        let color = match (d == *selected, in_month, on) {
            (true, ..) => Color32::WHITE,
            (false, _, false) => Color32::from_gray(if in_month { 78 } else { 58 }),
            (false, true, true) => Color32::from_gray(225),
            (false, false, true) => Color32::from_gray(110),
        };
        let has_counts = counts.is_some();
        let num_y = if has_counts { r.center().y - 4.0 } else { r.center().y };
        let font = FontId::proportional(TEXT);
        p.text(egui::pos2(r.center().x, num_y), Align2::CENTER_CENTER, d.day().to_string(), font, color);
        if let Some(counts) = counts {
            let n = counts(d);
            if n > 0 {
                let dots = n.min(4);
                let dot_col = if d == *selected {
                    Color32::WHITE
                } else if in_month {
                    ACCENT
                } else {
                    Color32::from_gray(90)
                };
                let total_w = dots as f32 * 6.0 - 2.0;
                for k in 0..dots {
                    let x = r.center().x - total_w / 2.0 + k as f32 * 6.0 + 2.0;
                    p.circle_filled(egui::pos2(x, r.bottom() - 6.0), 2.0, dot_col);
                }
            }
        }
        if resp.clicked() && on {
            *selected = d;
        }
    }
}

/// The entry of `date` (out of the ones shown on it) to name in the week list.
fn next_task(entries: &[Entry], date: NaiveDate) -> Option<&Entry> {
    let mut open = entries.iter().filter(|e| !e.status.is_closed());
    if date == today() {
        let nm = now_min() as i64;
        // running first, then the next upcoming one, then the oldest overdue one
        entries
            .iter()
            .find(|e| e.status == EntryStatus::Running)
            .or_else(|| entries.iter().find(|e| !e.status.is_closed() && e.start_on(date) >= nm))
            .or_else(|| open.next())
    } else {
        open.next()
    }
}

fn week_list(app: &mut TaskerApp, ui: &mut Ui, rect: Rect) {
    let monday = monday_of(app.selected);
    let row_h = rect.height() / 7.0;
    let now = now_ts();
    let today = today();
    let p = ui.painter().clone();
    p.hline(rect.x_range(), rect.top(), Stroke::new(1.0, GRID_LINE));
    let big = HEADLINE;
    // The dates form a column right of the longest weekday name.
    let name_w = WEEKDAY_LONG
        .iter()
        .map(|n| ui.fonts_mut(|f| f.layout_no_wrap(n.to_string(), FontId::proportional(big), Color32::WHITE).size().x))
        .fold(0.0, f32::max);
    for i in 0..7 {
        let d = monday + Duration::days(i);
        let r = Rect::from_min_size(
            egui::pos2(rect.left(), rect.top() + row_h * i as f32),
            egui::vec2(rect.width(), row_h),
        );
        let resp = ui.interact(r, Id::unique(("week-row", i)), Sense::click());
        if d == app.selected {
            p.rect_filled(r.shrink2(egui::vec2(0.0, 1.0)), 0.0, Color32::from_rgb(44, 58, 82));
        } else if resp.hovered() {
            p.rect_filled(r.shrink2(egui::vec2(0.0, 1.0)), 0.0, Color32::from_gray(40));
        }
        let entries = app.store.entries_shown_on(d, now);
        let name_col = if d == today { Color32::from_rgb(240, 200, 90) } else { Color32::from_gray(230) };
        let x = r.left() + 8.0;
        p.text(
            egui::pos2(x, r.top() + 6.0),
            Align2::LEFT_TOP,
            WEEKDAY_LONG[i as usize],
            FontId::proportional(big),
            name_col,
        );
        p.text(
            // on the name's baseline
            egui::pos2(x + name_w + 10.0, r.top() + 6.0 + (big - TEXT) * 0.8),
            Align2::LEFT_TOP,
            d.format("%d.%m.").to_string(),
            FontId::proportional(TEXT),
            Color32::from_gray(140),
        );
        let total = entries.len();
        let done = entries.iter().filter(|e| e.status == EntryStatus::Done).count();
        let failed = entries.iter().filter(|e| e.status == EntryStatus::Failed).count();
        let (green, red) = (Color32::from_rgb(110, 170, 120), Color32::from_rgb(215, 95, 85));
        // On the line of the day's name (not the row's center, which is lower than the
        // name in rows without a second line).
        p.text(
            egui::pos2(r.right() - 10.0, r.top() + 6.0 + big * 0.6),
            Align2::RIGHT_CENTER,
            total.to_string(),
            FontId::proportional((row_h * 0.38).clamp(16.0, 24.0)),
            if total == 0 {
                Color32::from_gray(90)
            } else if done == total {
                green
            } else if done + failed == total {
                red
            } else {
                Color32::WHITE
            },
        );
        if let Some(e) = next_task(&entries, d) {
            let txt = format!("{}  {}", fmt_hm(e.start_min), e.title);
            let g = elided(ui, &txt, FontId::proportional(TEXT), Color32::from_gray(160), r.width() - 60.0);
            let y = r.top() + 8.0 + big + 4.0;
            if y + TEXT < r.bottom() {
                p.circle_filled(egui::pos2(x + 3.0, y + 7.0), 3.0, e.priority.color());
                p.galley(egui::pos2(x + 11.0, y), g, Color32::from_gray(160));
            }
        } else if total > 0 {
            let y = r.top() + 8.0 + big + 4.0;
            let (text, color) =
                if failed == 0 { ("all done ✔".to_string(), green) } else { (format!("{failed} failed ✖"), red) };
            p.text(egui::pos2(x, y), Align2::LEFT_TOP, text, FontId::proportional(TEXT), color);
        }
        p.hline(rect.x_range(), r.bottom(), Stroke::new(1.0, GRID_LINE));
        if resp.clicked() {
            app.selected = d;
            app.month = first_of_month(d);
        }
    }
}

// ============================================================================ day view

const TIMELINE_W: f32 = 64.0;
const CONNECTOR_W: f32 = 70.0;
const HEADER_H: f32 = 44.0;

pub fn day_view(app: &mut TaskerApp, ui: &mut Ui) {
    let rect = ui.max_rect();
    let tl = Rect::from_min_max(rect.min, egui::pos2(rect.left() + TIMELINE_W, rect.bottom()));
    let gap =
        Rect::from_min_max(egui::pos2(tl.right(), rect.top()), egui::pos2(tl.right() + CONNECTOR_W, rect.bottom()));
    let list = Rect::from_min_max(egui::pos2(gap.right(), rect.top()), rect.max);
    let header = Rect::from_min_max(list.min, egui::pos2(list.right(), list.top() + HEADER_H));
    let body = Rect::from_min_max(egui::pos2(list.left(), header.bottom()), list.max);

    let now = now_ts();
    let day = app.selected;
    // Includes the entries of the day before that reach past midnight: they are on both days.
    let entries = app.store.entries_shown_on(day, now);

    // ---- header
    let p = ui.painter().clone();
    p.rect_filled(header, 0.0, Color32::from_gray(32));
    let title = p.text(
        egui::pos2(header.left() + 14.0, header.center().y),
        Align2::LEFT_CENTER,
        app.selected.format("%A, %d %B %Y").to_string(),
        FontId::proportional(HEADLINE),
        Color32::WHITE,
    );
    let done = entries.iter().filter(|e| e.status == EntryStatus::Done).count();
    let failed = entries.iter().filter(|e| e.status == EntryStatus::Failed).count();
    // Planned: only what lies on this day (the rest of a task that crosses midnight counts
    // for the other day). Tracked time can't be split like that; it counts for the task's own day.
    let est: u32 = entries.iter().map(|e| e.span_on(day)).map(|(start, end)| end - start).sum();
    let tracked: i64 = entries.iter().filter(|e| e.date == day).map(|e| e.tracked_secs).sum();
    let mut short = format!("{} tasks · {} done", entries.len(), done);
    if failed > 0 {
        short.push_str(&format!(" · {failed} failed"));
    }
    let long =
        format!("{short} · planned {} · tracked {}", fmt_duration_min(est), fmt_duration_min((tracked / 60) as u32));
    // drop the details when the window is too narrow for them
    let room = header.right() - 14.0 - (title.right() + 20.0);
    let layout = |t: String| ui.fonts_mut(|f| f.layout_no_wrap(t, FontId::proportional(TEXT), Color32::from_gray(160)));
    let summary = Some(layout(long))
        .filter(|g| g.size().x <= room)
        .or_else(|| Some(layout(short)).filter(|g| g.size().x <= room));
    if let Some(g) = summary {
        p.galley(
            egui::pos2(header.right() - 14.0 - g.size().x, header.center().y - g.size().y / 2.0),
            g,
            Color32::from_gray(160),
        );
    }
    p.hline(list.x_range(), header.bottom(), Stroke::new(1.0, GRID_LINE));

    // ---- task list
    let mut rows: Vec<Rect> = Vec::with_capacity(entries.len());
    let mut action: Option<Action> = None;
    let mut list_ui = ui.new_child(UiBuilder::new().max_rect(body).layout(Layout::top_down(Align::Min)));
    let viewport = ScrollArea::vertical()
        .id_salt(("tasklist", app.selected))
        .auto_shrink(false)
        .show(&mut list_ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if entries.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("No tasks on this day").size(TEXT).color(Color32::from_gray(140)));
                    ui.label(RichText::new("Use “+ Add task” to plan something.").color(Color32::from_gray(110)));
                });
            }
            for e in &entries {
                let undo_secs = e.run_id.and_then(|id| app.undo_secs_left(id));
                let (r, a) = task_row(ui, e, day, now, snoozed_until(&app.store, e), undo_secs);
                ui.painter().hline(ui.max_rect().x_range(), r.bottom(), Stroke::new(1.0, GRID_LINE));
                rows.push(r);
                if a.is_some() {
                    action = a;
                }
            }
        })
        .inner_rect;

    // ---- timeline
    timeline(ui, tl, &entries, day);

    // ---- connectors
    connectors(ui, tl, gap, viewport, &entries, day, &rows);

    if let Some(a) = action {
        app.apply_click(a);
    }
}

fn minute_y(tl: Rect, min: u32) -> f32 {
    tl.top() + tl.height() * (min as f32 / (24.0 * 60.0))
}

/// The 24 h ruler of `day` with each entry's planned time on it: for a task that crosses
/// midnight, the part that lies on this day.
fn timeline(ui: &Ui, tl: Rect, entries: &[Entry], day: NaiveDate) {
    let p = ui.painter_at(tl.expand2(egui::vec2(0.0, 1.0)));
    p.rect_filled(tl, 0.0, Color32::from_gray(26));
    let hour_h = tl.height() / 24.0;
    let label_every = if hour_h >= 16.0 {
        1
    } else if hour_h >= 9.0 {
        2
    } else {
        3
    };
    for h in 0..=24u32 {
        let y = minute_y(tl, h * 60);
        let major = h % 6 == 0;
        let col = if major { Color32::from_gray(85) } else { Color32::from_gray(55) };
        p.hline(egui::Rangef::new(tl.left() + if major { 0.0 } else { 34.0 }, tl.right()), y, Stroke::new(1.0, col));
        if h < 24 && h % label_every == 0 {
            p.text(
                egui::pos2(tl.left() + 5.0, y + 1.0),
                Align2::LEFT_TOP,
                format!("{h:02}:00"),
                FontId::monospace(10.0),
                Color32::from_gray(if major { 190 } else { 140 }),
            );
        }
    }
    // task spans, packed into lanes so overlaps stay visible
    let mut lane_end: Vec<u32> = Vec::new();
    for e in entries {
        let (start, end) = e.span_on(day);
        let lane = lane_end.iter().position(|&lane_end| lane_end <= start).unwrap_or_else(|| {
            lane_end.push(0);
            lane_end.len() - 1
        });
        lane_end[lane] = end.max(start + 1);
        let x = tl.right() - 6.0 - lane as f32 * 6.0;
        let (y0, y1) = (minute_y(tl, start), minute_y(tl, end).max(minute_y(tl, start) + 2.0));
        let col = if e.status.is_closed() { e.priority.color().gamma_multiply(0.35) } else { e.priority.color() };
        p.rect_filled(Rect::from_x_y_ranges(x - 2.0..=x + 2.0, y0..=y1), 2.0, col);
    }
    if day == today() {
        let y = minute_y(tl, now_min());
        p.hline(tl.x_range(), y, Stroke::new(1.5, Color32::from_rgb(235, 80, 80)));
        p.circle_filled(egui::pos2(tl.left() + 3.0, y), 3.0, Color32::from_rgb(235, 80, 80));
    }
}

fn s_curve(a: Pos2, b: Pos2, n: usize) -> Vec<Pos2> {
    let mid = (a.x + b.x) / 2.0;
    let (c1, c2) = (egui::pos2(mid, a.y), egui::pos2(mid, b.y));
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let x = u * u * u * a.x + 3.0 * u * u * t * c1.x + 3.0 * u * t * t * c2.x + t * t * t * b.x;
            let y = u * u * u * a.y + 3.0 * u * u * t * c1.y + 3.0 * u * t * t * c2.y + t * t * t * b.y;
            egui::pos2(x, y)
        })
        .collect()
}

/// Join each task row's top/bottom divider to its start/end time on the timeline of `day`
/// (midnight where the task goes on before or after this day).
fn connectors(ui: &Ui, tl: Rect, gap: Rect, viewport: Rect, entries: &[Entry], day: NaiveDate, rows: &[Rect]) {
    let clip = Rect::from_min_max(gap.min, egui::pos2(gap.right() + 1.0, gap.bottom()));
    let p = ui.painter_at(clip);
    let clamp = |y: f32| y.clamp(viewport.top(), viewport.bottom());
    for (e, r) in entries.iter().zip(rows) {
        if r.bottom() < viewport.top() - 400.0 || r.top() > viewport.bottom() + 400.0 {
            continue;
        }
        let (start, end) = e.span_on(day);
        let (t0, t1) = (minute_y(tl, start), minute_y(tl, end));
        let (r0, r1) = (clamp(r.top()), clamp(r.bottom()));
        let top = s_curve(egui::pos2(gap.left(), t0), egui::pos2(gap.right(), r0), 24);
        let bot = s_curve(egui::pos2(gap.left(), t1), egui::pos2(gap.right(), r1), 24);
        let done = e.status.is_closed();
        let fill = e.priority.color().gamma_multiply(if done { 0.07 } else { 0.16 });
        let mut mesh = Mesh::default();
        for (a, b) in top.iter().zip(&bot) {
            mesh.colored_vertex(*a, fill);
            mesh.colored_vertex(*b, fill);
        }
        for i in 0..top.len() as u32 - 1 {
            let k = i * 2;
            mesh.add_triangle(k, k + 1, k + 2);
            mesh.add_triangle(k + 1, k + 3, k + 2);
        }
        p.add(mesh);
        let line = Stroke::new(1.0, Color32::from_gray(if done { 60 } else { 95 }));
        p.line(top, line);
        p.line(bot, line);
        // ticks on the timeline edge
        let tick = Stroke::new(2.0, e.priority.color().gamma_multiply(if done { 0.4 } else { 1.0 }));
        p.line_segment([egui::pos2(gap.left(), t0), egui::pos2(gap.left() + 5.0, t0)], tick);
        p.line_segment([egui::pos2(gap.left(), t1), egui::pos2(gap.left() + 5.0, t1)], tick);
    }
}

fn status_chip(ui: &mut Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color.gamma_multiply(0.25))
        .stroke(Stroke::new(1.0, color))
        .corner_radius(8)
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(TEXT).color(color));
        });
}

/// One task in the list of `day`. Takes as much height as its content needs. Done tasks
/// are faded; a task that was given up has a dark red background. `undo_secs`: it was
/// started or given up here a moment ago and has an Undo button for that many more seconds.
fn task_row(
    ui: &mut Ui,
    e: &Entry,
    day: NaiveDate,
    now: i64,
    snoozed: Option<i64>,
    undo_secs: Option<u64>,
) -> (Rect, Option<Action>) {
    let mut action = None;
    let failed = e.status == EntryStatus::Failed;
    let done = e.status.is_closed();
    let fill = if failed { FAILED_RED } else { Color32::TRANSPARENT };
    let margin = Margin { left: 16, right: 12, top: 9, bottom: 10 };
    let resp = Frame::new().fill(fill).inner_margin(margin).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if done {
            ui.multiply_opacity(if failed { 0.8 } else { 0.5 });
        }
        ui.spacing_mut().item_spacing.y = 5.0;
        // title line, as high as its ✏ button whether that is shown or not
        ui.horizontal(|ui| {
            ui.set_min_height(action_height(ui));
            ui.label(RichText::new(&e.title).size(HEADLINE).strong().color(Color32::WHITE));
            match e.status {
                EntryStatus::Virtual => status_chip(ui, "planned", Color32::from_gray(160)),
                EntryStatus::Running => status_chip(ui, "running", Color32::from_rgb(110, 200, 130)),
                EntryStatus::Paused => status_chip(ui, "paused", Color32::from_rgb(230, 180, 80)),
                EntryStatus::Done => status_chip(ui, "done", Color32::from_rgb(140, 170, 150)),
                EntryStatus::Failed => status_chip(ui, "failed", Color32::from_rgb(240, 120, 110)),
            }
            if e.recurring {
                ui.label(RichText::new("⟳").color(Color32::from_gray(160))).on_hover_text("Recurring task");
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // A started one-off task has nothing left to edit (its data is fixed now).
                if e.task_exists && (e.recurring || e.run_id.is_none()) {
                    let tip = if e.run_id.is_some() {
                        "Edit task (changes upcoming occurrences; this started one keeps its data)"
                    } else {
                        "Edit task"
                    };
                    if icon_button(ui, "✏").on_hover_text(tip).clicked() {
                        action = Some(Action::Edit(e.task_id));
                    }
                    if e.recurring && action_button(ui, "📊 Statistics", Act::Plain, true).clicked() {
                        action = Some(Action::Stats(e.task_id));
                    }
                }
            });
        });
        // meta line
        let mut meta = format!(
            "{}   ·   est. {}   ·   {} priority   ·   {} {}",
            e.time_span_on(day),
            fmt_duration_min(e.estimate_min),
            e.priority.label(),
            e.notify.icon(),
            match e.notify {
                NotifyMode::Off => "no reminder",
                NotifyMode::Sound => "reminder",
                NotifyMode::Silent => "silent reminder",
            }
        );
        if e.notify_ahead_min > 0 {
            meta.push_str(&format!("   ·   ⏳ notice {} ahead", fmt_duration_min(e.notify_ahead_min)));
        }
        if !e.schedule_desc.is_empty() && e.recurring {
            meta.push_str(&format!("   ·   {}", e.schedule_desc));
        }
        ui.label(RichText::new(meta).size(TEXT).color(Color32::from_gray(170)));
        if !e.notes.trim().is_empty() {
            ui.label(RichText::new(e.notes.trim()).size(TEXT).color(Color32::from_gray(200)));
        }
        // tracking
        if e.run_id.is_some() {
            let est = e.estimate_min as i64 * 60;
            let frac = if est > 0 { e.tracked_secs as f32 / est as f32 } else { 0.0 };
            let col = if frac <= 0.9 {
                Color32::from_rgb(90, 170, 110)
            } else if frac <= 1.0 {
                Color32::from_rgb(220, 180, 70)
            } else {
                Color32::from_rgb(220, 90, 80)
            };
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("⏱ {}", fmt_hms(e.tracked_secs)))
                        .monospace()
                        .size(TEXT)
                        .color(Color32::WHITE),
                );
                ui.label(RichText::new(format!("/ {}", fmt_hms(est))).monospace().color(Color32::from_gray(150)));
                let (bar, _) = ui.allocate_exact_size(egui::vec2(160.0, 8.0), Sense::hover());
                let p = ui.painter();
                p.rect_filled(bar, 4.0, Color32::from_gray(55));
                let mut fill = bar;
                fill.set_width(bar.width() * frac.min(1.0));
                p.rect_filled(fill, 4.0, col);
                if est > 0 {
                    let diff = e.tracked_secs - est;
                    let txt = if diff > 0 {
                        format!("{} over", fmt_duration_min((diff / 60) as u32))
                    } else {
                        format!("{}%", (frac * 100.0).round() as i32)
                    };
                    ui.label(RichText::new(txt).size(TEXT).color(col));
                }
            });
            // correct the tracking for work done before pressing Start (while running only)
            if let (EntryStatus::Running, Some(id)) = (e.status, e.run_id) {
                preset_row(ui, |ui| {
                    let gray = Color32::from_gray(150);
                    ui.label(RichText::new("Add earlier work").size(TEXT).color(gray));
                    for (label, secs) in [("+1 min", 60), ("+10 min", 600), ("+1 hour", 3600)] {
                        let tip = "Count time you already worked on this before starting it";
                        if preset_button(ui, false, label).on_hover_text(tip).clicked() {
                            action = Some(Action::AddTime(id, secs));
                        }
                    }
                    if e.added_secs > 0 {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(format!("{} added", fmt_span_min(e.added_secs / 60))).size(TEXT).color(gray),
                        );
                        if preset_button(ui, false, "Reset").on_hover_text("Remove the added time again").clicked() {
                            action = Some(Action::ResetAddedTime(id));
                        }
                    }
                });
            }
        }
        // buttons
        ui.horizontal(|ui| {
            if let Some(a) = task_buttons(ui, e, 0) {
                action = Some(a);
            }
            if let (Some(secs), Some(id)) = (undo_secs, e.run_id) {
                let tip = "Clicked by accident? This takes it back: the task is planned again \
                           (and time tracked since then is dropped)";
                if action_button(ui, format!("↩ Undo ({secs})"), Act::Plain, true).on_hover_text(tip).clicked() {
                    action = Some(Action::Undo(id));
                }
            }
            // how early or late it was first started (resumes don't change that)
            start_offset_labels(ui, e, now, snoozed.filter(|_| !done), false);
        });
    });
    let r = resp.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_size(r.min, egui::vec2(4.0, r.height())),
        0.0,
        e.priority.color().gamma_multiply(if done { 0.4 } else { 1.0 }),
    );
    (r, action)
}
