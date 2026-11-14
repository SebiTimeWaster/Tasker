//! The main window: it has no system decorations, so the title bar and the resize edges
//! are ours. Also showing / hiding it (panel icon), the UI scale, and remembering its
//! geometry and fitting it to the screen.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use chrono::Local;
use egui::{Align, Color32, CursorIcon, Id, Layout, Pos2, RichText, Sense, Ui, ViewportCommand};

use super::TaskerApp;
use crate::dialogs::SettingsDialog;
use crate::settings::{WINDOW_SIZE, WindowGeom, step_scale};

impl TaskerApp {
    pub(super) fn show_window(&mut self, ctx: &egui::Context) {
        self.window_visible = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    pub(super) fn has_tray(&self) -> bool {
        self.tray.is_some() && self.tray_online.load(Ordering::Relaxed)
    }

    /// Minimize to the panel icon (window disappears from the window list).
    /// Without a tray host there would be no way back, so minimize normally instead.
    pub(super) fn hide_window(&mut self, ctx: &egui::Context) {
        if self.has_tray() {
            self.window_visible = false;
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        } else {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
    }

    /// Apply the UI scale (or the one previewed in the settings) and handle Ctrl +/-/0.
    pub(super) fn update_zoom(&mut self, ctx: &egui::Context) {
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
            .map(|(x, y, w, h)| {
                egui::Rect::from_min_size(egui::pos2(x as f32, y as f32), egui::vec2(w as f32, h as f32)) / ppp
            })
            .collect()
    }

    /// Fit the main window to the screen at startup and after a UI scale change: the
    /// minimum size is 1280×800 at 100 % and grows with the UI scale (limited to the
    /// monitor); a window that doesn't fit is shrunk / moved, and one whose title bar is
    /// off-screen (e.g. a monitor was removed) is centered on the primary monitor.
    pub(super) fn fit_window(&mut self, ctx: &egui::Context) {
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

        let fit = fit_to_screen(rect, &self.monitor_areas(ctx), WINDOW_SIZE.into());
        ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(fit.min));
        if first && self.win_geom.is_some_and(|g| g.maximized) && !maximized {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
            return;
        }
        if maximized {
            return;
        }
        let resized = (fit.size - rect.size()).length() > 1.0;
        if resized {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(fit.size));
        }
        if resized || !fit.reachable {
            if (fit.pos - rect.min).length() > 1.0 {
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(fit.pos));
            }
            crate::logfile::info(format!(
                "window fitted to screen: {:.0}x{:.0} at {:.0},{:.0} (points, scale {:.2})",
                fit.size.x, fit.size.y, rect.min.x, rect.min.y, ppp
            ));
        }
    }

    /// Remember the window's position, size and maximized state; saved once it has been
    /// stable for a second (not on every pixel of a drag) and on exit.
    pub(super) fn track_window(&mut self, ctx: &egui::Context) {
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

    pub(super) fn save_window(&mut self) {
        if let Some(g) = self.win_geom.filter(|g| Some(*g) != self.win_saved) {
            let r = self.store.save_window(&g);
            self.report(r);
            self.win_saved = Some(g);
        }
    }

    /// Window edges: our window has no system decorations, so we provide resizing.
    pub(super) fn handle_resize(&mut self, ctx: &egui::Context) -> bool {
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

    pub(super) fn title_bar(&mut self, ui: &mut Ui, resizing: bool) {
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
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [40, 40],
                    &crate::icon::render(40, crate::icon::IconState::Idle),
                );
                let t = ui.ctx().load_texture("app-icon", img, Default::default());
                ui.ctx().data_mut(|d| d.insert_temp(Id::unique("app-icon"), t.clone()));
                t
            });
            ui.painter().image(tex.id(), r, egui::Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
            ui.label(RichText::new("Tasker").strong().size(crate::widgets::HEADLINE));
            ui.label(RichText::new(Local::now().format("%A, %d %B %Y  %H:%M").to_string()).weak());

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let btn = |ui: &mut Ui, s: &str, tip: &str| crate::widgets::icon_button(ui, s).on_hover_text(tip);
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

/// Where the main window belongs, see [`TaskerApp::fit_window`].
#[derive(Debug, PartialEq)]
struct Fit {
    /// Its minimum size: the wanted one, unless the monitor is smaller.
    min: egui::Vec2,
    /// Its size: at least the minimum, at most the monitor.
    size: egui::Vec2,
    /// Its position: where it is, moved just enough to lie on the monitor, or centered on
    /// it when its title bar can't be reached.
    pos: Pos2,
    /// Whether enough of its title bar is on some monitor to grab it there.
    reachable: bool,
}

/// Fit the window `rect` to the monitor showing most of it (else the primary one) out of
/// `areas` (usable areas, primary first), with the minimum size `min`. All in points.
fn fit_to_screen(rect: egui::Rect, areas: &[egui::Rect], min: egui::Vec2) -> Fit {
    let overlap = |a: &egui::Rect| {
        let i = a.intersect(rect);
        if i.is_positive() { i.area() } else { 0.0 }
    };
    let area = areas
        .iter()
        .copied()
        .filter(|a| overlap(a) > 0.0)
        .max_by(|a, b| overlap(a).total_cmp(&overlap(b)))
        .or(areas.first().copied())
        .unwrap_or(rect);
    let min = min.min(area.size());
    let size = rect.size().max(min).min(area.size());
    let title = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 36.0));
    let reachable = areas.iter().any(|a| {
        let i = a.intersect(title);
        i.width() >= 100.0_f32.min(rect.width()) && i.height() >= 10.0
    });
    let pos = if reachable { rect.min } else { area.center() - size / 2.0 };
    let pos = egui::pos2(
        pos.x.clamp(area.left(), (area.right() - size.x).max(area.left())),
        pos.y.clamp(area.top(), (area.bottom() - size.y).max(area.top())),
    );
    Fit { min, size, pos, reachable }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Rect, pos2, vec2};

    const MIN: egui::Vec2 = vec2(1280.0, 800.0);

    fn monitor(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    }

    #[test]
    fn a_window_that_fits_stays() {
        let screen = [monitor(0.0, 0.0, 1920.0, 1050.0)];
        let fit = fit_to_screen(monitor(100.0, 50.0, 1400.0, 900.0), &screen, MIN);
        assert_eq!(fit, Fit { min: MIN, size: vec2(1400.0, 900.0), pos: pos2(100.0, 50.0), reachable: true });
    }

    #[test]
    fn too_small_and_too_big_windows_are_resized() {
        let screen = [monitor(0.0, 0.0, 1920.0, 1050.0)];
        assert_eq!(fit_to_screen(monitor(0.0, 0.0, 600.0, 400.0), &screen, MIN).size, MIN, "grows to the minimum");
        let big = fit_to_screen(monitor(300.0, 200.0, 3000.0, 2000.0), &screen, MIN);
        assert_eq!(big.size, vec2(1920.0, 1050.0), "shrinks to the monitor");
        assert_eq!(big.pos, pos2(0.0, 0.0), "and moves onto it");
        // a monitor smaller than the minimum size: the minimum is the monitor
        let laptop = [monitor(0.0, 0.0, 1024.0, 740.0)];
        let fit = fit_to_screen(monitor(0.0, 0.0, 1280.0, 800.0), &laptop, MIN);
        assert_eq!((fit.min, fit.size), (vec2(1024.0, 740.0), vec2(1024.0, 740.0)));
    }

    #[test]
    fn a_window_on_a_removed_monitor_is_centered_on_the_primary_one() {
        // the second monitor (right of the primary one) is gone
        let screen = [monitor(0.0, 0.0, 1920.0, 1050.0)];
        let fit = fit_to_screen(monitor(2200.0, 100.0, 1280.0, 800.0), &screen, MIN);
        assert!(!fit.reachable);
        assert_eq!(fit.pos, pos2(320.0, 125.0));
    }

    #[test]
    fn a_window_partly_off_screen_is_moved_onto_its_monitor() {
        let screen = [monitor(0.0, 0.0, 1920.0, 1050.0), monitor(1920.0, 0.0, 2560.0, 1400.0)];
        // mostly on the second monitor, its bottom below the screen
        let fit = fit_to_screen(monitor(2000.0, 900.0, 1400.0, 900.0), &screen, MIN);
        assert!(fit.reachable);
        assert_eq!(fit.pos, pos2(2000.0, 500.0), "up, onto the monitor showing most of it");
        // the title bar above the top of the screen can't be grabbed
        let fit = fit_to_screen(monitor(300.0, -500.0, 1300.0, 900.0), &screen, MIN);
        assert!(!fit.reachable);
        assert_eq!(fit.pos, pos2(310.0, 75.0), "centered");
    }
}
