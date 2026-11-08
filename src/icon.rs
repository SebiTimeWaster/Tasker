//! Placeholder app/tray icon, drawn procedurally: a rounded square with a check mark.

fn seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (vx, vy) = (bx - ax, by - ay);
    let t = (((px - ax) * vx + (py - ay) * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
    let (dx, dy) = (px - ax - t * vx, py - ay - t * vy);
    (dx * dx + dy * dy).sqrt()
}

/// What the icon color shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconState {
    /// Blue: nothing running or paused.
    Idle,
    /// Yellow: a task is paused (and none is running).
    Paused,
    /// Green: a task is running (wins over paused).
    Running,
}

/// RGBA8 pixels, `size`×`size`, tinted by `state`.
pub fn render(size: u32, state: IconState) -> Vec<u8> {
    let s = size as f32;
    let bg = match state {
        IconState::Idle => [70u8, 120, 210],
        IconState::Paused => [215, 160, 30],
        IconState::Running => [60, 170, 100],
    };
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    let r = s * 0.22;
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // rounded rect SDF
            let m = s * 0.06;
            let (hx, hy) = (s / 2.0 - m - r, s / 2.0 - m - r);
            let (qx, qy) = ((px - s / 2.0).abs() - hx, (py - s / 2.0).abs() - hy);
            let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
            let a_box = (0.5 - outside).clamp(0.0, 1.0);
            // check mark
            let w = s * 0.085;
            let d = seg_dist(px, py, s * 0.28, s * 0.52, s * 0.44, s * 0.68)
                .min(seg_dist(px, py, s * 0.44, s * 0.68, s * 0.74, s * 0.33));
            let a_check = (w - d + 0.5).clamp(0.0, 1.0);
            let c = |b: u8| (b as f32 * (1.0 - a_check) + 255.0 * a_check) as u8;
            out.extend_from_slice(&[c(bg[0]), c(bg[1]), c(bg[2]), (a_box * 255.0) as u8]);
        }
    }
    out
}
