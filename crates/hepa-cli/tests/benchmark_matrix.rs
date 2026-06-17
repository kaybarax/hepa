//! Side-by-side benchmark matrix: run the canonical benchmark task on HEPA with
//! the fake adapter (no real CLIs/Docker/network), then compare to the Phase 0.4
//! HOCA reference and validate the architecture targets.

use hepa_core::bench::{aggregate_timing_records, compare_to_reference};
use hepa_core::contracts::HepaTimingRecord;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_repo(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("hepa-bench-{label}-{nonce}"))
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

/// Run `hepa run <repo> "<task>"` via the built binary and read the timing
/// artifact it wrote.
fn run_and_read_timing(repo: &Path, task: &str) -> HepaTimingRecord {
    let status = Command::new(env!("CARGO_BIN_EXE_hepa-cli"))
        .args(["run", repo.to_str().expect("utf8"), task])
        .arg("--agent")
        .arg("fake")
        .current_dir(repo)
        .output()
        .expect("hepa run executes");
    assert!(
        status.status.success(),
        "hepa run failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let timing_path = repo.join(
        ".hepa/control/runs/run-cli-fake/tasks/task-cli-fake/lanes/lane-cli-fake/timing.json",
    );
    let text = std::fs::read_to_string(&timing_path)
        .unwrap_or_else(|error| panic!("read timing {}: {error}", timing_path.display()));
    serde_json::from_str(&text).expect("parse timing record")
}

#[test]
fn hepa_fake_adapter_matrix_meets_targets_versus_hoca_reference() {
    // The fake adapter is the only available implementation/review adapter in CI;
    // configured cloud adapters are a documented skip (no auth/network here).
    let repo_a = temp_repo("doc-1");
    let repo_b = temp_repo("doc-2");
    init_repo(&repo_a);
    init_repo(&repo_b);

    let task = "Trivial doc-only change: add a usage note to the README.";
    let runs = vec![
        run_and_read_timing(&repo_a, task),
        run_and_read_timing(&repo_b, task),
    ];

    // Each run is one agent loop with no containers (the one-loop model).
    for run in &runs {
        assert_eq!(run.counters.agent_loops, 1);
        assert_eq!(run.counters.container_count, 0);
        assert!(run.counters.manager_passes <= 2);
    }

    // Manual timing: the fake adapter does not measure real wall time / RSS.
    let candidate =
        aggregate_timing_records("PB-DOC-001", &runs, 0.0, 0.0, true).expect("aggregate");
    let comparison = compare_to_reference(&candidate).expect("compare to HOCA reference");

    assert!(comparison.targets.one_loop_per_attempt);
    assert!(comparison.targets.zero_default_containers);
    assert!(comparison.targets.manager_passes_within_budget);
    assert!(comparison.targets.faster_than_reference);
    assert!(
        comparison.targets.all_met(),
        "targets: {:?}",
        comparison.targets
    );

    std::fs::remove_dir_all(&repo_a).ok();
    std::fs::remove_dir_all(&repo_b).ok();
}
