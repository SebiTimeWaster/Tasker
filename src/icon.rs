//! Placeholder app/tray icon, drawn procedurally: a rounded square with a check mark.
//! `assets/tasker.ico` (the icon of tasker.exe, see build.rs) is this drawing saved as a
//! file; the test below keeps the two the same.

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
            let d = seg_dist(px, py, s * 0.28, s * 0.52, s * 0.44, s * 0.68).min(seg_dist(
                px,
                py,
                s * 0.44,
                s * 0.68,
                s * 0.74,
                s * 0.33,
            ));
            let a_check = (w - d + 0.5).clamp(0.0, 1.0);
            let c = |b: u8| (b as f32 * (1.0 - a_check) + 255.0 * a_check) as u8;
            out.extend_from_slice(&[c(bg[0]), c(bg[1]), c(bg[2]), (a_box * 255.0) as u8]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A .ico with one 32-bit BMP image per size.
    fn ico_file(sizes: &[u32]) -> Vec<u8> {
        let images: Vec<Vec<u8>> = sizes
            .iter()
            .map(|&s| {
                let rgba = render(s, IconState::Idle);
                let mut b = Vec::new();
                // BITMAPINFOHEADER; the height counts the color and the (unused) AND mask.
                for v in [40u32, s, s * 2] {
                    b.extend(v.to_le_bytes());
                }
                b.extend(1u16.to_le_bytes()); // planes
                b.extend(32u16.to_le_bytes()); // bits per pixel
                b.extend([0u8; 24]); // no compression, sizes / resolutions / palette unused
                // BGRA, bottom-up rows
                for y in (0..s).rev() {
                    for x in 0..s {
                        let p = ((y * s + x) * 4) as usize;
                        b.extend([rgba[p + 2], rgba[p + 1], rgba[p], rgba[p + 3]]);
                    }
                }
                // AND mask: 1 bit per pixel, rows padded to 4 bytes; all zero (alpha decides)
                b.extend(vec![0u8; (s.div_ceil(32) * 4 * s) as usize]);
                b
            })
            .collect();
        let mut ico = Vec::new();
        ico.extend([0u8, 0, 1, 0]); // reserved, type 1 = icon
        ico.extend((sizes.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * sizes.len() as u32;
        for (&s, img) in sizes.iter().zip(&images) {
            let dim = if s >= 256 { 0 } else { s as u8 }; // 0 means 256
            ico.extend([dim, dim, 0, 0]);
            ico.extend(1u16.to_le_bytes()); // planes
            ico.extend(32u16.to_le_bytes()); // bits per pixel
            ico.extend((img.len() as u32).to_le_bytes());
            ico.extend(offset.to_le_bytes());
            offset += img.len() as u32;
        }
        for img in images {
            ico.extend(img);
        }
        ico
    }

    /// After changing the drawing, write the file again with
    /// `TASKER_WRITE_ICON=1 cargo test exe_icon`.
    #[test]
    fn exe_icon_is_the_drawn_icon() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/tasker.ico");
        let ico = ico_file(&[16, 20, 24, 32, 40, 48, 64, 256]);
        if std::env::var_os("TASKER_WRITE_ICON").is_some() {
            std::fs::write(path, &ico).unwrap();
        }
        assert!(std::fs::read(path).unwrap() == ico, "assets/tasker.ico is out of date (see the comment above)");
    }
}
