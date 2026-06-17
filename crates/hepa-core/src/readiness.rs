use crate::contracts::{CONTRACT_SCHEMA_VERSION, HepaValidationStatus};
use serde::{Deserialize, Serialize};

/// Terminal readiness of a lane against the definition of done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaDoneStatus {
    Ready,
    NotReady,
}

/// Why a lane is not ready. Serialized names match the architecture's
/// not-ready classification vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaNotReadyReason {
    NeedsRebase,
    MergeConflict,
    CiFailed,
    ReviewFailed,
    MissingArtifact,
    HumanClarificationNeeded,
    BlockedByDependency,
    KanbanDrift,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaDoneBlocker {
    pub reason: HepaNotReadyReason,
    pub message: String,
}

/// Facts the done gate evaluates. These are already-resolved booleans/statuses
/// so the gate stays deterministic and testable without `gh`, CI, or network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaDoneGateInput {
    pub pr_exists: bool,
    pub branch_synced_with_base: bool,
    pub merge_conflict: bool,
    pub validation_status: HepaValidationStatus,
    pub review_passed: bool,
    pub residual_findings_accepted: bool,
    pub ci_present: bool,
    pub ci_passing: bool,
    pub ui_files_changed: bool,
    pub screenshots_attached: bool,
    pub risk_policy_approved: bool,
    pub card_lane_consistent: bool,
    pub unmet_dependencies: Vec<String>,
    pub human_clarification_needed: bool,
    pub missing_artifacts: Vec<String>,
}

impl Default for HepaDoneGateInput {
    /// A fully ready lane: every positive condition holds and nothing is
    /// outstanding. Tests flip individual fields to drive specific blockers.
    fn default() -> Self {
        Self {
            pr_exists: true,
            branch_synced_with_base: true,
            merge_conflict: false,
            validation_status: HepaValidationStatus::Passed,
            review_passed: true,
            residual_findings_accepted: false,
            ci_present: false,
            ci_passing: false,
            ui_files_changed: false,
            screenshots_attached: false,
            risk_policy_approved: true,
            card_lane_consistent: true,
            unmet_dependencies: Vec::new(),
            human_clarification_needed: false,
            missing_artifacts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaDoneGateResult {
    pub schema_version: u32,
    pub status: HepaDoneStatus,
    pub blockers: Vec<HepaDoneBlocker>,
}

impl HepaDoneGateResult {
    pub fn is_ready(&self) -> bool {
        matches!(self.status, HepaDoneStatus::Ready)
    }

    /// A Hermes card may only show done when the HEPA done gate is satisfied.
    /// Board requests to mark a card done are rejected while readiness fails.
    pub fn card_may_show_done(&self) -> bool {
        self.is_ready()
    }
}

impl HepaNotReadyReason {
    /// Stable wire name used in artifacts, card comments, and notifications.
    pub fn as_str(self) -> &'static str {
        match self {
            HepaNotReadyReason::NeedsRebase => "needs_rebase",
            HepaNotReadyReason::MergeConflict => "merge_conflict",
            HepaNotReadyReason::CiFailed => "ci_failed",
            HepaNotReadyReason::ReviewFailed => "review_failed",
            HepaNotReadyReason::MissingArtifact => "missing_artifact",
            HepaNotReadyReason::HumanClarificationNeeded => "human_clarification_needed",
            HepaNotReadyReason::BlockedByDependency => "blocked_by_dependency",
            HepaNotReadyReason::KanbanDrift => "kanban_drift",
        }
    }
}

/// Evaluate the definition of done. A lane is ready only when every required
/// condition holds; otherwise each failing condition yields a classified,
/// actionable blocker. PR existence alone is never sufficient.
pub fn evaluate_done_gate(input: &HepaDoneGateInput) -> HepaDoneGateResult {
    let mut blockers = Vec::new();

    if !input.pr_exists {
        blockers.push(blocker(
            HepaNotReadyReason::MissingArtifact,
            "No PR has been created; create the manager PR before marking done.",
        ));
    }
    if input.merge_conflict {
        blockers.push(blocker(
            HepaNotReadyReason::MergeConflict,
            "PR has merge conflicts; resolve conflicts against the base branch.",
        ));
    } else if !input.branch_synced_with_base {
        blockers.push(blocker(
            HepaNotReadyReason::NeedsRebase,
            "Branch is behind base; rebase the manager branch onto the base branch.",
        ));
    }

    if !matches!(
        input.validation_status,
        HepaValidationStatus::Passed | HepaValidationStatus::NoTestsDetected
    ) {
        blockers.push(blocker(
            HepaNotReadyReason::MissingArtifact,
            "Passing validation is required before done; re-run validation until it passes.",
        ));
    }

    if !(input.review_passed || input.residual_findings_accepted) {
        blockers.push(blocker(
            HepaNotReadyReason::ReviewFailed,
            "Review did not pass and no residual acceptance was recorded; address findings or record manager acceptance.",
        ));
    }

    if input.ci_present && !input.ci_passing {
        blockers.push(blocker(
            HepaNotReadyReason::CiFailed,
            "CI checks are failing; fix CI before marking done.",
        ));
    }

    if input.ui_files_changed && !input.screenshots_attached {
        blockers.push(blocker(
            HepaNotReadyReason::MissingArtifact,
            "UI files changed but no screenshot artifact is attached; attach UI screenshots.",
        ));
    }

    if !input.risk_policy_approved {
        blockers.push(blocker(
            HepaNotReadyReason::HumanClarificationNeeded,
            "Risk policy does not permit publication without approval; obtain risk sign-off.",
        ));
    }

    if !input.card_lane_consistent {
        blockers.push(blocker(
            HepaNotReadyReason::KanbanDrift,
            "Hermes card and HEPA lane disagree; reconcile board state before done.",
        ));
    }

    if !input.unmet_dependencies.is_empty() {
        let mut deps = input.unmet_dependencies.clone();
        deps.sort();
        blockers.push(blocker(
            HepaNotReadyReason::BlockedByDependency,
            format!("Blocked by unmet dependencies: {}.", deps.join(", ")),
        ));
    }

    if input.human_clarification_needed {
        blockers.push(blocker(
            HepaNotReadyReason::HumanClarificationNeeded,
            "Task needs human clarification before it can complete.",
        ));
    }

    let mut missing = input.missing_artifacts.clone();
    missing.sort();
    for artifact in missing {
        blockers.push(blocker(
            HepaNotReadyReason::MissingArtifact,
            format!("Required artifact missing: {artifact}."),
        ));
    }

    let status = if blockers.is_empty() {
        HepaDoneStatus::Ready
    } else {
        HepaDoneStatus::NotReady
    };

    HepaDoneGateResult {
        schema_version: CONTRACT_SCHEMA_VERSION,
        status,
        blockers,
    }
}

fn blocker(reason: HepaNotReadyReason, message: impl Into<String>) -> HepaDoneBlocker {
    HepaDoneBlocker {
        reason,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fully_satisfied_lane_is_ready() {
        let result = evaluate_done_gate(&HepaDoneGateInput::default());

        assert!(result.is_ready());
        assert!(result.blockers.is_empty());
    }

    #[test]
    fn pr_existence_alone_is_never_ready() {
        // PR exists but every other required condition is unmet.
        let input = HepaDoneGateInput {
            pr_exists: true,
            branch_synced_with_base: false,
            validation_status: HepaValidationStatus::Failed,
            review_passed: false,
            residual_findings_accepted: false,
            risk_policy_approved: false,
            ..HepaDoneGateInput::default()
        };

        let result = evaluate_done_gate(&input);

        assert_eq!(result.status, HepaDoneStatus::NotReady);
        assert!(!result.blockers.is_empty());
    }

    #[test]
    fn residual_acceptance_satisfies_the_review_requirement() {
        let input = HepaDoneGateInput {
            review_passed: false,
            residual_findings_accepted: true,
            ..HepaDoneGateInput::default()
        };

        let result = evaluate_done_gate(&input);

        assert!(result.is_ready());
    }

    #[test]
    fn each_not_ready_classification_is_reachable_and_actionable() {
        let cases: Vec<(HepaDoneGateInput, HepaNotReadyReason)> = vec![
            (
                HepaDoneGateInput {
                    branch_synced_with_base: false,
                    ..HepaDoneGateInput::default()
                },
                HepaNotReadyReason::NeedsRebase,
            ),
            (
                HepaDoneGateInput {
                    merge_conflict: true,
                    ..HepaDoneGateInput::default()
                },
                HepaNotReadyReason::MergeConflict,
            ),
            (
                HepaDoneGateInput {
                    ci_present: true,
                    ci_passing: false,
                    ..HepaDoneGateInput::default()
                },
                HepaNotReadyReason::CiFailed,
            ),
            (
                HepaDoneGateInput {
                    review_passed: false,
                    residual_findings_accepted: false,
                    ..HepaDoneGateInput::default()
                },
                HepaNotReadyReason::ReviewFailed,
            ),
            (
                HepaDoneGateInput {
                    pr_exists: false,
                    ..HepaDoneGateInput::default()
                },
                HepaNotReadyReason::MissingArtifact,
            ),
            (
                HepaDoneGateInput {
                    human_clarification_needed: true,
                    ..HepaDoneGateInput::default()
                },
                HepaNotReadyReason::HumanClarificationNeeded,
            ),
            (
                HepaDoneGateInput {
                    unmet_dependencies: vec!["task-0".to_string()],
                    ..HepaDoneGateInput::default()
                },
                HepaNotReadyReason::BlockedByDependency,
            ),
            (
                HepaDoneGateInput {
                    card_lane_consistent: false,
                    ..HepaDoneGateInput::default()
                },
                HepaNotReadyReason::KanbanDrift,
            ),
        ];

        for (input, expected) in cases {
            let result = evaluate_done_gate(&input);
            assert_eq!(result.status, HepaDoneStatus::NotReady, "{expected:?}");
            let blocker = result
                .blockers
                .iter()
                .find(|blocker| blocker.reason == expected)
                .unwrap_or_else(|| panic!("missing classification {expected:?}"));
            // Actionable: a non-trivial directive, not just a label.
            assert!(
                blocker.message.len() > 20,
                "non-actionable reason for {expected:?}: {}",
                blocker.message
            );
        }
    }

    #[test]
    fn classification_wire_names_match_the_architecture_vocabulary() {
        let expected = [
            (HepaNotReadyReason::NeedsRebase, "needs_rebase"),
            (HepaNotReadyReason::MergeConflict, "merge_conflict"),
            (HepaNotReadyReason::CiFailed, "ci_failed"),
            (HepaNotReadyReason::ReviewFailed, "review_failed"),
            (HepaNotReadyReason::MissingArtifact, "missing_artifact"),
            (
                HepaNotReadyReason::HumanClarificationNeeded,
                "human_clarification_needed",
            ),
            (
                HepaNotReadyReason::BlockedByDependency,
                "blocked_by_dependency",
            ),
            (HepaNotReadyReason::KanbanDrift, "kanban_drift"),
        ];

        for (reason, name) in expected {
            assert_eq!(reason.as_str(), name);
            let json = serde_json::to_string(&reason).expect("reason serializes");
            assert_eq!(json, format!("\"{name}\""));
        }
    }

    #[test]
    fn card_may_not_show_done_while_readiness_fails() {
        let blocked = evaluate_done_gate(&HepaDoneGateInput {
            card_lane_consistent: false,
            ..HepaDoneGateInput::default()
        });
        assert!(!blocked.card_may_show_done());

        let ready = evaluate_done_gate(&HepaDoneGateInput::default());
        assert!(ready.card_may_show_done());
    }
}
