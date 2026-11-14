//! SQLite persistence plus an in-memory copy of all data (it is small), written through on change.
//!
//! Every change is written to the database first (multi-step changes in one transaction)
//! and only then applied to the in-memory copy, so the two can't drift apart on errors.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use chrono::{Duration, NaiveDate};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::model::*;
use crate::settings::Settings;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tasks (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    notes TEXT NOT NULL DEFAULT '',
    schedule TEXT NOT NULL,
    start_min INTEGER NOT NULL,
    estimate_min INTEGER NOT NULL,
    priority INTEGER NOT NULL,
    notify INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0,
    -- minutes before the start to announce the task (0 = off)
    notify_ahead INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL,
    date TEXT NOT NULL,
    title TEXT NOT NULL,
    notes TEXT NOT NULL DEFAULT '',
    start_min INTEGER NOT NULL,
    estimate_min INTEGER NOT NULL,
    priority INTEGER NOT NULL,
    notify INTEGER NOT NULL,
    recurring INTEGER NOT NULL,
    status INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    finished_at INTEGER,
    -- tracked time added by hand for work done before the task was started
    added_secs INTEGER NOT NULL DEFAULT 0,
    UNIQUE(task_id, date)
);
CREATE TABLE IF NOT EXISTS segments (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES runs(id),
    start INTEGER NOT NULL,
    end INTEGER
);
CREATE TABLE IF NOT EXISTS notif_state (
    key TEXT PRIMARY KEY,
    dismissed INTEGER NOT NULL DEFAULT 0,
    snooze_until INTEGER
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

#[derive(Clone, Copy, Debug, Default)]
pub struct NotifState {
    pub dismissed: bool,
    pub snooze_until: Option<i64>,
}

pub struct Store {
    conn: Connection,
    /// All tasks, deleted ones included (their runs still refer to them).
    tasks: Vec<Task>,
    /// All runs ever. Only [`Store::undo_run`] removes one (and then rebuilds the lookups,
    /// which hold indices into this list).
    runs: Vec<Run>,
    notif_state: HashMap<String, NotifState>,
    ix: Index,
}

/// Lookups into `Store::tasks` and `Store::runs` (as indices). The UI asks for single
/// days, runs and tasks many times per frame; these keep that independent of how much
/// history has piled up. Only [`Store::index_tasks`], [`Store::index_runs`],
/// [`Store::index_run`] and [`Store::set_status`] write them, which is why the lists
/// themselves are private.
#[derive(Default)]
struct Index {
    task_by_id: HashMap<i64, usize>,
    /// Planned (not deleted) tasks: the repeating ones, and the one-off ones by their date.
    recurring: Vec<usize>,
    once: HashMap<NaiveDate, Vec<usize>>,
    run_by_id: HashMap<i64, usize>,
    runs_by_date: HashMap<NaiveDate, Vec<usize>>,
    runs_by_task: HashMap<i64, Vec<usize>>,
    /// Runs that are running or paused, oldest first.
    open_runs: BTreeSet<usize>,
}

/// Reminder state older than this many days is deleted at startup.
const NOTIF_STATE_KEEP_DAYS: i64 = 7;

type Res<T> = rusqlite::Result<T>;

/// Bring databases from older versions up to the current schema.
fn migrate(conn: &Connection) -> Res<()> {
    let has_added: bool = conn.prepare("SELECT 1 FROM pragma_table_info('runs') WHERE name='added_secs'")?.exists([])?;
    if !has_added {
        conn.execute_batch("ALTER TABLE runs ADD COLUMN added_secs INTEGER NOT NULL DEFAULT 0;")?;
    }
    let has_ahead: bool =
        conn.prepare("SELECT 1 FROM pragma_table_info('tasks') WHERE name='notify_ahead'")?.exists([])?;
    if !has_ahead {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN notify_ahead INTEGER NOT NULL DEFAULT 0;")?;
    }
    Ok(())
}

/// Reads the numbers of one row leniently: a damaged value (not a number, or outside what
/// the app can use) is replaced and written to the log instead of failing the whole load.
/// One bad row must not keep the app from starting.
struct Cols<'a, 'r> {
    row: &'a Row<'r>,
    /// Which row, for the log: "task 3", "run 12" …
    what: String,
}

impl Cols<'_, '_> {
    fn damaged(&self, col: &str, value: &dyn std::fmt::Debug, using: &dyn std::fmt::Display) {
        crate::logfile::error(format!("{}: unusable {col} {value:?}; using {using}", self.what));
    }

    /// The raw value of column `i`, as something to show in the log.
    fn raw(&self, i: usize) -> String {
        match self.row.get_ref(i) {
            Ok(ValueRef::Null) => "NULL".into(),
            Ok(ValueRef::Integer(v)) => v.to_string(),
            Ok(ValueRef::Real(v)) => v.to_string(),
            Ok(ValueRef::Text(t)) => String::from_utf8_lossy(t).into_owned(),
            Ok(ValueRef::Blob(b)) => format!("<{} bytes>", b.len()),
            Err(e) => e.to_string(),
        }
    }

    /// An integer, None if the column holds anything else.
    fn integer(&self, i: usize) -> Option<i64> {
        match self.row.get_ref(i) {
            Ok(ValueRef::Integer(v)) => Some(v),
            _ => None,
        }
    }

    /// Column `i` (`col`) as an integer, or `default`.
    fn int(&self, i: usize, col: &str, default: i64) -> i64 {
        self.integer(i).unwrap_or_else(|| {
            self.damaged(col, &self.raw(i), &default);
            default
        })
    }

    /// A column that may be empty (NULL): an integer, or None.
    fn opt_int(&self, i: usize, col: &str) -> Option<i64> {
        match self.row.get_ref(i) {
            Ok(ValueRef::Null) => None,
            Ok(ValueRef::Integer(v)) => Some(v),
            _ => {
                self.damaged(col, &self.raw(i), &"nothing");
                None
            }
        }
    }

    /// A number of minutes (or similar) in `0..=max`: one outside is limited to that range.
    fn minutes(&self, i: usize, col: &str, max: u32, default: u32) -> u32 {
        let Some(v) = self.integer(i) else {
            self.damaged(col, &self.raw(i), &default);
            return default;
        };
        let ok = v.clamp(0, max as i64) as u32;
        if ok as i64 != v {
            self.damaged(col, &v, &ok);
        }
        ok
    }
}

/// Order of the entries shown on `day`: by start time (one carried over from the day
/// before comes first), then the more important one, then by title.
fn sort_for_day(entries: &mut [Entry], day: NaiveDate) {
    entries.sort_by(|a, b| {
        a.start_on(day).cmp(&b.start_on(day)).then(b.priority.cmp(&a.priority)).then(a.title.cmp(&b.title))
    });
}

impl Store {
    pub fn open(path: &Path) -> Res<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        let mut s = Store { conn, tasks: vec![], runs: vec![], notif_state: HashMap::new(), ix: Index::default() };
        s.load()?;
        s.prune_notif_state(chrono::Local::now().date_naive())?;
        Ok(s)
    }

    fn load(&mut self) -> Res<()> {
        let mut st = self.conn.prepare(
            "SELECT id,title,notes,schedule,start_min,estimate_min,priority,notify,created_at,deleted,notify_ahead FROM tasks",
        )?;
        self.tasks = st
            .query_map([], |r| {
                let id: i64 = r.get(0)?;
                let title: String = r.get(1)?;
                let sched: String = r.get(3)?;
                // A schedule that can't be read must not keep the app from starting: the
                // task shows up today (so it gets noticed) and the log says what happened.
                let schedule = serde_json::from_str::<Schedule>(&sched).unwrap_or_else(|e| {
                    crate::logfile::error(format!(
                        "task {id} \"{title}\": unreadable schedule {sched:?} ({e}); shown as a one-off task today"
                    ));
                    Schedule::Date { date: chrono::Local::now().date_naive(), repeat: Repeat::Never }
                });
                let c = Cols { row: r, what: format!("task {id} \"{title}\"") };
                Ok(Task {
                    id,
                    title,
                    notes: r.get(2)?,
                    schedule: schedule.normalized(),
                    start_min: c.minutes(4, "start time", DAY_MIN - 1, 9 * 60),
                    estimate_min: c.minutes(5, "estimate", MAX_ESTIMATE_MIN, 30),
                    priority: Priority::from_i64(c.int(6, "priority", Priority::Normal as i64)),
                    // a damaged reminder setting keeps the reminder: that is what Tasker is for
                    notify: NotifyMode::from_i64(c.int(7, "reminder", NotifyMode::Sound as i64)),
                    created_at: c.int(8, "creation time", 0),
                    deleted: c.int(9, "deleted flag", 0) != 0,
                    notify_ahead_min: c.minutes(10, "notify ahead time", MAX_NOTIFY_AHEAD_MIN, 0),
                })
            })?
            .collect::<Res<_>>()?;
        drop(st);

        let mut segs: HashMap<i64, Vec<Segment>> = HashMap::new();
        let mut st = self.conn.prepare("SELECT id,run_id,start,end FROM segments ORDER BY start")?;
        for row in st.query_map([], |r| {
            let id: i64 = r.get(0)?;
            let c = Cols { row: r, what: format!("time segment {id}") };
            // Without a start the segment's time is unknown: leave it out (a made-up start
            // could add years of tracked time). A damaged end closes it where it began.
            let Some(start) = c.integer(2) else {
                c.damaged("start", &c.raw(2), &"nothing (segment left out)");
                return Ok(None);
            };
            let end = match r.get_ref(3)? {
                ValueRef::Null => None,
                ValueRef::Integer(v) => Some(v),
                _ => {
                    c.damaged("end", &c.raw(3), &"its start");
                    Some(start)
                }
            };
            Ok(c.integer(1).map(|run_id| (run_id, Segment { id, start, end })))
        })? {
            if let Some((run_id, seg)) = row? {
                segs.entry(run_id).or_default().push(seg);
            }
        }
        drop(st);

        let mut st = self.conn.prepare(
            "SELECT id,task_id,date,title,notes,start_min,estimate_min,priority,notify,recurring,status,created_at,finished_at,added_secs FROM runs",
        )?;
        self.runs = st
            .query_map([], |r| {
                let date: String = r.get(2)?;
                let id: i64 = r.get(0)?;
                // Same for a run's date: keep the run (its tracked time), filed under 1970-01-01.
                let date = NaiveDate::parse_from_str(&date, "%Y-%m-%d").unwrap_or_else(|e| {
                    crate::logfile::error(format!("run {id}: unreadable date {date:?} ({e}); filed under 1970-01-01"));
                    NaiveDate::default()
                });
                let c = Cols { row: r, what: format!("run {id}") };
                let added_secs = c.int(13, "added time", 0);
                Ok(Run {
                    id,
                    // 0 is no task's id: the run is kept, as one of a deleted task
                    task_id: c.int(1, "task", 0),
                    date,
                    title: r.get(3)?,
                    notes: r.get(4)?,
                    start_min: c.minutes(5, "start time", DAY_MIN - 1, 9 * 60),
                    estimate_min: c.minutes(6, "estimate", MAX_ESTIMATE_MIN, 30),
                    priority: Priority::from_i64(c.int(7, "priority", Priority::Normal as i64)),
                    notify: NotifyMode::from_i64(c.int(8, "reminder", NotifyMode::Sound as i64)),
                    recurring: c.int(9, "recurring flag", 0) != 0,
                    status: RunStatus::from_i64(c.int(10, "status", RunStatus::Paused as i64)),
                    created_at: c.int(11, "creation time", 0),
                    finished_at: c.opt_int(12, "finish time"),
                    added_secs: if added_secs < 0 {
                        c.damaged("added time", &added_secs, &0);
                        0
                    } else {
                        added_secs
                    },
                    segments: segs.remove(&id).unwrap_or_default(),
                })
            })?
            .collect::<Res<_>>()?;
        drop(st);
        self.index_tasks();
        self.index_runs();

        let mut st = self.conn.prepare("SELECT key,dismissed,snooze_until FROM notif_state")?;
        self.notif_state = st
            .query_map([], |r| {
                let key: String = r.get(0)?;
                let c = Cols { row: r, what: format!("reminder state {key:?}") };
                // damaged: not dismissed. A reminder too many is better than a lost one.
                let st = NotifState {
                    dismissed: c.int(1, "dismissed flag", 0) != 0,
                    snooze_until: c.opt_int(2, "snooze time"),
                };
                Ok((key, st))
            })?
            .collect::<Res<_>>()?;
        Ok(())
    }

    // ---------------------------------------------------------------- settings

    pub fn load_settings(&self) -> Settings {
        self.conn
            .query_row("SELECT value FROM settings WHERE key='settings'", [], |r| r.get::<_, String>(0))
            .optional()
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            .map(|mut s| {
                s.ui_scale = crate::settings::clamp_scale(s.ui_scale);
                let close = crate::settings::AHEAD_CLOSE_SECS;
                s.ahead_close_secs = s.ahead_close_secs.clamp(*close.start(), *close.end());
                s
            })
            .unwrap_or_default()
    }

    pub fn load_window(&self) -> Option<crate::settings::WindowGeom> {
        let json: String = self
            .conn
            .query_row("SELECT value FROM settings WHERE key='window'", [], |r| r.get(0))
            .optional()
            .ok()
            .flatten()?;
        serde_json::from_str::<crate::settings::WindowGeom>(&json)
            .ok()
            .filter(|g| [g.x, g.y, g.w, g.h].iter().all(|v| v.is_finite()) && g.w >= 100.0 && g.h >= 100.0)
    }

    pub fn save_window(&self, g: &crate::settings::WindowGeom) -> Res<()> {
        let json = serde_json::to_string(g).unwrap_or_default();
        self.conn.execute(
            "INSERT INTO settings(key,value) VALUES('window',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![json],
        )?;
        Ok(())
    }

    pub fn save_settings(&self, s: &Settings) -> Res<()> {
        let json = serde_json::to_string(s).unwrap_or_default();
        self.conn.execute(
            "INSERT INTO settings(key,value) VALUES('settings',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![json],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- tasks

    pub fn task(&self, id: i64) -> Option<&Task> {
        self.ix.task_by_id.get(&id).map(|&i| &self.tasks[i]).filter(|t| !t.deleted)
    }

    /// All tasks that have not been deleted.
    pub fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(|t| !t.deleted)
    }

    /// Rebuild the task lookups: after loading and after every change to a task (rare,
    /// and cheap next to the database write).
    fn index_tasks(&mut self) {
        let ix = &mut self.ix;
        ix.task_by_id.clear();
        ix.recurring.clear();
        ix.once.clear();
        for (i, t) in self.tasks.iter().enumerate() {
            ix.task_by_id.insert(t.id, i);
            match t.schedule {
                _ if t.deleted => {}
                Schedule::Date { date, repeat: Repeat::Never } => ix.once.entry(date).or_default().push(i),
                _ => ix.recurring.push(i),
            }
        }
    }

    pub fn save_task(&mut self, mut t: Task) -> Res<i64> {
        let sched = serde_json::to_string(&t.schedule).unwrap_or_default();
        if t.id == 0 {
            self.conn.execute(
                "INSERT INTO tasks(title,notes,schedule,start_min,estimate_min,priority,notify,created_at,notify_ahead) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![t.title, t.notes, sched, t.start_min, t.estimate_min, t.priority as i64, t.notify as i64, t.created_at, t.notify_ahead_min],
            )?;
            t.id = self.conn.last_insert_rowid();
            let id = t.id;
            self.tasks.push(t);
            self.index_tasks();
            Ok(id)
        } else {
            self.conn.execute(
                "UPDATE tasks SET title=?1,notes=?2,schedule=?3,start_min=?4,estimate_min=?5,priority=?6,notify=?7,notify_ahead=?8 WHERE id=?9",
                params![t.title, t.notes, sched, t.start_min, t.estimate_min, t.priority as i64, t.notify as i64, t.notify_ahead_min, t.id],
            )?;
            let id = t.id;
            if let Some(&i) = self.ix.task_by_id.get(&id) {
                self.tasks[i] = t;
                self.index_tasks();
            }
            Ok(id)
        }
    }

    /// Soft delete: runs (history, statistics) are kept.
    pub fn delete_task(&mut self, id: i64) -> Res<()> {
        self.conn.execute("UPDATE tasks SET deleted=1 WHERE id=?1", params![id])?;
        if let Some(&i) = self.ix.task_by_id.get(&id) {
            self.tasks[i].deleted = true;
            self.index_tasks();
        }
        Ok(())
    }

    // ---------------------------------------------------------------- entries

    /// The entries that belong to a day: its real runs plus the virtual occurrences that
    /// have not been started yet. Sorted by start time.
    pub fn entries_for(&self, date: NaiveDate, now: i64) -> Vec<Entry> {
        let mut out: Vec<Entry> = self.runs_on(date).map(|r| Entry::from_run(r, self.task(r.task_id), now)).collect();
        let started = out.len();
        for t in self.planned_on(date) {
            if !out[..started].iter().any(|e| e.task_id == t.id) {
                out.push(Entry::from_task(t, date));
            }
        }
        sort_for_day(&mut out, date);
        out
    }

    /// Everything to show on a day: its own entries, and those of the day before whose
    /// planned time reaches past midnight (they are on both days). Sorted by start time.
    pub fn entries_shown_on(&self, date: NaiveDate, now: i64) -> Vec<Entry> {
        let mut out = self.entries_for(date, now);
        for days in 1..=MAX_DAYS_REACHED {
            if let Some(before) = date.checked_sub_days(chrono::Days::new(days as u64)) {
                out.extend(self.entries_for(before, now).into_iter().filter(|e| e.reaches_day_after(days)));
            }
        }
        sort_for_day(&mut out, date);
        out
    }

    /// Number of entries [`Store::entries_shown_on`] would return, without building them.
    pub fn count_shown_on(&self, date: NaiveDate) -> usize {
        let mut n = self.count_where(date, |_, _| true);
        for days in 1..=MAX_DAYS_REACHED {
            if let Some(before) = date.checked_sub_days(chrono::Days::new(days as u64)) {
                n += self.count_where(before, |start, estimate| start + estimate > days * DAY_MIN);
            }
        }
        n
    }

    /// How many entries of `date` have a (start, estimate) that `keep` accepts.
    fn count_where(&self, date: NaiveDate, keep: impl Fn(u32, u32) -> bool) -> usize {
        let started: Vec<i64> = self.runs_on(date).map(|r| r.task_id).collect();
        let runs = self.runs_on(date).filter(|r| keep(r.start_min, r.estimate_min)).count();
        let planned = self.planned_on(date).filter(|t| !started.contains(&t.id) && keep(t.start_min, t.estimate_min));
        runs + planned.count()
    }

    /// One task's entry on one day: its run, or the virtual occurrence if it has none yet.
    pub fn entry(&self, task_id: i64, date: NaiveDate, now: i64) -> Option<Entry> {
        match self.run_on(task_id, date) {
            Some(r) => Some(Entry::from_run(r, self.task(task_id), now)),
            None => self.task(task_id).filter(|t| t.schedule.occurs_on(date)).map(|t| Entry::from_task(t, date)),
        }
    }

    /// The runs (started occurrences) of one day.
    pub fn runs_on(&self, date: NaiveDate) -> impl Iterator<Item = &Run> {
        self.ix.runs_by_date.get(&date).into_iter().flatten().map(|&i| &self.runs[i])
    }

    /// All runs of one task.
    pub fn runs_of(&self, task_id: i64) -> impl Iterator<Item = &Run> {
        self.ix.runs_by_task.get(&task_id).into_iter().flatten().map(|&i| &self.runs[i])
    }

    /// The runs that are running or paused, oldest first.
    pub fn open_runs(&self) -> impl Iterator<Item = &Run> {
        self.ix.open_runs.iter().map(|&i| &self.runs[i])
    }

    /// Tasks that occur on `date` according to their schedule (started or not).
    fn planned_on(&self, date: NaiveDate) -> impl Iterator<Item = &Task> {
        let once = self.ix.once.get(&date).into_iter().flatten();
        let recurring = self.ix.recurring.iter().filter(move |&&i| self.tasks[i].schedule.occurs_on(date));
        once.chain(recurring).map(|&i| &self.tasks[i])
    }

    pub fn run(&self, id: i64) -> Option<&Run> {
        self.run_index(id).map(|i| &self.runs[i])
    }

    /// The run of a task on a day, if that occurrence has been started (or given up).
    fn run_on(&self, task_id: i64, date: NaiveDate) -> Option<&Run> {
        self.runs_on(date).find(|r| r.task_id == task_id)
    }

    fn run_index(&self, id: i64) -> Option<usize> {
        self.ix.run_by_id.get(&id).copied()
    }

    /// Rebuild the run lookups: after loading, and after a run was removed.
    fn index_runs(&mut self) {
        let ix = &mut self.ix;
        ix.run_by_id.clear();
        ix.runs_by_date.clear();
        ix.runs_by_task.clear();
        ix.open_runs.clear();
        for i in 0..self.runs.len() {
            self.index_run(i);
        }
    }

    /// Enter the run at index `i` into the lookups (once: when it is loaded or created).
    fn index_run(&mut self, i: usize) {
        let r = &self.runs[i];
        self.ix.run_by_id.insert(r.id, i);
        self.ix.runs_by_date.entry(r.date).or_default().push(i);
        self.ix.runs_by_task.entry(r.task_id).or_default().push(i);
        if r.status.is_open() {
            self.ix.open_runs.insert(i);
        }
    }

    /// The one place a run's status changes in memory (keeps the open-runs lookup right).
    fn set_status(&mut self, i: usize, status: RunStatus, finished_at: Option<i64>) {
        let r = &mut self.runs[i];
        r.status = status;
        r.finished_at = finished_at;
        if status.is_open() {
            self.ix.open_runs.insert(i);
        } else {
            self.ix.open_runs.remove(&i);
        }
    }

    // ---------------------------------------------------------------- time tracking

    /// Start a virtual occurrence (creates the real task) or continue a paused/finished run.
    pub fn start(&mut self, task_id: i64, date: NaiveDate, now: i64) -> Res<i64> {
        if let Some(id) = self.run_on(task_id, date).map(|r| r.id) {
            self.resume(id, now)?;
            return Ok(id);
        }
        self.create_run(task_id, date, now, RunStatus::Running)
    }

    /// Give up a virtual occurrence without working on it: creates the real task like
    /// [`Store::start`] does, but as failed and with no tracked time. No-op if the
    /// occurrence has been started already.
    pub fn fail(&mut self, task_id: i64, date: NaiveDate, now: i64) -> Res<i64> {
        match self.run_on(task_id, date).map(|r| r.id) {
            Some(id) => Ok(id),
            None => self.create_run(task_id, date, now, RunStatus::Failed),
        }
    }

    /// Turn a virtual occurrence into a real task: a snapshot of the task as it is now.
    /// As `Running` its clock starts; any other status counts as finished right away.
    fn create_run(&mut self, task_id: i64, date: NaiveDate, now: i64, status: RunStatus) -> Res<i64> {
        let Some(t) = self.task(task_id).cloned() else {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        };
        let recurring = t.schedule.is_recurring();
        let running = status == RunStatus::Running;
        let finished_at = (!running).then_some(now);
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO runs(task_id,date,title,notes,start_min,estimate_min,priority,notify,recurring,status,created_at,finished_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![t.id, date.format("%Y-%m-%d").to_string(), t.title, t.notes, t.start_min, t.estimate_min,
                    t.priority as i64, t.notify as i64, recurring as i64, status as i64, now, finished_at],
        )?;
        let id = tx.last_insert_rowid();
        let mut segments = Vec::new();
        if running {
            tx.execute("INSERT INTO segments(run_id,start) VALUES(?1,?2)", params![id, now])?;
            segments.push(Segment { id: tx.last_insert_rowid(), start: now, end: None });
        }
        tx.commit()?;
        self.runs.push(Run {
            id,
            task_id: t.id,
            date,
            title: t.title,
            notes: t.notes,
            start_min: t.start_min,
            estimate_min: t.estimate_min,
            priority: t.priority,
            notify: t.notify,
            recurring,
            status,
            created_at: now,
            finished_at,
            added_secs: 0,
            segments,
        });
        self.index_run(self.runs.len() - 1);
        Ok(id)
    }

    /// Take a start or give-up back (it was clicked by accident): the run is deleted
    /// together with its tracked time, so the occurrence is a planned one again.
    ///
    /// Reminder state goes with it: that of the run's over-estimate reminder (a later run
    /// may get the same id), and a due reminder that was closed by the click (clicking
    /// Start or give-up in it marks it dismissed) is due again. A snooze is kept.
    pub fn undo_run(&mut self, run_id: i64) -> Res<()> {
        let Some(idx) = self.run_index(run_id) else { return Ok(()) };
        let r = &self.runs[idx];
        let due = format!("due:{}:{}", r.task_id, r.date);
        let mut forget = vec![format!("over:{run_id}")];
        if self.notif(&due).dismissed {
            forget.push(due);
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM segments WHERE run_id=?1", params![run_id])?;
        tx.execute("DELETE FROM runs WHERE id=?1", params![run_id])?;
        for key in &forget {
            tx.execute("DELETE FROM notif_state WHERE key=?1", params![key])?;
        }
        tx.commit()?;
        for key in &forget {
            self.notif_state.remove(key);
        }
        self.runs.remove(idx);
        self.index_runs();
        Ok(())
    }

    /// Continue a paused, finished or given-up run (a new time segment starts). No-op if
    /// it is running.
    pub fn resume(&mut self, run_id: i64, now: i64) -> Res<()> {
        let Some(idx) = self.run_index(run_id) else { return Ok(()) };
        if self.runs[idx].status == RunStatus::Running {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("INSERT INTO segments(run_id,start) VALUES(?1,?2)", params![run_id, now])?;
        let seg = Segment { id: tx.last_insert_rowid(), start: now, end: None };
        tx.execute(
            "UPDATE runs SET status=?1, finished_at=NULL WHERE id=?2",
            params![RunStatus::Running as i64, run_id],
        )?;
        tx.commit()?;
        self.runs[idx].segments.push(seg);
        self.set_status(idx, RunStatus::Running, None);
        Ok(())
    }

    /// Stop the clock of a running run. No-op unless it is running (a finished run must
    /// not turn into a paused one).
    pub fn pause(&mut self, run_id: i64, now: i64) -> Res<()> {
        let Some(idx) = self.run_index(run_id) else { return Ok(()) };
        if self.runs[idx].status != RunStatus::Running {
            return Ok(());
        }
        self.stop_clock(idx, now, RunStatus::Paused, None)
    }

    /// Mark a running or paused run as done. No-op otherwise: a finished run keeps its
    /// finish time, and a given-up one doesn't count as done.
    pub fn finish(&mut self, run_id: i64, now: i64) -> Res<()> {
        let Some(idx) = self.run_index(run_id) else { return Ok(()) };
        if !self.runs[idx].status.is_open() {
            return Ok(());
        }
        self.stop_clock(idx, now, RunStatus::Done, Some(now))
    }

    /// Close the open time segment (if any) and set the new status, atomically.
    fn stop_clock(&mut self, idx: usize, now: i64, status: RunStatus, finished_at: Option<i64>) -> Res<()> {
        let run_id = self.runs[idx].id;
        let open = self.runs[idx].segments.iter().position(|s| s.end.is_none());
        let tx = self.conn.unchecked_transaction()?;
        if let Some(i) = open {
            tx.execute("UPDATE segments SET end=?1 WHERE id=?2", params![now, self.runs[idx].segments[i].id])?;
        }
        tx.execute(
            "UPDATE runs SET status=?1, finished_at=?2 WHERE id=?3",
            params![status as i64, finished_at, run_id],
        )?;
        tx.commit()?;
        if let Some(i) = open {
            self.runs[idx].segments[i].end = Some(now);
        }
        self.set_status(idx, status, finished_at);
        Ok(())
    }

    /// Count `secs` more tracked time for work done before the task was started. Kept
    /// separately from the real time segments so it can be reset.
    pub fn add_time(&mut self, run_id: i64, secs: i64) -> Res<()> {
        let total = self.run(run_id).map_or(0, |r| r.added_secs) + secs;
        self.set_added_time(run_id, total.max(0))
    }

    /// Remove all time added with [`Store::add_time`].
    pub fn reset_added_time(&mut self, run_id: i64) -> Res<()> {
        self.set_added_time(run_id, 0)
    }

    fn set_added_time(&mut self, run_id: i64, secs: i64) -> Res<()> {
        let Some(idx) = self.run_index(run_id) else { return Ok(()) };
        self.conn.execute("UPDATE runs SET added_secs=?1 WHERE id=?2", params![secs, run_id])?;
        self.runs[idx].added_secs = secs;
        Ok(())
    }

    // ---------------------------------------------------------------- notification state

    pub fn notif(&self, key: &str) -> NotifState {
        self.notif_state.get(key).copied().unwrap_or_default()
    }

    /// Delete reminder state nobody will ask for again: due/announcement state of days
    /// more than a week ago, and over-estimate state of old closed (or vanished) runs.
    /// Without this the table grows by a few rows every day, forever.
    pub fn prune_notif_state(&mut self, today: NaiveDate) -> Res<usize> {
        let cutoff = today - Duration::days(NOTIF_STATE_KEEP_DAYS);
        let stale = |key: &str| -> bool {
            let parts: Vec<&str> = key.split(':').collect();
            match parts.as_slice() {
                ["due" | "ahead", _, date] => NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok_and(|d| d < cutoff),
                ["over", id] => id
                    .parse::<i64>()
                    .is_ok_and(|id| self.run(id).is_none_or(|r| !r.status.is_open() && r.date < cutoff)),
                _ => false,
            }
        };
        let keys: Vec<String> = self.notif_state.keys().filter(|k| stale(k)).cloned().collect();
        if keys.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        for k in &keys {
            tx.execute("DELETE FROM notif_state WHERE key=?1", params![k])?;
        }
        tx.commit()?;
        for k in &keys {
            self.notif_state.remove(k);
        }
        Ok(keys.len())
    }

    pub fn set_notif(&mut self, key: &str, st: NotifState) -> Res<()> {
        self.conn.execute(
            "INSERT INTO notif_state(key,dismissed,snooze_until) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET dismissed=excluded.dismissed, snooze_until=excluded.snooze_until",
            params![key, st.dismissed as i64, st.snooze_until],
        )?;
        self.notif_state.insert(key.to_string(), st);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_old_database() {
        let path = std::env::temp_dir().join(format!("tasker-migrate-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            // runs table as created by older versions (no added_secs)
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE runs (id INTEGER PRIMARY KEY, task_id INTEGER NOT NULL, date TEXT NOT NULL,
                 title TEXT NOT NULL, notes TEXT NOT NULL DEFAULT '', start_min INTEGER NOT NULL,
                 estimate_min INTEGER NOT NULL, priority INTEGER NOT NULL, notify INTEGER NOT NULL,
                 recurring INTEGER NOT NULL, status INTEGER NOT NULL, created_at INTEGER NOT NULL,
                 finished_at INTEGER, UNIQUE(task_id, date));
                 INSERT INTO runs VALUES (1, 1, '2026-11-05', 'Old', '', 600, 30, 1, 1, 0, 1, 0, NULL);
                 CREATE TABLE tasks (id INTEGER PRIMARY KEY, title TEXT NOT NULL, notes TEXT NOT NULL DEFAULT '',
                 schedule TEXT NOT NULL, start_min INTEGER NOT NULL, estimate_min INTEGER NOT NULL,
                 priority INTEGER NOT NULL, notify INTEGER NOT NULL, created_at INTEGER NOT NULL,
                 deleted INTEGER NOT NULL DEFAULT 0);
                 INSERT INTO tasks VALUES (1, 'Old', '', '{\"Date\":{\"date\":\"2026-11-05\",\"repeat\":\"Never\"}}', 600, 30, 1, 1, 0, 0);",
            )
            .unwrap();
        }
        let mut s = Store::open(&path).unwrap();
        assert_eq!(s.run(1).unwrap().added_secs, 0);
        assert_eq!(s.task(1).unwrap().notify_ahead_min, 0, "old tasks have no announcement");
        s.add_time(1, 600).unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(s.run(1).unwrap().added_secs, 600, "persisted");
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn damaged_rows_still_load() {
        let path = temp_db("damaged");
        {
            let mut s = Store::open(&path).unwrap();
            let id = s
                .save_task(test_task("Broken", Schedule::Date { date: day(11, 5), repeat: Repeat::Never }, 600, 30))
                .unwrap();
            s.start(id, day(11, 5), 100).unwrap();
            s.conn.execute_batch("UPDATE tasks SET schedule='not json'; UPDATE runs SET date='05.11.2026';").unwrap();
        }
        // Both are reported in the log; the data stays usable.
        let s = Store::open(&path).unwrap();
        assert!(matches!(s.tasks[0].schedule, Schedule::Date { repeat: Repeat::Never, .. }));
        assert_eq!(s.runs[0].date, NaiveDate::default());
        assert_eq!(s.runs[0].tracked_secs(160), 60, "the tracked time is kept");
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn damaged_numbers_still_load() {
        let path = temp_db("numbers");
        let (a, b) = {
            let mut s = Store::open(&path).unwrap();
            let once = Schedule::Date { date: day(11, 5), repeat: Repeat::Never };
            let a = s.save_task(test_task("A", once.clone(), 600, 30)).unwrap();
            let b = s.save_task(test_task("B", once, 700, 30)).unwrap();
            let run = s.start(a, day(11, 5), 1_000).unwrap();
            s.pause(run, 1_600).unwrap();
            s.resume(run, 2_000).unwrap();
            s.pause(run, 2_300).unwrap();
            s.resume(run, 3_000).unwrap();
            s.set_notif("due:2:2026-11-05", NotifState { dismissed: true, snooze_until: None }).unwrap();
            // Values the app never writes: out of range, or not numbers at all (SQLite
            // keeps text that doesn't look like a number as text, even in an INTEGER column).
            s.conn
                .execute_batch(&format!(
                    "UPDATE tasks SET start_min=-5, estimate_min=999999, notify_ahead='soon', priority='high' WHERE id={a};
                     UPDATE tasks SET start_min=99999, notify='loud', created_at=1.5, deleted='no' WHERE id={b};
                     UPDATE runs SET added_secs=-100, estimate_min='long', finished_at='later' WHERE id={run};
                     UPDATE segments SET end='then' WHERE start=1000;
                     UPDATE segments SET start='once' WHERE start=2000;
                     UPDATE notif_state SET dismissed='maybe', snooze_until='soon';"
                ))
                .unwrap();
            (a, b)
        };
        let s = Store::open(&path).expect("damaged numbers don't keep the app from starting");
        let ta = s.task(a).unwrap();
        assert_eq!((ta.start_min, ta.estimate_min, ta.notify_ahead_min), (0, MAX_ESTIMATE_MIN, 0), "limited");
        assert_eq!(ta.priority, Priority::Normal);
        let tb = s.task(b).expect("not deleted");
        assert_eq!(
            (tb.start_min, tb.notify, tb.created_at),
            (DAY_MIN - 1, NotifyMode::Sound, 0),
            "a reminder stays on"
        );
        let r = s.runs_of(a).next().unwrap();
        assert_eq!((r.added_secs, r.estimate_min, r.finished_at, r.status), (0, 30, None, RunStatus::Running));
        // the first segment ends where it began, the one without a start is left out, the
        // running one goes on
        assert_eq!(r.segments.len(), 2);
        assert_eq!(r.tracked_secs(3_100), 100);
        assert!(!s.notif("due:2:2026-11-05").dismissed, "the reminder comes back rather than being lost");
        assert_eq!(s.notif("due:2:2026-11-05").snooze_until, None);
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn settings_roundtrip() {
        use crate::settings::{Corner, SoundChoice};
        let path = temp_db("settings");
        {
            let s = Store::open(&path).unwrap();
            assert_eq!(s.load_settings(), Settings::default(), "first start: the defaults");
            let set = Settings {
                corner: Corner::TopLeft,
                sound: SoundChoice::File,
                sound_file: "/home/me/alarm.ogg".into(),
                time_presets: vec![10, 20],
                ui_scale: 1.5,
                ..Settings::default()
            };
            s.save_settings(&set).unwrap();
        }
        let s = Store::open(&path).unwrap();
        let set = s.load_settings();
        assert_eq!(
            (set.corner, set.sound, set.sound_file.as_str()),
            (Corner::TopLeft, SoundChoice::File, "/home/me/alarm.ogg")
        );
        assert_eq!((set.time_presets.as_slice(), set.ui_scale), (&[10, 20][..], 1.5));
        // values from outside the allowed ranges are limited
        s.save_settings(&Settings { ui_scale: 9.0, ahead_close_secs: 0, ..set.clone() }).unwrap();
        assert_eq!((s.load_settings().ui_scale, s.load_settings().ahead_close_secs), (2.0, 1));
        s.save_settings(&Settings { ahead_close_secs: 100_000, ..set }).unwrap();
        assert_eq!(s.load_settings().ahead_close_secs, 300);
        // settings that can't be read: the defaults, instead of not starting
        s.conn.execute("UPDATE settings SET value='not json' WHERE key='settings'", []).unwrap();
        assert_eq!(s.load_settings(), Settings::default());
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn window_geometry_roundtrip() {
        use crate::settings::WindowGeom;
        let s = Store::open(Path::new(":memory:")).unwrap();
        assert_eq!(s.load_window(), None);
        let g = WindowGeom { x: -1920.0, y: 40.0, w: 1400.0, h: 880.0, maximized: true };
        s.save_window(&g).unwrap();
        assert_eq!(s.load_window(), Some(g));
        // nonsense sizes are ignored (default size instead)
        s.save_window(&WindowGeom { w: 0.0, ..g }).unwrap();
        assert_eq!(s.load_window(), None);
    }

    #[test]
    fn virtual_then_real() {
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        let id = s
            .save_task(Task {
                id: 0,
                title: "Standup".into(),
                notes: String::new(),
                schedule: Schedule::Weekdays { mask: 0b11111, from: day, every: WeekRepeat::Weekly },
                start_min: 9 * 60,
                estimate_min: 15,
                priority: Priority::Normal,
                notify: NotifyMode::Sound,
                notify_ahead_min: 10,
                created_at: 0,
                deleted: false,
            })
            .unwrap();
        assert_eq!(s.entries_for(day, 0)[0].status, EntryStatus::Virtual);
        assert_eq!(s.entries_for(day, 0)[0].notify_ahead_min, 10);
        assert!(s.runs.is_empty(), "virtual tasks create no rows");

        let run = s.start(id, day, 100).unwrap();
        s.pause(run, 160).unwrap();
        s.resume(run, 200).unwrap();
        s.finish(run, 230).unwrap();
        s.add_time(run, 60).unwrap();
        assert_eq!(s.run(run).unwrap().tracked_secs(300), 60 + 30 + 60);
        assert_eq!(s.run(run).unwrap().first_start(), Some(40), "added time counts as an earlier start");
        let e = &s.entries_for(day, 1000)[0];
        assert_eq!(e.status, EntryStatus::Done);
        assert_eq!(e.tracked_secs, 150, "60 s tracked + 30 s + 60 s added");
        s.reset_added_time(run).unwrap();
        assert_eq!(s.run(run).unwrap().tracked_secs(300), 90, "reset keeps the real time");
        assert_eq!(s.run(run).unwrap().first_start(), Some(100));
        s.resume(run, 100 + 86_400).unwrap();
        s.finish(run, 160 + 86_400).unwrap();
        assert_eq!(s.run(run).unwrap().first_start(), Some(100), "resumed the next day: still the first start");

        // Editing the virtual task doesn't touch the real one.
        let mut t = s.task(id).unwrap().clone();
        t.title = "Daily".into();
        s.save_task(t).unwrap();
        assert_eq!(s.entries_for(day, 0)[0].title, "Standup");
        let next = day.succ_opt().unwrap();
        assert_eq!(s.entries_for(next, 0)[0].title, "Daily");
        assert_eq!(s.count_shown_on(day), 1);
    }

    fn day(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    fn temp_db(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("tasker-{name}-{}.db", std::process::id()));
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
        path
    }

    #[test]
    fn finished_runs_stay_finished() {
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        let id = s
            .save_task(test_task("Report", Schedule::Date { date: day(11, 5), repeat: Repeat::Never }, 600, 30))
            .unwrap();
        let run = s.start(id, day(11, 5), 100).unwrap();
        s.finish(run, 200).unwrap();
        // stale actions (e.g. from an old panel menu or a reminder) must not change it
        s.pause(run, 300).unwrap();
        s.finish(run, 400).unwrap();
        let r = s.run(run).unwrap();
        assert_eq!(r.status, RunStatus::Done);
        assert_eq!(r.finished_at, Some(200), "the first finish time is kept");
        assert_eq!(r.tracked_secs(1000), 100);
        // resuming a finished run starts a new segment; pausing twice closes it once
        s.resume(run, 500).unwrap();
        s.resume(run, 550).unwrap();
        assert_eq!(s.run(run).unwrap().segments.len(), 2, "resume while running is a no-op");
        s.pause(run, 560).unwrap();
        s.pause(run, 900).unwrap();
        let r = s.run(run).unwrap();
        assert_eq!((r.status, r.finished_at), (RunStatus::Paused, None));
        assert_eq!(r.tracked_secs(1000), 160);
        // starting it again from the day view continues the same run
        assert_eq!(s.start(id, day(11, 5), 1000).unwrap(), run);
        assert_eq!(s.runs.len(), 1);
    }

    #[test]
    fn giving_up_creates_a_failed_run() {
        let path = temp_db("failed");
        let mut s = Store::open(&path).unwrap();
        let id = s
            .save_task(test_task("Taxes", Schedule::Date { date: day(11, 5), repeat: Repeat::Never }, 600, 30))
            .unwrap();
        let run = s.fail(id, day(11, 5), 100).unwrap();
        let e = s.entry(id, day(11, 5), 500).unwrap();
        assert_eq!((e.status, e.run_id, e.tracked_secs), (EntryStatus::Failed, Some(run), 0));
        assert_eq!(s.run(run).unwrap().finished_at, Some(100));
        assert_eq!(s.open_runs().count(), 0, "nothing to pause or finish");
        // stale actions must not turn it into something else
        s.finish(run, 200).unwrap();
        s.pause(run, 200).unwrap();
        assert_eq!(s.fail(id, day(11, 5), 300).unwrap(), run, "giving up twice is the same run");
        assert_eq!((s.run(run).unwrap().status, s.run(run).unwrap().finished_at), (RunStatus::Failed, Some(100)));
        drop(s);
        let mut s = Store::open(&path).unwrap();
        assert_eq!(s.run(run).unwrap().status, RunStatus::Failed, "persisted");
        // doing it after all: the clock starts, and it can be finished like any other
        s.resume(run, 1_000).unwrap();
        assert_eq!(s.open_runs().map(|r| r.id).collect::<Vec<_>>(), [run]);
        s.finish(run, 1_600).unwrap();
        let r = s.run(run).unwrap();
        assert_eq!((r.status, r.tracked_secs(9_999), r.finished_at), (RunStatus::Done, 600, Some(1_600)));
        assert_eq!(s.open_runs().count(), 0);
        // a started task can't be given up this way
        let id2 = s
            .save_task(test_task("Report", Schedule::Date { date: day(11, 5), repeat: Repeat::Never }, 700, 30))
            .unwrap();
        let started = s.start(id2, day(11, 5), 2_000).unwrap();
        assert_eq!(s.fail(id2, day(11, 5), 2_100).unwrap(), started);
        assert_eq!(s.run(started).unwrap().status, RunStatus::Running);
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn undo_makes_it_a_planned_task_again() {
        let path = temp_db("undo");
        let mut s = Store::open(&path).unwrap();
        let once = |title: &str| test_task(title, Schedule::Date { date: day(11, 5), repeat: Repeat::Never }, 600, 30);
        let (a, b, c) =
            (s.save_task(once("A")).unwrap(), s.save_task(once("B")).unwrap(), s.save_task(once("C")).unwrap());
        let run_a = s.start(a, day(11, 5), 100).unwrap();
        let run_b = s.start(b, day(11, 5), 110).unwrap();
        let run_c = s.fail(c, day(11, 5), 120).unwrap();
        // B was started from its reminder (which closes it for good), A's reminder is snoozed
        let closed = NotifState { dismissed: true, snooze_until: None };
        s.set_notif(&format!("over:{run_b}"), closed).unwrap();
        s.set_notif(&format!("due:{b}:2026-11-05"), closed).unwrap();
        s.set_notif(&format!("due:{a}:2026-11-05"), NotifState { dismissed: false, snooze_until: Some(900) }).unwrap();
        // a started one from the middle, with the others' lookups staying right
        s.undo_run(run_b).unwrap();
        assert!(s.run(run_b).is_none());
        let e = s.entry(b, day(11, 5), 200).unwrap();
        assert_eq!((e.status, e.run_id, e.tracked_secs), (EntryStatus::Virtual, None, 0), "planned again");
        assert!(!s.notif(&format!("over:{run_b}")).dismissed, "its reminder state is gone too");
        assert!(!s.notif(&format!("due:{b}:2026-11-05")).dismissed, "and it is due again");
        assert_eq!(s.run(run_a).unwrap().title, "A");
        assert_eq!(s.run(run_c).unwrap().title, "C");
        assert_eq!(s.open_runs().map(|r| r.id).collect::<Vec<_>>(), [run_a]);
        assert_eq!((s.runs_on(day(11, 5)).count(), s.runs_of(b).count(), s.count_shown_on(day(11, 5))), (2, 0, 3));
        // a given-up one, and undoing twice does nothing
        s.undo_run(run_c).unwrap();
        s.undo_run(run_a).unwrap();
        assert_eq!(s.notif(&format!("due:{a}:2026-11-05")).snooze_until, Some(900), "a snooze is kept");
        let run_a = s.start(a, day(11, 5), 100).unwrap();
        s.undo_run(run_c).unwrap();
        assert_eq!(s.entry(c, day(11, 5), 200).unwrap().status, EntryStatus::Virtual);
        // it can be started again, and the others still work
        let again = s.start(b, day(11, 5), 300).unwrap();
        s.pause(run_a, 400).unwrap();
        assert_eq!(s.run(run_a).unwrap().tracked_secs(999), 300);
        assert_eq!(s.run(again).unwrap().tracked_secs(360), 60, "nothing left of the undone run's time");
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(s.runs.len(), 2, "deleted in the database too");
        assert_eq!(s.run(again).unwrap().segments.len(), 1);
        assert_eq!(s.entry(c, day(11, 5), 0).unwrap().status, EntryStatus::Virtual);
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tasks_across_midnight_are_on_both_days() {
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        let every_day = Schedule::Weekdays { mask: 0b111_1111, from: day(11, 5), every: WeekRepeat::Weekly };
        let night = s.save_task(test_task("Night shift", every_day, 23 * 60, 120)).unwrap();
        s.save_task(test_task("Lunch", Schedule::Date { date: day(11, 6), repeat: Repeat::Never }, 12 * 60, 60))
            .unwrap();
        let shown = |s: &Store, d: NaiveDate| -> Vec<(String, NaiveDate, (u32, u32))> {
            s.entries_shown_on(d, 0).iter().map(|e| (e.title.clone(), e.date, e.span_on(d))).collect()
        };
        let night_shift = |date: NaiveDate, span: (u32, u32)| ("Night shift".to_string(), date, span);
        assert_eq!(shown(&s, day(11, 5)), [night_shift(day(11, 5), (23 * 60, 24 * 60))], "the first day: only its own");
        assert_eq!(
            shown(&s, day(11, 6)),
            [
                night_shift(day(11, 5), (0, 60)),
                ("Lunch".to_string(), day(11, 6), (12 * 60, 13 * 60)),
                night_shift(day(11, 6), (23 * 60, 24 * 60)),
            ],
            "yesterday's rest first, then the day's own"
        );
        assert_eq!(s.entries_for(day(11, 6), 0).len(), 2, "but it belongs to one day only");
        // the carried-over one is the same occurrence: starting it shows on both days
        s.start(night, day(11, 5), 100).unwrap();
        let e = s.entries_shown_on(day(11, 6), 200);
        assert_eq!((e[0].date, e[0].status), (day(11, 5), EntryStatus::Running));
        assert_eq!(e[2].status, EntryStatus::Virtual);
        for d in 4..=8 {
            assert_eq!(s.count_shown_on(day(11, d)), s.entries_shown_on(day(11, d), 0).len(), "count on day {d}");
        }
        // one that ends at midnight stays on its day
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        s.save_task(test_task("Late", Schedule::Date { date: day(11, 5), repeat: Repeat::Never }, 23 * 60, 60))
            .unwrap();
        assert_eq!((s.count_shown_on(day(11, 5)), s.count_shown_on(day(11, 6))), (1, 0));

        // the longest estimates reach two days further
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        s.save_task(test_task("Trip", Schedule::Date { date: day(11, 5), repeat: Repeat::Never }, 23 * 60, 47 * 60))
            .unwrap();
        for (d, span) in [(5, (23 * 60, 24 * 60)), (6, (0, 24 * 60)), (7, (0, 22 * 60))] {
            let e = s.entries_shown_on(day(11, d), 0);
            assert_eq!((e.len(), e[0].span_on(day(11, d))), (1, span), "day {d}");
            assert_eq!(s.count_shown_on(day(11, d)), 1, "count on day {d}");
        }
        assert_eq!((s.entries_shown_on(day(11, 8), 0).len(), s.count_shown_on(day(11, 8))), (0, 0));
    }

    #[test]
    fn lookups_follow_task_changes() {
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        let once = Schedule::Date { date: day(11, 5), repeat: Repeat::Never };
        let id = s.save_task(test_task("Move me", once, 600, 30)).unwrap();
        assert_eq!((s.count_shown_on(day(11, 5)), s.count_shown_on(day(11, 9))), (1, 0));
        // moved to another day, then made a repeating task, then deleted
        let mut t = s.task(id).unwrap().clone();
        t.schedule = Schedule::Date { date: day(11, 9), repeat: Repeat::Never };
        s.save_task(t.clone()).unwrap();
        assert_eq!((s.count_shown_on(day(11, 5)), s.count_shown_on(day(11, 9))), (0, 1));
        t.schedule = Schedule::Weekdays { mask: 0b111_1111, from: day(11, 9), every: WeekRepeat::Weekly };
        s.save_task(t).unwrap();
        assert_eq!((s.count_shown_on(day(11, 9)), s.count_shown_on(day(11, 20))), (1, 1));
        assert_eq!(s.tasks().count(), 1);
        s.delete_task(id).unwrap();
        assert_eq!((s.count_shown_on(day(11, 9)), s.tasks().count()), (0, 0));
        assert!(s.task(id).is_none() && s.entry(id, day(11, 9), 0).is_none());
    }

    #[test]
    fn everything_survives_a_restart() {
        let path = temp_db("reopen");
        let (id, run) = {
            let mut s = Store::open(&path).unwrap();
            let mut t = test_task(
                "Gym",
                Schedule::Weekdays { mask: 0b1_0000, from: day(11, 2), every: WeekRepeat::Weekly },
                18 * 60,
                60,
            );
            t.notify_ahead_min = 15;
            t.priority = Priority::High;
            let id = s.save_task(t).unwrap();
            let run = s.start(id, day(11, 6), 1_000).unwrap();
            s.pause(run, 1_600).unwrap();
            s.resume(run, 2_000).unwrap();
            s.add_time(run, 300).unwrap();
            s.set_notif("over:1", NotifState { dismissed: false, snooze_until: Some(5_000) }).unwrap();
            (id, run)
        };
        let s = Store::open(&path).unwrap();
        let t = s.task(id).unwrap();
        assert_eq!((t.title.as_str(), t.notify_ahead_min, t.priority), ("Gym", 15, Priority::High));
        let r = s.run(run).unwrap();
        assert_eq!(r.status, RunStatus::Running);
        assert_eq!(r.segments.len(), 2);
        assert_eq!(r.tracked_secs(2_100), 600 + 100 + 300);
        assert_eq!(s.notif("over:1").snooze_until, Some(5_000));
        assert_eq!(s.runs_on(day(11, 6)).count(), 1, "date index rebuilt on load");
        assert_eq!(s.open_runs().map(|r| r.id).collect::<Vec<_>>(), [run], "and the open runs");
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn day_lists_mix_runs_and_planned_tasks() {
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        let daily = s
            .save_task(test_task(
                "Mail",
                Schedule::Weekdays { mask: 0b111_1111, from: day(11, 1), every: WeekRepeat::Weekly },
                8 * 60,
                30,
            ))
            .unwrap();
        let once = s
            .save_task(test_task("Dentist", Schedule::Date { date: day(11, 6), repeat: Repeat::Never }, 10 * 60, 60))
            .unwrap();
        s.start(daily, day(11, 6), 100).unwrap();
        for d in 1..=10 {
            let date = day(11, d);
            assert_eq!(s.count_shown_on(date), s.entries_shown_on(date, 0).len(), "the count matches on day {d}");
        }
        let e = s.entries_for(day(11, 6), 200);
        assert_eq!(
            e.iter().map(|e| (e.title.as_str(), e.status)).collect::<Vec<_>>(),
            [("Mail", EntryStatus::Running), ("Dentist", EntryStatus::Virtual)]
        );
        assert_eq!(s.entries_for(day(11, 7), 0).len(), 1, "the one-off is only on its day");
        assert_eq!(s.count_shown_on(day(10, 31)), 0, "nothing before the schedule starts");

        // Deleting a task keeps its history but stops planning it.
        s.delete_task(daily).unwrap();
        let e = s.entries_for(day(11, 6), 200);
        assert_eq!(e.len(), 2);
        assert!(!e[0].task_exists, "run of a deleted task is shown without edit buttons");
        assert_eq!(s.count_shown_on(day(11, 8)), 0);
        assert!(s.start(daily, day(11, 8), 300).is_err(), "deleted tasks can't be started");
        assert!(s.task(once).is_some());
    }

    #[test]
    fn old_reminder_state_is_pruned() {
        let path = temp_db("prune");
        {
            let mut s = Store::open(&path).unwrap();
            let id = s
                .save_task(test_task("T", Schedule::Date { date: day(10, 1), repeat: Repeat::Never }, 600, 10))
                .unwrap();
            let old_run = s.start(id, day(10, 1), 100).unwrap();
            s.finish(old_run, 200).unwrap();
            let id2 = s
                .save_task(test_task("U", Schedule::Date { date: day(10, 2), repeat: Repeat::Never }, 600, 10))
                .unwrap();
            let open_run = s.start(id2, day(10, 2), 100).unwrap();
            let st = NotifState { dismissed: true, snooze_until: None };
            for key in [
                "due:1:2026-10-20".to_string(),
                "ahead:1:2026-10-20".into(),
                "due:1:2026-11-01".into(),
                format!("over:{old_run}"),
                format!("over:{open_run}"),
                "over:999".into(),
                "something-else".into(),
            ] {
                s.set_notif(&key, st).unwrap();
            }
            assert_eq!(s.prune_notif_state(day(11, 6)).unwrap(), 4);
            let mut kept: Vec<&String> = s.notif_state.keys().collect();
            kept.sort();
            assert_eq!(kept, ["due:1:2026-11-01", "over:2", "something-else"]);
        }
        let s = Store::open(&path).unwrap();
        assert!(!s.notif_state.contains_key("due:1:2026-10-20"), "deleted in the database too");
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    /// One frame's store queries (month grid, week list, day view, reminders, panel icon)
    /// with 3 years of history for 20 daily tasks plus one finished one-off task per day.
    /// Must not grow with the history (it did before the lookups in `Index`).
    /// `cargo test --release frame_queries -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn frame_queries_with_long_history() {
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        let from = NaiveDate::from_ymd_opt(2023, 11, 6).unwrap();
        let every_day = Schedule::Weekdays { mask: 0b111_1111, from, every: WeekRepeat::Weekly };
        let ids: Vec<i64> = (0..20)
            .map(|i| s.save_task(test_task(&format!("Task {i}"), every_day.clone(), 8 * 60 + i * 20, 15)).unwrap())
            .collect();
        let today = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        let mut d = from;
        while d < today - Duration::days(30) {
            let once = s.save_task(test_task("Once", Schedule::Date { date: d, repeat: Repeat::Never }, 600, 15));
            for id in ids.iter().copied().chain(once) {
                let r = s.start(id, d, 1000).unwrap();
                s.finish(r, 1600).unwrap();
            }
            d += Duration::days(1);
        }
        let frames = 100;
        let t0 = std::time::Instant::now();
        for _ in 0..frames {
            let first = today - Duration::days(20);
            let cells: usize = (0..42).map(|i| s.count_shown_on(first + Duration::days(i))).sum();
            let rows: usize = (0..8).map(|i| s.entries_shown_on(today - Duration::days(i), 0).len()).sum();
            // the reminder check and the panel icon's list
            let open = s.open_runs().count()
                + (-1..=1).map(|i| s.entries_for(today + Duration::days(i), 0).len()).sum::<usize>();
            let found = ids.iter().filter(|&&id| s.task(id).is_some() && s.run(id).is_some()).count();
            assert_eq!((cells, rows, open, found), (42 * 20, 8 * 20, 3 * 20, 20));
        }
        println!("{} runs: {:?} per frame", s.runs.len(), t0.elapsed() / frames);
    }
}
