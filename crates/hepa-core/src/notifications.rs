use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Terminal outcomes are the only states HEPA notifies on. There is deliberately
/// no "started"/"branch created" variant, so progress chatter cannot be emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaNotificationStatus {
    Done,
    Blocked,
}

impl HepaNotificationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            HepaNotificationStatus::Done => "done",
            HepaNotificationStatus::Blocked => "blocked",
        }
    }
}

/// A terminal notification for one task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaNotification {
    pub task_id: String,
    pub status: HepaNotificationStatus,
}

impl HepaNotification {
    pub fn new(task_id: impl Into<String>, status: HepaNotificationStatus) -> Self {
        Self {
            task_id: task_id.into(),
            status,
        }
    }
}

/// Outcome of attempting to emit a notification through the dedupe log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaNotificationOutcome {
    Emitted,
    Deduped,
}

/// Records which terminal `(task, status)` notifications have already fired so a
/// task notifies exactly once per terminal state.
#[derive(Debug, Clone, Default)]
pub struct HepaNotificationLog {
    seen: BTreeSet<(String, &'static str)>,
}

impl HepaNotificationLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Emit a terminal notification the first time a `(task, status)` is seen,
    /// and dedupe every repeat.
    pub fn record(&mut self, notification: &HepaNotification) -> HepaNotificationOutcome {
        let key = (notification.task_id.clone(), notification.status.as_str());
        if self.seen.insert(key) {
            HepaNotificationOutcome::Emitted
        } else {
            HepaNotificationOutcome::Deduped
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_notifies_exactly_once_at_a_terminal_state() {
        let mut log = HepaNotificationLog::new();
        let done = HepaNotification::new("task-1", HepaNotificationStatus::Done);

        assert_eq!(log.record(&done), HepaNotificationOutcome::Emitted);
        assert_eq!(log.record(&done), HepaNotificationOutcome::Deduped);
        assert_eq!(log.record(&done), HepaNotificationOutcome::Deduped);
    }

    #[test]
    fn distinct_tasks_each_notify_once() {
        let mut log = HepaNotificationLog::new();

        assert_eq!(
            log.record(&HepaNotification::new(
                "task-1",
                HepaNotificationStatus::Blocked
            )),
            HepaNotificationOutcome::Emitted
        );
        assert_eq!(
            log.record(&HepaNotification::new(
                "task-2",
                HepaNotificationStatus::Done
            )),
            HepaNotificationOutcome::Emitted
        );
    }
}
