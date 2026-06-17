//! Structural performance-budget CI test: a real fake-adapter happy-path run
//! must stay within the one-loop budget. A regression that reintroduces
//! structural overhead is also shown to fail the budget.

use hepa_core::contracts::HepaTimingRecord;
use hepa_core::perf_budget::{
    HepaBudgetViolation, HepaPerfBudget, HepaStructuralMetrics, check_perf_budget,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_repo(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("hepa-perf-{label}-{nonce}"))
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
    std::fs::write(repo.join("README.md"), "# Demo\n").expect("seed file");
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-m", "seed"]);
}

fn run_and_read_timing(repo: &Path) -> HepaTimingRecord {
    let output = Command::new(env!("CARGO_BIN_EXE_hepa-cli"))
        .args([
            "run",
            repo.to_str().expect("utf8"),
            "Doc change",
            "--agent",
            "fake",
        ])
        .current_dir(repo)
        .output()
        .expect("hepa run executes");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let timing_path = repo.join(
        ".hepa/control/runs/run-cli-fake/tasks/task-cli-fake/lanes/lane-cli-fake/timing.json",
    );
    let text = std::fs::read_to_string(&timing_path).expect("read timing");
    serde_json::from_str(&text).expect("parse timing")
}

#[test]
fn fake_happy_path_run_stays_within_structural_budget() {
    let repo = temp_repo("budget");
    init_repo(&repo);
    let timing = run_and_read_timing(&repo);

    // No per-attempt wrapper spawn, unchanged lockfile, no board sync overhead.
    let metrics = HepaStructuralMetrics::from_timing(&timing, 0, 4096, false, 0.0);
    let violations = check_perf_budget(&metrics, &HepaPerfBudget::default());
    assert!(violations.is_empty(), "budget violations: {violations:?}");

    // A regression that starts a container in default mode fails the budget.
    let mut regressed = HepaStructuralMetrics::from_timing(&timing, 0, 4096, false, 0.0);
    regressed.container_count = 1;
    assert!(
        check_perf_budget(&regressed, &HepaPerfBudget::default())
            .contains(&HepaBudgetViolation::ContainerStartedInDefaultMode)
    );

    std::fs::remove_dir_all(&repo).ok();
}
