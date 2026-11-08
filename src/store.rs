//! SQLite persistence plus an in-memory copy of all data (it is small), written through on change.
//!
//! Every change is written to the database first (multi-step changes in one transaction)
//! and only then applied to the in-memory copy, so the two can't drift apart on errors.

use std::collections::HashMap;
use std::path::Path;

use chrono::{Duration, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension};

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
    pub tasks: Vec<Task>,
    /// All runs ever (append-only: runs are never removed, so indices stay valid).
    pub runs: Vec<Run>,
    pub notif_state: HashMap<String, NotifState>,
    /// Indices into `runs` by date. The UI asks for single days many times per frame;
    /// this keeps that independent of how much history has piled up.
    by_date: HashMap<NaiveDate, Vec<usize>>,
}

/// Reminder state older than this many days is deleted at startup.
const NOTIF_STATE_KEEP_DAYS: i64 = 7;

type Res<T> = rusqlite::Result<T>;

/// Bring databases from older versions up to the current schema.
fn migrate(conn: &Connection) -> Res<()> {
    let has_added: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('runs') WHERE name='added_secs'")?
        .exists([])?;
    if !has_added {
        conn.execute_batch("ALTER TABLE runs ADD COLUMN added_secs INTEGER NOT NULL DEFAULT 0;")?;
    }
    let has_ahead: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('tasks') WHERE name='notify_ahead'")?
        .exists([])?;
    if !has_ahead {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN notify_ahead INTEGER NOT NULL DEFAULT 0;")?;
    }
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Res<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        let mut s = Store { conn, tasks: vec![], runs: vec![], notif_state: HashMap::new(), by_date: HashMap::new() };
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
                let sched: String = r.get(3)?;
                Ok(Task {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    notes: r.get(2)?,
                    schedule: serde_json::from_str::<Schedule>(&sched)
                        .unwrap_or(Schedule::Date { date: chrono::Local::now().date_naive(), repeat: Repeat::Never })
                        .normalized(),
                    start_min: r.get(4)?,
                    estimate_min: r.get(5)?,
                    priority: Priority::from_i64(r.get(6)?),
                    notify: NotifyMode::from_i64(r.get(7)?),
                    created_at: r.get(8)?,
                    deleted: r.get::<_, i64>(9)? != 0,
                    notify_ahead_min: r.get(10)?,
                })
            })?
            .collect::<Res<_>>()?;
        drop(st);

        let mut segs: HashMap<i64, Vec<Segment>> = HashMap::new();
        let mut st = self.conn.prepare("SELECT id,run_id,start,end FROM segments ORDER BY start")?;
        for row in st.query_map([], |r| {
            Ok((r.get::<_, i64>(1)?, Segment { id: r.get(0)?, start: r.get(2)?, end: r.get(3)? }))
        })? {
            let (run_id, seg) = row?;
            segs.entry(run_id).or_default().push(seg);
        }
        drop(st);

        let mut st = self.conn.prepare(
            "SELECT id,task_id,date,title,notes,start_min,estimate_min,priority,notify,recurring,status,created_at,finished_at,added_secs FROM runs",
        )?;
        self.runs = st
            .query_map([], |r| {
                let date: String = r.get(2)?;
                let id: i64 = r.get(0)?;
                Ok(Run {
                    id,
                    task_id: r.get(1)?,
                    date: NaiveDate::parse_from_str(&date, "%Y-%m-%d").unwrap_or_default(),
                    title: r.get(3)?,
                    notes: r.get(4)?,
                    start_min: r.get(5)?,
                    estimate_min: r.get(6)?,
                    priority: Priority::from_i64(r.get(7)?),
                    notify: NotifyMode::from_i64(r.get(8)?),
                    recurring: r.get::<_, i64>(9)? != 0,
                    status: RunStatus::from_i64(r.get(10)?),
                    created_at: r.get(11)?,
                    finished_at: r.get(12)?,
                    added_secs: r.get(13)?,
                    segments: segs.remove(&id).unwrap_or_default(),
                })
            })?
            .collect::<Res<_>>()?;
        drop(st);
        self.by_date.clear();
        for (i, r) in self.runs.iter().enumerate() {
            self.by_date.entry(r.date).or_default().push(i);
        }

        let mut st = self.conn.prepare("SELECT key,dismissed,snooze_until FROM notif_state")?;
        self.notif_state = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    NotifState { dismissed: r.get::<_, i64>(1)? != 0, snooze_until: r.get(2)? },
                ))
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
        self.tasks.iter().find(|t| t.id == id && !t.deleted)
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
            Ok(id)
        } else {
            self.conn.execute(
                "UPDATE tasks SET title=?1,notes=?2,schedule=?3,start_min=?4,estimate_min=?5,priority=?6,notify=?7,notify_ahead=?8 WHERE id=?9",
                params![t.title, t.notes, sched, t.start_min, t.estimate_min, t.priority as i64, t.notify as i64, t.notify_ahead_min, t.id],
            )?;
            let id = t.id;
            if let Some(slot) = self.tasks.iter_mut().find(|x| x.id == id) {
                *slot = t;
            }
            Ok(id)
        }
    }

    /// Soft delete: runs (history, statistics) are kept.
    pub fn delete_task(&mut self, id: i64) -> Res<()> {
        self.conn.execute("UPDATE tasks SET deleted=1 WHERE id=?1", params![id])?;
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.deleted = true;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- entries

    /// Everything to show on a given day: real runs of that day plus virtual occurrences
    /// that have not been started yet. Sorted by start time.
    pub fn entries_for(&self, date: NaiveDate, now: i64) -> Vec<Entry> {
        let mut out: Vec<Entry> = self.runs_on(date).map(|r| Entry::from_run(r, self.task(r.task_id), now)).collect();
        let started = out.len();
        for t in self.virtual_tasks_on(date) {
            if !out[..started].iter().any(|e| e.task_id == t.id) {
                out.push(Entry::from_task(t, date));
            }
        }
        out.sort_by(|a, b| a.start_min.cmp(&b.start_min).then(b.priority.cmp(&a.priority)).then(a.title.cmp(&b.title)));
        out
    }

    /// Number of entries [`Store::entries_for`] would return, without building them.
    pub fn count_for(&self, date: NaiveDate) -> usize {
        let run_tasks: Vec<i64> = self.runs_on(date).map(|r| r.task_id).collect();
        let virt = self.virtual_tasks_on(date).filter(|t| !run_tasks.contains(&t.id)).count();
        run_tasks.len() + virt
    }

    /// The runs (started occurrences) of one day.
    pub fn runs_on(&self, date: NaiveDate) -> impl Iterator<Item = &Run> {
        self.by_date.get(&date).into_iter().flatten().map(|&i| &self.runs[i])
    }

    /// Tasks that occur on `date` according to their schedule (started or not).
    fn virtual_tasks_on(&self, date: NaiveDate) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(move |t| !t.deleted && t.schedule.occurs_on(date))
    }

    pub fn run(&self, id: i64) -> Option<&Run> {
        self.runs.iter().find(|r| r.id == id)
    }

    fn run_index(&self, id: i64) -> Option<usize> {
        self.runs.iter().position(|r| r.id == id)
    }

    // ---------------------------------------------------------------- time tracking

    /// Start a virtual occurrence (creates the real task) or continue a paused/finished run.
    pub fn start(&mut self, task_id: i64, date: NaiveDate, now: i64) -> Res<i64> {
        let existing = self.runs_on(date).find(|r| r.task_id == task_id).map(|r| r.id);
        if let Some(id) = existing {
            self.resume(id, now)?;
            return Ok(id);
        }
        let Some(t) = self.task(task_id).cloned() else {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        };
        let recurring = t.schedule.is_recurring();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO runs(task_id,date,title,notes,start_min,estimate_min,priority,notify,recurring,status,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![t.id, date.format("%Y-%m-%d").to_string(), t.title, t.notes, t.start_min, t.estimate_min,
                    t.priority as i64, t.notify as i64, recurring as i64, RunStatus::Running as i64, now],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute("INSERT INTO segments(run_id,start) VALUES(?1,?2)", params![id, now])?;
        let seg = Segment { id: tx.last_insert_rowid(), start: now, end: None };
        tx.commit()?;
        self.by_date.entry(date).or_default().push(self.runs.len());
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
            status: RunStatus::Running,
            created_at: now,
            finished_at: None,
            added_secs: 0,
            segments: vec![seg],
        });
        Ok(id)
    }

    /// Continue a paused or finished run (a new time segment starts). No-op if running.
    pub fn resume(&mut self, run_id: i64, now: i64) -> Res<()> {
        let Some(idx) = self.run_index(run_id) else { return Ok(()) };
        if self.runs[idx].status == RunStatus::Running {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("INSERT INTO segments(run_id,start) VALUES(?1,?2)", params![run_id, now])?;
        let seg = Segment { id: tx.last_insert_rowid(), start: now, end: None };
        tx.execute("UPDATE runs SET status=?1, finished_at=NULL WHERE id=?2", params![RunStatus::Running as i64, run_id])?;
        tx.commit()?;
        let r = &mut self.runs[idx];
        r.segments.push(seg);
        r.status = RunStatus::Running;
        r.finished_at = None;
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

    /// Mark a run as done. No-op if it already is (keeps the original finish time).
    pub fn finish(&mut self, run_id: i64, now: i64) -> Res<()> {
        let Some(idx) = self.run_index(run_id) else { return Ok(()) };
        if self.runs[idx].status == RunStatus::Done {
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
        tx.execute("UPDATE runs SET status=?1, finished_at=?2 WHERE id=?3", params![status as i64, finished_at, run_id])?;
        tx.commit()?;
        let r = &mut self.runs[idx];
        if let Some(i) = open {
            r.segments[i].end = Some(now);
        }
        r.status = status;
        r.finished_at = finished_at;
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
    /// more than a week ago, and over-estimate state of old finished (or vanished) runs.
    /// Without this the table grows by a few rows every day, forever.
    pub fn prune_notif_state(&mut self, today: NaiveDate) -> Res<usize> {
        let cutoff = today - Duration::days(NOTIF_STATE_KEEP_DAYS);
        let stale = |key: &str| -> bool {
            let parts: Vec<&str> = key.split(':').collect();
            match parts.as_slice() {
                ["due" | "ahead", _, date] => NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok_and(|d| d < cutoff),
                ["over", id] => id.parse::<i64>().is_ok_and(|id| {
                    self.run(id).is_none_or(|r| r.status == RunStatus::Done && r.date < cutoff)
                }),
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

        // Editing the virtual task doesn't touch the real one.
        let mut t = s.task(id).unwrap().clone();
        t.title = "Daily".into();
        s.save_task(t).unwrap();
        assert_eq!(s.entries_for(day, 0)[0].title, "Standup");
        let next = day.succ_opt().unwrap();
        assert_eq!(s.entries_for(next, 0)[0].title, "Daily");
        assert_eq!(s.count_for(day), 1);
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
        let id = s.save_task(test_task("Report", Schedule::Date { date: day(11, 5), repeat: Repeat::Never }, 600, 30)).unwrap();
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
    fn everything_survives_a_restart() {
        let path = temp_db("reopen");
        let (id, run) = {
            let mut s = Store::open(&path).unwrap();
            let mut t = test_task("Gym", Schedule::Weekdays { mask: 0b1_0000, from: day(11, 2), every: WeekRepeat::Weekly }, 18 * 60, 60);
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
        drop(s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn day_lists_mix_runs_and_planned_tasks() {
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        let daily = s.save_task(test_task("Mail", Schedule::Weekdays { mask: 0b111_1111, from: day(11, 1), every: WeekRepeat::Weekly }, 8 * 60, 30)).unwrap();
        let once = s.save_task(test_task("Dentist", Schedule::Date { date: day(11, 6), repeat: Repeat::Never }, 10 * 60, 60)).unwrap();
        s.start(daily, day(11, 6), 100).unwrap();
        for d in 1..=10 {
            let date = day(11, d);
            assert_eq!(s.count_for(date), s.entries_for(date, 0).len(), "count_for matches on day {d}");
        }
        let e = s.entries_for(day(11, 6), 200);
        assert_eq!(e.iter().map(|e| (e.title.as_str(), e.status)).collect::<Vec<_>>(), [
            ("Mail", EntryStatus::Running),
            ("Dentist", EntryStatus::Virtual)
        ]);
        assert_eq!(s.entries_for(day(11, 7), 0).len(), 1, "the one-off is only on its day");
        assert_eq!(s.count_for(day(10, 31)), 0, "nothing before the schedule starts");

        // Deleting a task keeps its history but stops planning it.
        s.delete_task(daily).unwrap();
        let e = s.entries_for(day(11, 6), 200);
        assert_eq!(e.len(), 2);
        assert!(!e[0].task_exists, "run of a deleted task is shown without edit buttons");
        assert_eq!(s.count_for(day(11, 8)), 0);
        assert!(s.start(daily, day(11, 8), 300).is_err(), "deleted tasks can't be started");
        assert!(s.task(once).is_some());
    }

    #[test]
    fn old_reminder_state_is_pruned() {
        let path = temp_db("prune");
        {
            let mut s = Store::open(&path).unwrap();
            let id = s.save_task(test_task("T", Schedule::Date { date: day(10, 1), repeat: Repeat::Never }, 600, 10)).unwrap();
            let old_run = s.start(id, day(10, 1), 100).unwrap();
            s.finish(old_run, 200).unwrap();
            let id2 = s.save_task(test_task("U", Schedule::Date { date: day(10, 2), repeat: Repeat::Never }, 600, 10)).unwrap();
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

    /// One frame's store queries (month grid, week list, day view) with 3 years of
    /// history for 20 daily tasks. Must not grow with the history (it did before the
    /// date index: ~1 ms per frame here, now ~15 µs).
    /// `cargo test --release frame_queries -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn frame_queries_with_long_history() {
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        let from = NaiveDate::from_ymd_opt(2023, 11, 6).unwrap();
        let every_day = Schedule::Weekdays { mask: 0b111_1111, from, every: WeekRepeat::Weekly };
        let ids: Vec<i64> = (0..20).map(|i| s.save_task(test_task(&format!("Task {i}"), every_day.clone(), 8 * 60 + i * 20, 15)).unwrap()).collect();
        let today = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        let mut d = from;
        while d < today {
            for &id in &ids {
                let r = s.start(id, d, 1000).unwrap();
                s.finish(r, 1600).unwrap();
            }
            d += Duration::days(1);
        }
        let frames = 100;
        let t0 = std::time::Instant::now();
        for _ in 0..frames {
            let first = today - Duration::days(20);
            let cells: usize = (0..42).map(|i| s.count_for(first + Duration::days(i))).sum();
            let rows: usize = (0..8).map(|i| s.entries_for(today - Duration::days(i), 0).len()).sum();
            assert_eq!((cells, rows), (42 * 20, 8 * 20));
        }
        println!("{} runs: {:?} per frame", s.runs.len(), t0.elapsed() / frames);
    }
}
