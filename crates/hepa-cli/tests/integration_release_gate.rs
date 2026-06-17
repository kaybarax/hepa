//! End-to-end integration fixtures composing the real HEPA gates with the fake
//! adapter and a fake `gh`. No real CLIs, Docker, network, or live Hermes.

use hepa_adapters::fake::{HepaFakeAdapter, HepaFakeReviewerInput, HepaFakeWorkerInput};
use hepa_core::contracts::{
    CONTRACT_SCHEMA_VERSION, HepaReviewStatus, HepaRiskLevel, HepaTaskSpec, HepaValidationStatus,
};
use hepa_core::notifications::{
    HepaInMemoryNotificationSink, HepaNotification, HepaNotificationOutcome,
    HepaNotificationStatus, HepaNotifier,
};
use hepa_core::readiness::{HepaDoneGateInput, evaluate_done_gate};
use hepa_git::pr::{
    HepaCommitMessage, HepaManagerGitLifecycle, HepaPrError, HepaPrRequest, HepaProcessOutput,
    HepaProcessRunner,
};
use hepa_git::staging::HepaSafeStaging;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("hepa-it-rg-{label}-{nonce}"))
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(repo: &Path) {
    std::fs::create_dir_all(repo).expect("repo dir");
    git(repo, &["init", "-b", "main"]);
    git(repo, &["config", "user.email", "hepa-test"]);
    git(repo, &["config", "user.name", "HEPA Test"]);
    // Create the manager lane branch the PR head requires.
    std::fs::write(repo.join("README.md"), "# Demo\n").expect("seed file");
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-m", "seed"]);
    git(repo, &["checkout", "-b", "hepa/manager/lane-1"]);
}

/// A fake `gh`/`git` runner that records calls and returns a canned PR URL.
#[derive(Default)]
struct FakeRunner {
    calls: RefCell<Vec<String>>,
}

impl HepaProcessRunner for FakeRunner {
    fn run(
        &self,
        program: &str,
        _args: &[String],
        _cwd: &Path,
    ) -> Result<HepaProcessOutput, HepaPrError> {
        self.calls.borrow_mut().push(program.to_string());
        Ok(HepaProcessOutput {
            status: 0,
            stdout: "https://example.invalid/org/repo/pull/1".to_string(),
            stderr: String::new(),
        })
    }
}

fn task_spec() -> HepaTaskSpec {
    HepaTaskSpec {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: "task-1".to_string(),
        project_id: "project-1".to_string(),
        goal: "Update docs".to_string(),
        non_goals: Vec::new(),
        expected_areas: vec!["README.md".to_string()],
        acceptance_criteria: vec!["docs updated".to_string()],
        validation_commands: vec!["true".to_string()],
        dependencies: Vec::new(),
        target_branch: Some("main".to_string()),
        risk_level: HepaRiskLevel::Low,
        max_total_rounds: 1,
        created_at: "2026-06-16T00:00:00Z".to_string(),
    }
}

#[test]
fn review_fanout_with_two_reviewers_and_arbitration() {
    use hepa_core::contracts::{HepaFindingSeverity, HepaReviewFinding, HepaReviewSignal};
    use hepa_review::arbitration::{
        apply_deterministic_downgrade_rules, evaluate_staging_after_arbitration,
    };
    use hepa_review::fanout::{
        HepaConfiguredReviewer, HepaReviewFanoutInput, run_configured_reviewers_concurrently,
    };

    fn signal(
        adapter: &str,
        status: HepaReviewStatus,
        findings: Vec<HepaReviewFinding>,
    ) -> HepaReviewSignal {
        HepaReviewSignal {
            schema_version: CONTRACT_SCHEMA_VERSION,
            review_id: format!("review-{adapter}"),
            lane_id: "lane-1".to_string(),
            adapter_id: adapter.to_string(),
            status,
            findings,
            summary: vec!["reviewed".to_string()],
            completed_at: "2026-06-16T00:00:03Z".to_string(),
        }
    }

    let reviewers = vec![
        HepaConfiguredReviewer::new("fake-reviewer-a", |request| {
            Ok(signal(
                &request.adapter_id,
                HepaReviewStatus::Approved,
                Vec::new(),
            ))
        }),
        HepaConfiguredReviewer::new("fake-reviewer-b", |request| {
            let finding = HepaReviewFinding {
                finding_id: "finding-1".to_string(),
                severity: HepaFindingSeverity::Medium,
                category: "style".to_string(),
                evidence: "Out-of-scope nit.".to_string(),
                in_scope: false,
                release_risk: false,
                recommended_action: "Optional cleanup.".to_string(),
                file_ref: Some("README.md".to_string()),
                line: Some(1),
                message: "Minor style nit out of scope.".to_string(),
                accepted: false,
            };
            Ok(signal(
                &request.adapter_id,
                HepaReviewStatus::ChangesRequested,
                vec![finding],
            ))
        }),
    ];

    let result = run_configured_reviewers_concurrently(
        HepaReviewFanoutInput {
            lane_id: "lane-1".to_string(),
            diff_context: "diff --git a/README.md".to_string(),
            validation_summary: "passed".to_string(),
            max_diff_bytes: 4096,
        },
        reviewers,
    )
    .expect("fanout runs concurrently");
    assert_eq!(result.signals.len(), 2);

    // Arbitrate the single finding: out-of-scope, non-release-risk -> downgraded.
    let finding = result
        .signals
        .iter()
        .flat_map(|signal| signal.findings.clone())
        .next()
        .expect("a finding to arbitrate");
    let decision = apply_deterministic_downgrade_rules(finding).expect("arbitration");
    let gate = evaluate_staging_after_arbitration(&[decision]).expect("staging gate");
    // The settled, non-blocking finding allows staging.
    assert!(gate.staging_allowed, "blockers: {:?}", gate.blockers);
}

#[test]
fn repair_loop_forces_a_failure_aware_round_two_rewrite() {
    use hepa_core::contracts::HepaValidationCommandResult;
    use hepa_review::repair::{
        HepaRepairBriefInput, HepaRepairRoundPolicy, HepaRepairRoundState,
        enforce_repair_round_budget, rewrite_repair_prompt_from_evidence,
    };

    // Round 1 failed with a failing validation command.
    let failing = HepaValidationCommandResult {
        command: "cargo test".to_string(),
        exit_code: 101,
        duration_ms: 120,
    };
    let prior_prompt = "Implement the login redirect fix.".to_string();

    // The budget allows a round-2 repair.
    let decision = enforce_repair_round_budget(
        HepaRepairRoundPolicy {
            max_repair_rounds: 2,
            max_total_attempts: 4,
        },
        HepaRepairRoundState {
            next_repair_round: 2,
            total_attempts_after_next: 3,
        },
    )
    .expect("budget");
    assert!(decision.allowed);

    // The round-2 brief is rewritten from failure evidence.
    let brief = rewrite_repair_prompt_from_evidence(HepaRepairBriefInput {
        lane_id: "lane-1".to_string(),
        repair_round: 2,
        prior_prompt: prior_prompt.clone(),
        failing_commands: vec![failing],
        review_findings: Vec::new(),
        diff_state: "diff --git a/src/login.rs".to_string(),
        files_touched: vec!["src/login.rs".to_string()],
    })
    .expect("repair brief");

    assert_eq!(brief.repair_round, 2);
    assert!(brief.prompt.contains("Round: 2"));
    // Failure-aware: the new brief cites the failing command and differs from the
    // prior prompt.
    assert!(brief.prompt.contains("cargo test"));
    assert!(
        brief
            .evidence
            .iter()
            .any(|line| line.contains("cargo test"))
    );
    assert_ne!(brief.prompt, prior_prompt);
}

#[test]
fn fake_adapter_runs_through_every_gate_to_pr_readiness() {
    let repo = temp_dir("e2e");
    init_repo(&repo);

    // 1. Worker attempt (one agent loop).
    let fake = HepaFakeAdapter::default();
    let attempt = fake
        .run_worker_attempt(&HepaFakeWorkerInput {
            task_spec: task_spec(),
            lane_id: "lane-1".to_string(),
            attempt_id: "attempt-1".to_string(),
            round: 1,
            started_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: "2026-06-16T00:00:01Z".to_string(),
        })
        .expect("worker attempt");
    assert!(!attempt.changed_files.is_empty());

    // The worker's edit (simulated as a real file change in the worktree).
    std::fs::write(repo.join("README.md"), "# Demo\n\nUpdated docs.\n").expect("worker edit");

    // 2. Validation passes (placeholder command).
    let validation_passed = HepaValidationStatus::Passed;

    // 3. Review fanout (one fake reviewer) approves.
    let review = fake
        .run_reviewer(&HepaFakeReviewerInput {
            lane_id: "lane-1".to_string(),
            review_id: "review-1".to_string(),
            completed_at: "2026-06-16T00:00:02Z".to_string(),
        })
        .expect("review");
    assert_eq!(review.status, HepaReviewStatus::Approved);

    // 4. Safe staging of the approved file only.
    let staging = HepaSafeStaging::new(&repo);
    let staged = staging
        .stage_approved_files(&["README.md".to_string()])
        .expect("safe staging");
    assert_eq!(staged.staged_files, vec!["README.md".to_string()]);

    // 5. Manager commit + PR (fake gh).
    let lifecycle = HepaManagerGitLifecycle::manager(&repo);
    lifecycle
        .commit_staged(&HepaCommitMessage::new("docs: update README"))
        .expect("manager commit");
    let runner = FakeRunner::default();
    let pr = lifecycle
        .create_pr(
            &HepaPrRequest {
                title: "docs: update README".to_string(),
                body: "## Summary\nUpdated docs.".to_string(),
                base_branch: "main".to_string(),
                head_branch: "hepa/manager/lane-1".to_string(),
            },
            &runner,
        )
        .expect("manager PR");
    assert!(pr.url.starts_with("http"));

    // 6. Done gate: every required condition holds -> ready.
    let done = evaluate_done_gate(&HepaDoneGateInput {
        pr_exists: true,
        validation_status: validation_passed,
        review_passed: matches!(review.status, HepaReviewStatus::Approved),
        ..HepaDoneGateInput::default()
    });
    assert!(
        done.is_ready(),
        "lane should reach PR-readiness: {:?}",
        done.blockers
    );

    // 7. Exactly one terminal notification.
    let mut notifier = HepaNotifier::new();
    let mut sink = HepaInMemoryNotificationSink::default();
    let notification = HepaNotification::new(
        "project-1",
        "task-1",
        "lane-1",
        HepaNotificationStatus::Done,
        "Review and merge the PR.",
    )
    .with_pr_url(pr.url);
    assert_eq!(
        notifier.notify(&notification, &mut sink).expect("notify"),
        HepaNotificationOutcome::Emitted
    );
    assert_eq!(
        notifier.notify(&notification, &mut sink).expect("dedupe"),
        HepaNotificationOutcome::Deduped
    );
    assert_eq!(sink.delivered.len(), 1);

    std::fs::remove_dir_all(&repo).ok();
}
