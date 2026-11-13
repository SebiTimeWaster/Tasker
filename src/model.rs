//! Core data model: virtual tasks (definitions + schedules), real runs, and the
//! per-day "entries" the UI shows (a mix of both).

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};

pub const WEEKDAY_SHORT: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
pub const WEEKDAY_LONG: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// Repetition of a single-date schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    Never,
    /// Legacy (older databases); converted to a weekday schedule on load.
    Weekly,
    /// Legacy (older databases); converted to a day-of-month schedule on load.
    Monthly,
    Yearly,
}

impl Repeat {
    /// The choices offered for a single date.
    pub const CHOICES: [Repeat; 2] = [Repeat::Never, Repeat::Yearly];
    pub fn label(self) -> &'static str {
        match self {
            Repeat::Never => "Once",
            Repeat::Weekly => "Weekly",
            Repeat::Monthly => "Monthly",
            Repeat::Yearly => "Every year",
        }
    }
}

/// Which occurrences of the selected weekdays count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WeekRepeat {
    #[default]
    Weekly,
    /// Every second week, counted from the week of the start date.
    BiWeekly,
    /// Only the first of each selected weekday in a month (e.g. first Monday).
    FirstInMonth,
    /// Only the last of each selected weekday in a month (e.g. last Friday).
    LastInMonth,
}

impl WeekRepeat {
    pub const ALL: [WeekRepeat; 4] =
        [WeekRepeat::Weekly, WeekRepeat::BiWeekly, WeekRepeat::FirstInMonth, WeekRepeat::LastInMonth];
    pub fn label(self) -> &'static str {
        match self {
            WeekRepeat::Weekly => "Every week",
            WeekRepeat::BiWeekly => "Every 2 weeks",
            WeekRepeat::FirstInMonth => "First in month",
            WeekRepeat::LastInMonth => "Last in month",
        }
    }
}

/// When a task is active.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Schedule {
    /// A single date, optionally repeating weekly/monthly/yearly from that date on.
    Date { date: NaiveDate, repeat: Repeat },
    /// Selected weekdays (bit 0 = Monday) starting at `from`, filtered by `every`.
    Weekdays {
        mask: u8,
        from: NaiveDate,
        #[serde(default)]
        every: WeekRepeat,
    },
    /// Selected days of the month (bit n = day n, 1..=31), every month starting at `from`.
    /// Months that lack a selected day (e.g. the 31st) are skipped for that day.
    MonthDays { mask: u32, from: NaiveDate },
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    (NaiveDate::from_ymd_opt(ny, nm, 1).unwrap() - Duration::days(1)).day()
}

pub fn weekday_index(d: NaiveDate) -> usize {
    d.weekday().num_days_from_monday() as usize
}

impl Schedule {
    pub fn occurs_on(&self, d: NaiveDate) -> bool {
        match *self {
            Schedule::Date { date, repeat } => {
                if d < date {
                    return false;
                }
                match repeat {
                    Repeat::Never => d == date,
                    Repeat::Weekly => d.weekday() == date.weekday(),
                    Repeat::Monthly => {
                        // Clamp e.g. the 31st to the last day of shorter months.
                        let dim = days_in_month(d.year(), d.month());
                        d.day() == date.day().min(dim)
                    }
                    Repeat::Yearly => {
                        if d.month() != date.month() {
                            return false;
                        }
                        let dim = days_in_month(d.year(), d.month());
                        d.day() == date.day().min(dim)
                    }
                }
            }
            Schedule::Weekdays { mask, from, every } => {
                if d < from || mask & (1 << weekday_index(d)) == 0 {
                    return false;
                }
                match every {
                    WeekRepeat::Weekly => true,
                    WeekRepeat::BiWeekly => ((monday_of(d) - monday_of(from)).num_days() / 7) % 2 == 0,
                    WeekRepeat::FirstInMonth => d.day() <= 7,
                    WeekRepeat::LastInMonth => d.day() + 7 > days_in_month(d.year(), d.month()),
                }
            }
            Schedule::MonthDays { mask, from } => d >= from && mask & (1 << d.day()) != 0,
        }
    }

    /// Converts schedules from older versions (weekly/monthly single dates).
    pub fn normalized(self) -> Schedule {
        match self {
            Schedule::Date { date, repeat: Repeat::Weekly } => {
                Schedule::Weekdays { mask: 1 << weekday_index(date), from: date, every: WeekRepeat::Weekly }
            }
            Schedule::Date { date, repeat: Repeat::Monthly } => {
                Schedule::MonthDays { mask: 1 << date.day(), from: date }
            }
            s => s,
        }
    }

    pub fn is_recurring(&self) -> bool {
        !matches!(self, Schedule::Date { repeat: Repeat::Never, .. })
    }

    /// The first day this schedule could occur on.
    pub fn first_date(&self) -> NaiveDate {
        match *self {
            Schedule::Date { date, .. } => date,
            Schedule::Weekdays { from, .. } | Schedule::MonthDays { from, .. } => from,
        }
    }

    pub fn describe(&self) -> String {
        match *self {
            Schedule::Date { date, repeat: Repeat::Never } => date.format("%a %d.%m.%Y").to_string(),
            Schedule::Date { date, repeat: Repeat::Weekly } => {
                format!("Weekly on {}", WEEKDAY_SHORT[weekday_index(date)])
            }
            Schedule::Date { date, repeat: Repeat::Monthly } => format!("Monthly on the {}.", date.day()),
            Schedule::Date { date, repeat: Repeat::Yearly } => format!("Yearly on {}", date.format("%d.%m.")),
            Schedule::Weekdays { mask, every, .. } => {
                let days = || -> String {
                    let d: Vec<&str> = (0..7).filter(|i| mask & (1 << i) != 0).map(|i| WEEKDAY_SHORT[i]).collect();
                    d.join(", ")
                };
                match every {
                    WeekRepeat::Weekly if mask == 0b111_1111 => "Every day".into(),
                    WeekRepeat::Weekly if mask == 0b001_1111 => "Weekdays (Mon–Fri)".into(),
                    WeekRepeat::Weekly => format!("Every {}", days()),
                    WeekRepeat::BiWeekly => format!("Every other week on {}", days()),
                    WeekRepeat::FirstInMonth => format!("First {} of the month", days()),
                    WeekRepeat::LastInMonth => format!("Last {} of the month", days()),
                }
            }
            Schedule::MonthDays { mask, .. } => {
                let days: Vec<String> = (1..=31).filter(|i| mask & (1 << i) != 0).map(|i| format!("{i}.")).collect();
                format!("Monthly on the {}", days.join(", "))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Priority {
    Low = 0,
    Normal = 1,
    High = 2,
    Urgent = 3,
}

impl Priority {
    pub const ALL: [Priority; 4] = [Priority::Low, Priority::Normal, Priority::High, Priority::Urgent];
    pub fn from_i64(v: i64) -> Self {
        match v {
            0 => Priority::Low,
            2 => Priority::High,
            3 => Priority::Urgent,
            _ => Priority::Normal,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Priority::Low => "Low",
            Priority::Normal => "Normal",
            Priority::High => "High",
            Priority::Urgent => "Urgent",
        }
    }
    pub fn color(self) -> egui::Color32 {
        match self {
            Priority::Low => egui::Color32::from_rgb(110, 150, 120),
            Priority::Normal => egui::Color32::from_rgb(90, 140, 210),
            Priority::High => egui::Color32::from_rgb(225, 160, 60),
            Priority::Urgent => egui::Color32::from_rgb(220, 70, 70),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotifyMode {
    Off = 0,
    Sound = 1,
    Silent = 2,
}

impl NotifyMode {
    pub const ALL: [NotifyMode; 3] = [NotifyMode::Sound, NotifyMode::Silent, NotifyMode::Off];
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => NotifyMode::Sound,
            2 => NotifyMode::Silent,
            _ => NotifyMode::Off,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            NotifyMode::Off => "No",
            NotifyMode::Sound => "Yes",
            NotifyMode::Silent => "Yes, without sound",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            NotifyMode::Off => "🔕",
            NotifyMode::Sound => "🔔",
            NotifyMode::Silent => "🔈",
        }
    }
}

/// A virtual task: the user-editable definition. Never "done" by itself.
#[derive(Clone, Debug)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub notes: String,
    pub schedule: Schedule,
    /// Minutes after midnight.
    pub start_min: u32,
    pub estimate_min: u32,
    pub priority: Priority,
    pub notify: NotifyMode,
    /// Minutes before the start to show a short announcement (0 = off).
    pub notify_ahead_min: u32,
    pub created_at: i64,
    pub deleted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Running = 0,
    Paused = 1,
    Done = 2,
    /// Given up ("I am bad and won't do it") without working on it.
    Failed = 3,
}

impl RunStatus {
    pub fn from_i64(v: i64) -> Self {
        match v {
            0 => RunStatus::Running,
            2 => RunStatus::Done,
            3 => RunStatus::Failed,
            _ => RunStatus::Paused,
        }
    }
    /// Still being worked on: running or paused (neither done nor given up).
    pub fn is_open(self) -> bool {
        matches!(self, RunStatus::Running | RunStatus::Paused)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub id: i64,
    pub start: i64,
    pub end: Option<i64>,
}

/// A real task: created when the user starts an occurrence of a virtual task.
/// Holds a snapshot of the task definition so later edits don't change history.
#[derive(Clone, Debug)]
pub struct Run {
    pub id: i64,
    pub task_id: i64,
    pub date: NaiveDate,
    pub title: String,
    pub notes: String,
    pub start_min: u32,
    pub estimate_min: u32,
    pub priority: Priority,
    pub notify: NotifyMode,
    pub recurring: bool,
    pub status: RunStatus,
    #[allow(dead_code)]
    pub created_at: i64,
    pub finished_at: Option<i64>,
    /// Tracked time added by hand (work done before pressing Start).
    pub added_secs: i64,
    pub segments: Vec<Segment>,
}

impl Run {
    pub fn tracked_secs(&self, now: i64) -> i64 {
        self.segments.iter().map(|s| (s.end.unwrap_or(now) - s.start).max(0)).sum::<i64>() + self.added_secs
    }
    /// When work on it began: the first time segment, moved earlier by any added time.
    pub fn first_start(&self) -> Option<i64> {
        self.segments.first().map(|s| s.start - self.added_secs)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryStatus {
    Virtual,
    Running,
    Paused,
    Done,
    Failed,
}

impl EntryStatus {
    /// Nothing left to do: done or given up.
    pub fn is_closed(self) -> bool {
        matches!(self, EntryStatus::Done | EntryStatus::Failed)
    }
}

/// Minutes of a day.
pub const DAY_MIN: u32 = 24 * 60;
/// The longest estimate: just under two days.
pub const MAX_ESTIMATE_MIN: u32 = 48 * 60 - 1;
/// How many days after its own day an entry can still reach (a late start plus the
/// longest estimate); those days show it too.
pub const MAX_DAYS_REACHED: u32 = 2;

/// What the UI shows for one task on one day: a virtual occurrence or a real run.
#[derive(Clone, Debug)]
pub struct Entry {
    pub task_id: i64,
    pub run_id: Option<i64>,
    pub date: NaiveDate,
    pub title: String,
    pub notes: String,
    pub start_min: u32,
    pub estimate_min: u32,
    pub priority: Priority,
    pub notify: NotifyMode,
    /// Minutes before the start to announce the task (0 = off).
    pub notify_ahead_min: u32,
    pub recurring: bool,
    pub schedule_desc: String,
    pub status: EntryStatus,
    pub tracked_secs: i64,
    /// When work began, including added earlier work (real tasks only).
    pub first_start: Option<i64>,
    /// Tracked time added by hand (included in `tracked_secs`).
    pub added_secs: i64,
    /// Whether the virtual task definition still exists (for edit / stats buttons).
    pub task_exists: bool,
}

impl Entry {
    /// Whether the planned time reaches into the day `days` days after its own (which then
    /// shows it too); `1`: it crosses midnight.
    pub fn reaches_day_after(&self, days: u32) -> bool {
        self.start_min + self.estimate_min > days * DAY_MIN
    }
    /// The planned start in minutes after midnight of `day`: negative when the entry
    /// belongs to the day before and is shown on `day` because it crosses midnight.
    pub fn start_on(&self, day: NaiveDate) -> i64 {
        (self.date - day).num_days() * DAY_MIN as i64 + self.start_min as i64
    }
    /// The part of the planned time that lies on `day`, as minutes after its midnight
    /// (start, end). The rest belongs to the day before or after.
    pub fn span_on(&self, day: NaiveDate) -> (u32, u32) {
        let start = self.start_on(day);
        let on_day = |min: i64| min.clamp(0, DAY_MIN as i64) as u32;
        (on_day(start), on_day(start + self.estimate_min as i64))
    }
    /// The planned time as shown on `day`, e.g. "23:00 – 01:00 (next day)".
    pub fn time_span_on(&self, day: NaiveDate) -> String {
        fmt_time_span(self.start_min, self.estimate_min, (day - self.date).num_days().max(0))
    }
    pub fn from_task(t: &Task, date: NaiveDate) -> Self {
        Entry {
            task_id: t.id,
            run_id: None,
            date,
            title: t.title.clone(),
            notes: t.notes.clone(),
            start_min: t.start_min,
            estimate_min: t.estimate_min,
            priority: t.priority,
            notify: t.notify,
            notify_ahead_min: t.notify_ahead_min,
            recurring: t.schedule.is_recurring(),
            schedule_desc: t.schedule.describe(),
            status: EntryStatus::Virtual,
            tracked_secs: 0,
            first_start: None,
            added_secs: 0,
            task_exists: true,
        }
    }
    pub fn from_run(r: &Run, task: Option<&Task>, now: i64) -> Self {
        Entry {
            task_id: r.task_id,
            run_id: Some(r.id),
            date: r.date,
            title: r.title.clone(),
            notes: r.notes.clone(),
            start_min: r.start_min,
            estimate_min: r.estimate_min,
            priority: r.priority,
            notify: r.notify,
            notify_ahead_min: task.map_or(0, |t| t.notify_ahead_min),
            recurring: r.recurring,
            schedule_desc: task.map(|t| t.schedule.describe()).unwrap_or_default(),
            status: match r.status {
                RunStatus::Running => EntryStatus::Running,
                RunStatus::Paused => EntryStatus::Paused,
                RunStatus::Done => EntryStatus::Done,
                RunStatus::Failed => EntryStatus::Failed,
            },
            tracked_secs: r.tracked_secs(now),
            first_start: r.first_start(),
            added_secs: r.added_secs,
            task_exists: task.is_some(),
        }
    }
}

pub fn fmt_hm(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

/// "09:00 – 09:30". A span that crosses midnight says on which day an end lies that
/// isn't on the day it is shown on, `days_after_start` days after its start:
/// "23:00 – 01:00 (next day)", "23:00 (day before) – 01:00", "23:00 (2 days before) – 22:00".
pub fn fmt_time_span(start_min: u32, estimate_min: u32, days_after_start: i64) -> String {
    let end = start_min + estimate_min;
    // ending exactly at midnight: "24:00" of the day before
    let (end_day, end_hm) = if end > 0 && end.is_multiple_of(DAY_MIN) {
        (end / DAY_MIN - 1, "24:00".to_string())
    } else {
        (end / DAY_MIN, fmt_hm(end % DAY_MIN))
    };
    let note = |offset: i64| match offset {
        0 => String::new(),
        1 => " (next day)".into(),
        -1 => " (day before)".into(),
        n if n > 0 => format!(" (in {n} days)"),
        n => format!(" ({} days before)", -n),
    };
    let (start_note, end_note) = (note(-days_after_start), note(end_day as i64 - days_after_start));
    format!("{}{start_note} – {end_hm}{end_note}", fmt_hm(start_min % DAY_MIN))
}

pub fn fmt_duration_min(min: u32) -> String {
    match (min / 60, min % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// The scheduled start as local date and time. Never panics, even for a damaged
/// `start_min` of 24 h or more (it then lies on a following day).
pub fn scheduled_at(date: NaiveDate, start_min: u32) -> NaiveDateTime {
    date.and_time(NaiveTime::MIN) + Duration::minutes(start_min as i64)
}

/// A timestamp as local date and time.
pub fn local_time(t: i64) -> Option<NaiveDateTime> {
    chrono::DateTime::from_timestamp(t, 0).map(|t| t.with_timezone(&chrono::Local).naive_local())
}

/// Minutes from the scheduled start (`date` at `start_min`) to the local time `t`;
/// positive = later. Rounded down, so 30 s early counts as 1 min early.
pub fn minutes_between(date: NaiveDate, start_min: u32, t: NaiveDateTime) -> i64 {
    (t - scheduled_at(date, start_min)).num_seconds().div_euclid(60)
}

/// Minutes from the scheduled start (`date` at `start_min`) to the timestamp `t`; positive = later.
pub fn minutes_from_start(date: NaiveDate, start_min: u32, t: i64) -> i64 {
    local_time(t).map_or(0, |t| minutes_between(date, start_min, t))
}

/// "25 min", "2 h 5 min", "3 days 4 h" for a (positive) number of minutes.
pub fn fmt_span_min(min: i64) -> String {
    let min = min.max(0);
    if min < 24 * 60 {
        return fmt_duration_min(min as u32);
    }
    let (d, h) = (min / (24 * 60), (min % (24 * 60)) / 60);
    let days = if d == 1 { "1 day".to_string() } else { format!("{d} days") };
    if h == 0 || d >= 3 { days } else { format!("{days} {h} h") }
}

pub fn fmt_hms(secs: i64) -> String {
    let s = secs.max(0);
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

pub fn monday_of(d: NaiveDate) -> NaiveDate {
    d - Duration::days(weekday_index(d) as i64)
}

pub fn first_of_month(d: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap()
}

pub fn add_months(d: NaiveDate, n: i32) -> NaiveDate {
    let total = d.year() * 12 + d.month0() as i32 + n;
    let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    let day = d.day().min(days_in_month(y, m));
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// A plain task for tests (normal priority, reminder with sound, no announcement).
#[cfg(test)]
pub fn test_task(title: &str, schedule: Schedule, start_min: u32, estimate_min: u32) -> Task {
    Task {
        id: 0,
        title: title.into(),
        notes: String::new(),
        schedule,
        start_min,
        estimate_min,
        priority: Priority::Normal,
        notify: NotifyMode::Sound,
        notify_ahead_min: 0,
        created_at: 0,
        deleted: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn single_date() {
        let s = Schedule::Date { date: d(2026, 11, 5), repeat: Repeat::Never };
        assert!(s.occurs_on(d(2026, 11, 5)));
        assert!(!s.occurs_on(d(2026, 11, 12)));
        assert!(!s.is_recurring());
    }

    #[test]
    fn weekly_and_monthly_clamp() {
        let s = Schedule::Date { date: d(2026, 11, 5), repeat: Repeat::Weekly };
        assert!(s.occurs_on(d(2026, 11, 12)));
        assert!(!s.occurs_on(d(2026, 10, 29)), "not before start");
        let m = Schedule::Date { date: d(2026, 1, 31), repeat: Repeat::Monthly };
        assert!(m.occurs_on(d(2026, 2, 28)));
        assert!(m.occurs_on(d(2026, 4, 30)));
        assert!(!m.occurs_on(d(2026, 4, 29)));
        let y = Schedule::Date { date: d(2024, 2, 29), repeat: Repeat::Yearly };
        assert!(y.occurs_on(d(2025, 2, 28)));
        assert!(y.occurs_on(d(2028, 2, 29)));
    }

    #[test]
    fn weekdays_and_monthdays() {
        // Mon + Wed
        let s = Schedule::Weekdays { mask: 0b101, from: d(2026, 11, 1), every: WeekRepeat::Weekly };
        assert!(s.occurs_on(d(2026, 11, 2))); // Monday
        assert!(s.occurs_on(d(2026, 11, 4))); // Wednesday
        assert!(!s.occurs_on(d(2026, 11, 3)));
        assert!(s.occurs_on(d(2031, 6, 2)), "endless into the future");
        let m = Schedule::MonthDays { mask: (1 << 1) | (1 << 31), from: d(2026, 1, 1) };
        assert!(m.occurs_on(d(2026, 3, 31)));
        assert!(!m.occurs_on(d(2026, 4, 30)));
        assert!(m.occurs_on(d(2026, 4, 1)));
    }

    #[test]
    fn weekday_rules() {
        let from = d(2026, 11, 2); // Monday
        let bi = Schedule::Weekdays { mask: 1, from, every: WeekRepeat::BiWeekly };
        assert!(bi.occurs_on(d(2026, 11, 2)));
        assert!(!bi.occurs_on(d(2026, 11, 9)));
        assert!(bi.occurs_on(d(2026, 11, 16)));
        // Friday: first is 6 Nov, last is 27 Nov 2026
        let first = Schedule::Weekdays { mask: 1 << 4, from, every: WeekRepeat::FirstInMonth };
        assert!(first.occurs_on(d(2026, 11, 6)));
        assert!(!first.occurs_on(d(2026, 11, 13)));
        assert!(first.occurs_on(d(2026, 12, 4)));
        let last = Schedule::Weekdays { mask: 1 << 4, from, every: WeekRepeat::LastInMonth };
        assert!(last.occurs_on(d(2026, 11, 27)));
        assert!(!last.occurs_on(d(2026, 11, 20)));
        assert!(last.occurs_on(d(2026, 12, 25)));
        // legacy conversion
        let old = Schedule::Date { date: d(2026, 11, 5), repeat: Repeat::Weekly }.normalized();
        assert_eq!(old, Schedule::Weekdays { mask: 1 << 3, from: d(2026, 11, 5), every: WeekRepeat::Weekly });
        let json = r#"{"Weekdays":{"mask":3,"from":"2026-11-01"}}"#;
        let s: Schedule = serde_json::from_str(json).unwrap();
        assert!(matches!(s, Schedule::Weekdays { every: WeekRepeat::Weekly, .. }));
    }

    #[test]
    fn month_math() {
        assert_eq!(add_months(d(2026, 1, 31), 1), d(2026, 2, 28));
        assert_eq!(add_months(d(2026, 1, 15), -1), d(2025, 12, 15));
        assert_eq!(monday_of(d(2026, 11, 5)), d(2026, 11, 2));
    }

    #[test]
    fn descriptions() {
        let from = d(2026, 11, 2);
        let wd = |mask, every| Schedule::Weekdays { mask, from, every }.describe();
        assert_eq!(wd(0b111_1111, WeekRepeat::Weekly), "Every day");
        assert_eq!(wd(0b001_1111, WeekRepeat::Weekly), "Weekdays (Mon–Fri)");
        assert_eq!(wd(0b101, WeekRepeat::Weekly), "Every Mon, Wed");
        assert_eq!(wd(1, WeekRepeat::BiWeekly), "Every other week on Mon");
        assert_eq!(wd(1 << 4, WeekRepeat::LastInMonth), "Last Fri of the month");
        assert_eq!(Schedule::MonthDays { mask: (1 << 1) | (1 << 15), from }.describe(), "Monthly on the 1., 15.");
        assert_eq!(Schedule::Date { date: from, repeat: Repeat::Yearly }.describe(), "Yearly on 02.11.");
        assert_eq!(Schedule::Date { date: from, repeat: Repeat::Never }.describe(), "Mon 02.11.2026");
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_hm(9 * 60 + 5), "09:05");
        assert_eq!(fmt_duration_min(45), "45 min");
        assert_eq!(fmt_duration_min(120), "2 h");
        assert_eq!(fmt_duration_min(150), "2 h 30 min");
        assert_eq!(fmt_span_min(-5), "0 min", "negative spans are clamped");
        assert_eq!(fmt_span_min(25 * 60), "1 day 1 h");
        assert_eq!(fmt_span_min(4 * 24 * 60 + 300), "4 days", "hours dropped from 3 days on");
        assert_eq!(fmt_hms(3725), "01:02:05");
        assert_eq!(fmt_hms(-3), "00:00:00");
    }

    #[test]
    fn spans_across_midnight() {
        let day = d(2026, 11, 6);
        let mut e = Entry::from_task(
            &test_task("Night shift", Schedule::Date { date: day, repeat: Repeat::Never }, 23 * 60, 120),
            day,
        );
        assert!(e.reaches_day_after(1));
        assert_eq!(e.span_on(day), (23 * 60, DAY_MIN), "its own day: until midnight");
        assert_eq!(e.span_on(d(2026, 11, 7)), (0, 60), "the next day: from midnight");
        assert_eq!((e.start_on(day), e.start_on(d(2026, 11, 7))), (23 * 60, -60));
        assert_eq!(e.time_span_on(day), "23:00 – 01:00 (next day)");
        assert_eq!(e.time_span_on(d(2026, 11, 7)), "23:00 (day before) – 01:00");
        e.estimate_min = 60;
        assert!(!e.reaches_day_after(1), "ending at midnight stays on its day");
        assert_eq!(e.span_on(day), (23 * 60, DAY_MIN));
        assert_eq!(e.time_span_on(day), "23:00 – 24:00");
        assert_eq!(fmt_time_span(9 * 60, 30, 0), "09:00 – 09:30");
        // the longest estimates reach two days further
        e.estimate_min = 47 * 60;
        assert!(e.reaches_day_after(2) && !e.reaches_day_after(3));
        assert_eq!(e.span_on(d(2026, 11, 8)), (0, 22 * 60));
        assert_eq!(e.time_span_on(day), "23:00 – 22:00 (in 2 days)");
        assert_eq!(e.time_span_on(d(2026, 11, 7)), "23:00 (day before) – 22:00 (next day)");
        assert_eq!(e.time_span_on(d(2026, 11, 8)), "23:00 (2 days before) – 22:00");
    }

    #[test]
    fn time_offsets() {
        let day = d(2026, 11, 6);
        let at = |h, m, s| day.and_hms_opt(h, m, s).unwrap();
        assert_eq!(minutes_between(day, 9 * 60, at(9, 0, 0)), 0);
        assert_eq!(minutes_between(day, 9 * 60, at(9, 12, 59)), 12, "late, rounded down");
        assert_eq!(minutes_between(day, 9 * 60, at(8, 59, 30)), -1, "30 s early is 1 min early");
        assert_eq!(minutes_between(day, 9 * 60, at(8, 30, 0)), -30);
        // a damaged start time must not panic; it simply lies on the next day
        assert_eq!(scheduled_at(day, 24 * 60 + 30), d(2026, 11, 7).and_hms_opt(0, 30, 0).unwrap());
    }

    #[test]
    fn calendar_helpers() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2026, 12), 31);
        assert_eq!(first_of_month(d(2026, 11, 17)), d(2026, 11, 1));
        assert_eq!(add_months(d(2026, 12, 15), 1), d(2027, 1, 15));
        assert_eq!(add_months(d(2026, 3, 31), -13), d(2025, 2, 28));
        assert_eq!(monday_of(d(2026, 11, 2)), d(2026, 11, 2), "a Monday is its own week start");
        assert_eq!(monday_of(d(2026, 11, 8)), d(2026, 11, 2), "Sunday belongs to the week before");
    }

    #[test]
    fn biweekly_counts_from_the_start_week() {
        // Started on a Wednesday: its week (and every other one after it) counts, also
        // for weekdays before the start day in later weeks.
        let s = Schedule::Weekdays { mask: 0b11, from: d(2026, 11, 4), every: WeekRepeat::BiWeekly };
        assert!(!s.occurs_on(d(2026, 11, 2)), "before the start");
        assert!(s.occurs_on(d(2026, 11, 16)));
        assert!(s.occurs_on(d(2026, 11, 17)));
        assert!(!s.occurs_on(d(2026, 11, 9)));
        assert!(!s.occurs_on(d(2026, 11, 23)));
    }
}
