use serde::{Deserialize, Serialize};

use crate::model::{NotifyMode, Priority};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SoundChoice {
    Chime,
    Bell,
    Beep,
    File,
}

impl SoundChoice {
    pub const ALL: [SoundChoice; 4] = [SoundChoice::Chime, SoundChoice::Bell, SoundChoice::Beep, SoundChoice::File];
    pub fn label(self) -> &'static str {
        match self {
            SoundChoice::Chime => "Chime (built-in)",
            SoundChoice::Bell => "Bell (built-in)",
            SoundChoice::Beep => "Beep (built-in)",
            SoundChoice::File => "Custom file…",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub corner: Corner,
    /// Preset buttons for a task's start time in the editor (minutes of the day).
    pub start_presets: Vec<u32>,
    /// Preset buttons for a task's estimate and its "notify ahead" time in the editor
    /// (minutes); for "notify ahead" an "Off" button comes before them anyway.
    pub time_presets: Vec<u32>,
    /// What a new task starts with in the editor.
    pub new_task: NewTaskDefaults,
    /// Whether a "starts soon" notice stays until it is dismissed, instead of closing on
    /// its own after `ahead_close_secs`.
    pub ahead_stays: bool,
    pub ahead_close_secs: u32,
    pub sound: SoundChoice,
    pub sound_file: String,
    /// Seconds to ramp from `start_volume` to `end_volume`.
    pub ramp_secs: f32,
    /// 0.0 ..= 1.0
    pub start_volume: f32,
    /// 0.0 ..= 1.0
    pub end_volume: f32,
    pub close_to_tray: bool,
    pub start_minimized: bool,
    /// Zoom of the whole UI (1.0 = 100 %). Ctrl +/- / Ctrl 0 change it too.
    pub ui_scale: f32,
}

/// The values of a new task in the editor (the start time is the next full hour).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NewTaskDefaults {
    pub estimate_min: u32,
    pub priority: Priority,
    pub notify: NotifyMode,
    pub notify_ahead_min: u32,
}

impl Default for NewTaskDefaults {
    fn default() -> Self {
        NewTaskDefaults { estimate_min: 30, priority: Priority::Normal, notify: NotifyMode::Sound, notify_ahead_min: 0 }
    }
}

/// Allowed time (seconds) for a "starts soon" notice to close on its own.
pub const AHEAD_CLOSE_SECS: std::ops::RangeInclusive<u32> = 1..=300;

/// Main window size at 100 % UI scale: the size on first start and the minimum size.
/// Both grow with the UI scale (limited to the screen).
pub const WINDOW_SIZE: [f32; 2] = [1280.0, 800.0];

/// The main window's position and size, remembered between runs. In logical pixels
/// (physical pixels / display scale), so independent of Tasker's own UI scale. Position
/// and size are those of the normal (not maximized) window.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowGeom {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub maximized: bool,
}

/// Allowed UI scale range in percent (settings slider).
pub const UI_SCALE_PCT: std::ops::RangeInclusive<u32> = 50..=200;

/// Steps for Ctrl + / Ctrl −.
const ZOOM_STEPS: [f32; 11] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];

/// The next zoom step above (`up`) or below the current scale.
pub fn step_scale(cur: f32, up: bool) -> f32 {
    let next = if up {
        ZOOM_STEPS.iter().copied().find(|&f| f > cur + 0.001)
    } else {
        ZOOM_STEPS.iter().rev().copied().find(|&f| f < cur - 0.001)
    };
    next.unwrap_or(cur)
}

/// A UI scale from the settings, limited to the allowed range.
pub fn clamp_scale(f: f32) -> f32 {
    let (lo, hi) = (*UI_SCALE_PCT.start() as f32 / 100.0, *UI_SCALE_PCT.end() as f32 / 100.0);
    if f.is_finite() { f.clamp(lo, hi) } else { 1.0 }
}

/// The snooze times a due reminder offers (minutes).
pub const SNOOZE_MINUTES: [u32; 5] = [5, 10, 30, 60, 120];

/// 08:00 to 20:00 every two hours.
pub const DEFAULT_START_PRESETS: [u32; 7] = [8 * 60, 10 * 60, 12 * 60, 14 * 60, 16 * 60, 18 * 60, 20 * 60];
pub const DEFAULT_TIME_PRESETS: [u32; 8] = [5, 15, 30, 60, 2 * 60, 4 * 60, 8 * 60, 24 * 60];
/// The longest time preset (minutes).
pub const MAX_DURATION_MIN: u32 = 24 * 60;

impl Default for Settings {
    fn default() -> Self {
        Settings {
            corner: Corner::BottomRight,
            start_presets: DEFAULT_START_PRESETS.to_vec(),
            time_presets: DEFAULT_TIME_PRESETS.to_vec(),
            new_task: NewTaskDefaults::default(),
            ahead_stays: false,
            ahead_close_secs: 10,
            sound: SoundChoice::Chime,
            sound_file: String::new(),
            ramp_secs: 30.0,
            start_volume: 0.2,
            end_volume: 1.0,
            close_to_tray: false,
            start_minimized: false,
            ui_scale: 1.0,
        }
    }
}

pub fn fmt_snooze(min: u32) -> String {
    if min >= 60 && min.is_multiple_of(60) { format!("{} h", min / 60) } else { format!("{min} min") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_steps() {
        assert_eq!(step_scale(1.0, true), 1.1);
        assert_eq!(step_scale(1.0, false), 0.9);
        assert_eq!(step_scale(1.37, true), 1.5, "from a slider value to the next step");
        assert_eq!(step_scale(1.37, false), 1.25);
        assert_eq!(step_scale(2.0, true), 2.0, "stops at the maximum");
        assert_eq!(step_scale(0.5, false), 0.5, "stops at the minimum");
    }

    #[test]
    fn scale_is_limited() {
        assert_eq!(clamp_scale(4.0), 2.0);
        assert_eq!(clamp_scale(0.1), 0.5);
        assert_eq!(clamp_scale(1.37), 1.37);
        assert_eq!(clamp_scale(f32::NAN), 1.0);
    }

    #[test]
    fn snooze_labels() {
        assert_eq!(fmt_snooze(5), "5 min");
        assert_eq!(fmt_snooze(90), "90 min");
        assert_eq!(fmt_snooze(120), "2 h");
    }

    #[test]
    fn older_settings_still_load() {
        // Saved by an older version: fields added later get their defaults.
        // (snooze_minutes is no longer a setting and is ignored)
        let s: Settings = serde_json::from_str(r#"{"corner":"TopLeft","snooze_minutes":[15],"sound":"Bell"}"#).unwrap();
        assert_eq!(s.corner, Corner::TopLeft);
        assert_eq!(s.start_presets, DEFAULT_START_PRESETS);
        assert_eq!(s.time_presets, DEFAULT_TIME_PRESETS);
        assert_eq!(s.new_task, NewTaskDefaults::default());
        assert_eq!((s.ahead_stays, s.ahead_close_secs), (false, 10));
        assert_eq!(s.sound, SoundChoice::Bell);
        assert_eq!(s.ui_scale, 1.0);
        assert_eq!(s.ramp_secs, Settings::default().ramp_secs);
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
