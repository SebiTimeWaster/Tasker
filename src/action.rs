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
