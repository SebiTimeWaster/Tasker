//! What the user can do with a task: from its row in the day view, a reminder window or
//! the panel icon's menu.

use chrono::NaiveDate;

/// Something the user clicked on a task (in the list or in a notification).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start {
        task_id: i64,
        date: NaiveDate,
    },
    /// "I am bad and won't do it": give the occurrence up. Like `Start` it creates the real
    /// task, but marked as failed.
    Fail {
        task_id: i64,
        date: NaiveDate,
    },
    /// Take a start or give-up back: the run is removed and the task is planned again.
    Undo(i64),
    Pause(i64),
    Finish(i64),
    Resume(i64),
    /// Add tracked time (seconds) for work done before the task was started.
    AddTime(i64, i64),
    /// Remove all added time again.
    ResetAddedTime(i64),
    Edit(i64),
    Stats(i64),
}
