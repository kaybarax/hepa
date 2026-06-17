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

    /// Whether a Hermes card's terminal status agrees with this notification, so
    /// card updates and terminal notifications never disagree.
    pub fn agrees_with_card_status(self, card_status: &str) -> bool {
        let normalized = card_status.trim().to_ascii_lowercase();
        match self {
            HepaNotificationStatus::Done => {
                matches!(
                    normalized.as_str(),
                    "done" | "completed" | "ready_for_human" | "ready"
                )
            }
            HepaNotificationStatus::Blocked => {
                matches!(normalized.as_str(), "blocked" | "failed")
            }
        }
    }
}

/// A terminal notification for one task.
/// A terminal notification carrying everything a human needs to act: which
/// project/task, the Hermes card and lane, the PR, the terminal status, and the
/// required human action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaNotification {
    pub project_id: String,
    pub task_id: String,
    pub card_id: Option<String>,
    pub lane_id: String,
    pub pr_url: Option<String>,
    pub status: HepaNotificationStatus,
    pub required_human_action: String,
}

impl HepaNotification {
    pub fn new(
        project_id: impl Into<String>,
        task_id: impl Into<String>,
        lane_id: impl Into<String>,
        status: HepaNotificationStatus,
        required_human_action: impl Into<String>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            task_id: task_id.into(),
            card_id: None,
            lane_id: lane_id.into(),
            pr_url: None,
            status,
            required_human_action: required_human_action.into(),
        }
    }

    pub fn with_card(mut self, card_id: impl Into<String>) -> Self {
        self.card_id = Some(card_id.into());
        self
    }

    pub fn with_pr_url(mut self, pr_url: impl Into<String>) -> Self {
        self.pr_url = Some(pr_url.into());
        self
    }

    /// Render a single sanitized line carrying every required field.
    pub fn render(&self) -> String {
        format!(
            "[{status}] project={project} task={task} card={card} lane={lane} pr={pr} action={action}",
            status = self.status.as_str(),
            project = self.project_id,
            task = self.task_id,
            card = self.card_id.as_deref().unwrap_or("none"),
            lane = self.lane_id,
            pr = self.pr_url.as_deref().unwrap_or("none"),
            action = self.required_human_action,
        )
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

/// A delivery transport for terminal notifications.
///
/// Transports are pluggable: the default is the local terminal, and optional
/// transports such as Telegram are ported by implementing this trait (no vendor
/// transport is bundled or privileged, and none receives manager credentials).
pub trait HepaNotificationSink {
    fn deliver(&mut self, notification: &HepaNotification) -> Result<(), String>;
}

/// In-memory sink that records delivered notifications, for tests and headless
/// runs.
#[derive(Debug, Default, Clone)]
pub struct HepaInMemoryNotificationSink {
    pub delivered: Vec<HepaNotification>,
}

impl HepaNotificationSink for HepaInMemoryNotificationSink {
    fn deliver(&mut self, notification: &HepaNotification) -> Result<(), String> {
        self.delivered.push(notification.clone());
        Ok(())
    }
}

/// Routes terminal notifications through the dedupe log to a sink so repeated
/// states are never delivered twice.
#[derive(Debug, Clone, Default)]
pub struct HepaNotifier {
    log: HepaNotificationLog,
}

impl HepaNotifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Deliver a notification to the sink only the first time its terminal
    /// `(task, status)` is seen; repeats are deduped and never delivered.
    pub fn notify(
        &mut self,
        notification: &HepaNotification,
        sink: &mut dyn HepaNotificationSink,
    ) -> Result<HepaNotificationOutcome, String> {
        match self.log.record(notification) {
            HepaNotificationOutcome::Emitted => {
                sink.deliver(notification)?;
                Ok(HepaNotificationOutcome::Emitted)
            }
            HepaNotificationOutcome::Deduped => Ok(HepaNotificationOutcome::Deduped),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done_notification(task_id: &str) -> HepaNotification {
        HepaNotification::new(
            "project-1",
            task_id,
            "lane-1",
            HepaNotificationStatus::Done,
            "Review and merge the PR.",
        )
    }

    #[test]
    fn task_notifies_exactly_once_at_a_terminal_state() {
        let mut log = HepaNotificationLog::new();
        let done = done_notification("task-1");

        assert_eq!(log.record(&done), HepaNotificationOutcome::Emitted);
        assert_eq!(log.record(&done), HepaNotificationOutcome::Deduped);
        assert_eq!(log.record(&done), HepaNotificationOutcome::Deduped);
    }

    #[test]
    fn distinct_tasks_each_notify_once() {
        let mut log = HepaNotificationLog::new();

        assert_eq!(
            log.record(&HepaNotification::new(
                "project-1",
                "task-1",
                "lane-1",
                HepaNotificationStatus::Blocked,
                "Resolve the merge conflict.",
            )),
            HepaNotificationOutcome::Emitted
        );
        assert_eq!(
            log.record(&done_notification("task-2")),
            HepaNotificationOutcome::Emitted
        );
    }

    #[test]
    fn notifier_delivers_once_and_dedupes_repeated_states() {
        let mut notifier = HepaNotifier::new();
        let mut sink = HepaInMemoryNotificationSink::default();
        let done = done_notification("task-1");

        assert_eq!(
            notifier.notify(&done, &mut sink).expect("first delivery"),
            HepaNotificationOutcome::Emitted
        );
        assert_eq!(
            notifier.notify(&done, &mut sink).expect("repeat deduped"),
            HepaNotificationOutcome::Deduped
        );
        assert_eq!(
            notifier.notify(&done, &mut sink).expect("repeat deduped"),
            HepaNotificationOutcome::Deduped
        );
        // The repeated states never reached the transport.
        assert_eq!(sink.delivered.len(), 1);
    }

    #[test]
    fn card_status_and_terminal_notification_agree() {
        for card_status in ["done", "completed", "ready_for_human"] {
            assert!(HepaNotificationStatus::Done.agrees_with_card_status(card_status));
        }
        assert!(HepaNotificationStatus::Blocked.agrees_with_card_status("blocked"));
        // A done notification must not agree with an in-progress card.
        assert!(!HepaNotificationStatus::Done.agrees_with_card_status("in_progress"));
        assert!(!HepaNotificationStatus::Blocked.agrees_with_card_status("done"));
    }

    #[test]
    fn rendered_notification_includes_every_required_field() {
        let notification = done_notification("task-1")
            .with_card("hermes-card-1")
            .with_pr_url("https://example.invalid/org/repo/pull/7");

        let line = notification.render();

        for fragment in [
            "[done]",
            "project=project-1",
            "task=task-1",
            "card=hermes-card-1",
            "lane=lane-1",
            "pr=https://example.invalid/org/repo/pull/7",
            "action=Review and merge the PR.",
        ] {
            assert!(line.contains(fragment), "missing {fragment} in: {line}");
        }
    }
}
