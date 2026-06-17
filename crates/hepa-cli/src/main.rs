mod run;

use hepa_adapters::{
    doctor::{HepaAdapterDoctorReport, HepaSystemAdapterDoctorProbe, format_adapter_list},
    interactive::{HepaLaneSteeringRequest, HepaSystemTmux, HepaTmux, HepaTmuxInteractiveLauncher},
    registry::HepaAdapterRegistry,
};
use hepa_core::config::{HepaConfig, HepaConfigOverrides};
use hepa_core::contracts::{HepaLaneState, HepaTimingRecord};
use hepa_kanban::doctor::{HepaKanbanDoctorCheck, HepaKanbanDoctorReport};
use hepa_kanban::spec_import::import_markdown_spec;
use hepa_kanban::sync::{
    HepaKanbanSyncEngine, HepaKanbanSyncStatus, HepaUnavailableHermesCardStore,
};

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match run_cli(&args) {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}

fn run_cli(args: &[String]) -> Result<String, String> {
    let mut tmux = HepaSystemTmux;
    run_cli_with_tmux(args, &mut tmux)
}

fn run_cli_with_tmux(args: &[String], tmux: &mut impl HepaTmux) -> Result<String, String> {
    match args {
        [] => Ok(format!(
            "HEPA workspace initialized ({})",
            hepa_core::crate_name()
        )),
        [command, subcommand, lane_id, message, flags @ ..]
            if command == "lane" && subcommand == "send" =>
        {
            let options = parse_lane_send_options(flags)?;
            let receipt = HepaTmuxInteractiveLauncher
                .send(
                    &HepaLaneSteeringRequest {
                        lane_id: lane_id.clone(),
                        message: message.clone(),
                        manager_approved: options.manager_approved,
                        dry_run: options.dry_run,
                        lane_state: options.lane_state,
                        artifact_dir: options.artifact_dir,
                    },
                    tmux,
                )
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "HEPA lane send {}: lane={} session={} log={}",
                if receipt.sent { "queued" } else { "dry-run" },
                receipt.lane_id,
                receipt.session_id,
                receipt.log_path.display()
            ))
        }
        [command, ..] if command == "lane" => Err("unknown lane command".to_string()),
        [command, subcommand] if command == "kanban" && subcommand == "sync" => {
            let mut store = HepaUnavailableHermesCardStore::new("Hermes CLI/API unavailable");
            let summary = HepaKanbanSyncEngine::new().sync_tasks(&[], &mut store)?;
            match summary.status {
                HepaKanbanSyncStatus::Synced => Ok(format!(
                    "HEPA kanban sync completed: created={} updated={}",
                    summary.created, summary.updated
                )),
                HepaKanbanSyncStatus::Degraded => Ok(format!(
                    "HEPA kanban sync degraded: reason={} skipped={}",
                    summary
                        .degraded_reason
                        .unwrap_or_else(|| "unknown".to_string()),
                    summary.skipped
                )),
            }
        }
        [command, subcommand] if command == "kanban" && subcommand == "doctor" => {
            let report = HepaKanbanDoctorReport::from_checks([
                HepaKanbanDoctorCheck::missing("cli", "Install or configure the Hermes CLI/API."),
                HepaKanbanDoctorCheck::missing("api", "Configure Hermes API access."),
                HepaKanbanDoctorCheck::missing("auth", "Authenticate the Hermes integration."),
                HepaKanbanDoctorCheck::missing("workspace", "Select a Hermes workspace."),
                HepaKanbanDoctorCheck::missing("board", "Select a reachable Hermes board."),
            ]);
            Ok(report.to_redacted_summary())
        }
        [command, ..] if command == "kanban" => Err("unknown kanban command".to_string()),
        [command, subcommand] if command == "adapter" && subcommand == "list" => {
            let config = load_cli_config()?;
            let registry = HepaAdapterRegistry::load_from_config(&config)
                .map_err(|error| format!("failed to load adapter registry: {error}"))?;
            Ok(format_adapter_list(&registry))
        }
        [command, subcommand] if command == "adapter" && subcommand == "doctor" => {
            let config = load_cli_config()?;
            let registry = HepaAdapterRegistry::load_from_config(&config)
                .map_err(|error| format!("failed to load adapter registry: {error}"))?;
            let report =
                HepaAdapterDoctorReport::from_registry(&registry, &HepaSystemAdapterDoctorProbe);
            Ok(report.to_redacted_summary())
        }
        [command, ..] if command == "adapter" => Err("unknown adapter command".to_string()),
        [command, subcommand, path] if command == "spec" && subcommand == "import" => {
            let text = std::fs::read_to_string(path)
                .map_err(|error| format!("failed to read spec file: {error}"))?;
            let imported = import_markdown_spec(&text).map_err(|error| error.to_string())?;
            Ok(format!(
                "HEPA spec import completed: tasks={}",
                imported.tasks.len()
            ))
        }
        [command, ..] if command == "spec" => Err("unknown spec command".to_string()),
        [command, subcommand, path] if command == "timing" && subcommand == "summary" => {
            let text = std::fs::read_to_string(path)
                .map_err(|error| format!("failed to read timing file: {error}"))?;
            let timing: HepaTimingRecord = serde_json::from_str(&text)
                .map_err(|error| format!("failed to parse timing file: {error}"))?;
            Ok(format_timing_summary(&timing))
        }
        [command, ..] if command == "timing" => Err("unknown timing command".to_string()),
        [command, repo_path, task_text, flag] if command == "run" && flag == "--fake" => {
            let repo_path = std::path::PathBuf::from(repo_path);
            let result = run::run_fake_task(&run::HepaFakeRunConfig {
                control_root: repo_path.join(".hepa/control"),
                worktree_root: repo_path.join(".hepa/worktrees"),
                archive_root: repo_path.join(".hepa/archive"),
                repo_path,
                run_id: "run-cli-fake".to_string(),
                task_id: "task-cli-fake".to_string(),
                lane_id: "lane-cli-fake".to_string(),
                task_text: task_text.clone(),
                timing: false,
            })?;
            Ok(format!(
                "HEPA fake run completed: run={} lane={} status={}",
                result.run_id, result.lane_id, result.status
            ))
        }
        [command, repo_path, task_text, flag] if command == "run" && flag == "--timing" => {
            let repo_path = std::path::PathBuf::from(repo_path);
            let result = run::run_fake_task(&run::HepaFakeRunConfig {
                control_root: repo_path.join(".hepa/control"),
                worktree_root: repo_path.join(".hepa/worktrees"),
                archive_root: repo_path.join(".hepa/archive"),
                repo_path,
                run_id: "run-cli-fake".to_string(),
                task_id: "task-cli-fake".to_string(),
                lane_id: "lane-cli-fake".to_string(),
                task_text: task_text.clone(),
                timing: true,
            })?;
            Ok(format_timing_summary(&result.timing))
        }
        [command, ..] if command == "run" => Err("unknown run command".to_string()),
        _ => Err("unknown command".to_string()),
    }
}

struct HepaLaneSendOptions {
    manager_approved: bool,
    dry_run: bool,
    lane_state: HepaLaneState,
    artifact_dir: std::path::PathBuf,
}

fn parse_lane_send_options(flags: &[String]) -> Result<HepaLaneSendOptions, String> {
    let mut manager_approved = false;
    let mut dry_run = false;
    let mut lane_state = None;
    let mut artifact_dir = None;
    let mut index = 0;
    while index < flags.len() {
        match flags[index].as_str() {
            "--manager-approved" => {
                manager_approved = true;
                index += 1;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            "--lane-state" => {
                let Some(value) = flags.get(index + 1) else {
                    return Err("--lane-state requires a value".to_string());
                };
                lane_state = Some(parse_lane_state(value)?);
                index += 2;
            }
            "--artifact-dir" => {
                let Some(value) = flags.get(index + 1) else {
                    return Err("--artifact-dir requires a value".to_string());
                };
                artifact_dir = Some(std::path::PathBuf::from(value));
                index += 2;
            }
            flag => return Err(format!("unknown lane send flag: {flag}")),
        }
    }
    if !manager_approved {
        return Err("manager approval is required: pass --manager-approved".to_string());
    }
    let lane_state =
        lane_state.ok_or_else(|| "lane state is required: pass --lane-state".to_string())?;
    let artifact_dir = artifact_dir
        .ok_or_else(|| "artifact logging is required: pass --artifact-dir".to_string())?;
    Ok(HepaLaneSendOptions {
        manager_approved,
        dry_run,
        lane_state,
        artifact_dir,
    })
}

fn parse_lane_state(value: &str) -> Result<HepaLaneState, String> {
    match value {
        "draft_spec" => Ok(HepaLaneState::DraftSpec),
        "ready" => Ok(HepaLaneState::Ready),
        "allocated" => Ok(HepaLaneState::Allocated),
        "starting" => Ok(HepaLaneState::Starting),
        "running" => Ok(HepaLaneState::Running),
        "validating" => Ok(HepaLaneState::Validating),
        "reviewing" => Ok(HepaLaneState::Reviewing),
        "repairing" => Ok(HepaLaneState::Repairing),
        "staging" => Ok(HepaLaneState::Staging),
        "pr_created" => Ok(HepaLaneState::PrCreated),
        "ready_for_human" => Ok(HepaLaneState::ReadyForHuman),
        "blocked" => Ok(HepaLaneState::Blocked),
        "failed" => Ok(HepaLaneState::Failed),
        "cancelled" => Ok(HepaLaneState::Cancelled),
        "cleaned" => Ok(HepaLaneState::Cleaned),
        "completed" => Ok(HepaLaneState::Completed),
        _ => Err(format!("unknown lane state: {value}")),
    }
}

fn load_cli_config() -> Result<HepaConfig, String> {
    HepaConfig::load_from_env_and_dotenv_file(".env", HepaConfigOverrides::default())
        .map_err(|error| format!("failed to load HEPA config: {error}"))
}

fn format_timing_summary(timing: &HepaTimingRecord) -> String {
    let phases = timing
        .phases
        .iter()
        .map(|phase| {
            format!(
                "{}={:.3}s adapter={} routing={} sandbox={}",
                phase.name,
                phase.duration_seconds,
                phase.adapter_id.as_deref().unwrap_or("none"),
                phase.routing_reason.as_deref().unwrap_or("none"),
                phase.sandbox_posture.as_deref().unwrap_or("none")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "HEPA timing: run={} agent_loops={} manager_passes={} install_events={} container_count={} phases=[{}]",
        timing.run_id,
        timing.counters.agent_loops,
        timing.counters.manager_passes,
        timing.counters.install_events,
        timing.counters.container_count,
        phases
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hepa_core::contracts::{
        CONTRACT_SCHEMA_VERSION, HepaAgentRole, HepaPhaseStatus, HepaTimingCounters,
        HepaTimingPhase,
    };
    use std::{
        fs,
        path::Path,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[derive(Default)]
    struct FakeTmux {
        sent: Vec<(String, String)>,
    }

    impl HepaTmux for FakeTmux {
        fn new_session(
            &mut self,
            _session_id: &str,
            _command: &str,
            _workdir: &Path,
        ) -> Result<(), hepa_adapters::interactive::HepaInteractiveSessionError> {
            Ok(())
        }

        fn capture_pane(
            &mut self,
            _session_id: &str,
        ) -> Result<String, hepa_adapters::interactive::HepaInteractiveSessionError> {
            Ok(String::new())
        }

        fn send_keys(
            &mut self,
            session_id: &str,
            message: &str,
        ) -> Result<(), hepa_adapters::interactive::HepaInteractiveSessionError> {
            self.sent
                .push((session_id.to_string(), message.to_string()));
            Ok(())
        }
    }

    #[test]
    fn kanban_sync_command_runs_empty_sync() {
        let output = run_cli(&args(&["kanban", "sync"])).expect("sync should run");

        assert_eq!(
            output,
            "HEPA kanban sync degraded: reason=Hermes CLI/API unavailable skipped=0"
        );
    }

    #[test]
    fn lane_send_command_is_the_steering_primitive() {
        let root = unique_test_dir("lane-send");
        let artifact_dir = root.join("artifacts");
        let mut tmux = FakeTmux::default();
        let output = run_cli_with_tmux(
            &args(&[
                "lane",
                "send",
                "lane-1",
                "continue with tests",
                "--manager-approved",
                "--lane-state",
                "running",
                "--artifact-dir",
                artifact_dir.to_str().expect("test path is UTF-8"),
            ]),
            &mut tmux,
        )
        .expect("lane send should run");

        assert!(output.contains("HEPA lane send queued: lane=lane-1 session=hepa-lane-1"));
        assert!(output.contains("steering-log.jsonl"));
        assert_eq!(
            tmux.sent,
            vec![("hepa-lane-1".to_string(), "continue with tests".to_string())]
        );
        let log = fs::read_to_string(artifact_dir.join("steering-log.jsonl"))
            .expect("steering log is written");
        assert!(log.contains("\"manager_approved\":true"));
        assert!(log.contains("\"lane_state\":\"running\""));
        assert_eq!(
            run_cli_with_tmux(&args(&["lane", "nudge", "lane-1", "msg"]), &mut tmux)
                .expect_err("other steering commands must not exist"),
            "unknown lane command"
        );

        remove_test_dir(root);
    }

    #[test]
    fn lane_send_requires_approval_and_supports_dry_run() {
        let root = unique_test_dir("lane-send-dry-run");
        let artifact_dir = root.join("artifacts");
        let mut tmux = FakeTmux::default();

        assert_eq!(
            run_cli_with_tmux(
                &args(&[
                    "lane",
                    "send",
                    "lane-1",
                    "continue",
                    "--lane-state",
                    "running",
                    "--artifact-dir",
                    artifact_dir.to_str().expect("test path is UTF-8"),
                ]),
                &mut tmux,
            )
            .expect_err("approval should be required"),
            "manager approval is required: pass --manager-approved"
        );

        let output = run_cli_with_tmux(
            &args(&[
                "lane",
                "send",
                "lane-1",
                "continue",
                "--manager-approved",
                "--dry-run",
                "--lane-state",
                "running",
                "--artifact-dir",
                artifact_dir.to_str().expect("test path is UTF-8"),
            ]),
            &mut tmux,
        )
        .expect("dry-run should log but not send");

        assert!(output.contains("HEPA lane send dry-run: lane=lane-1 session=hepa-lane-1"));
        assert!(tmux.sent.is_empty());
        let log = fs::read_to_string(artifact_dir.join("steering-log.jsonl"))
            .expect("steering log is written");
        assert!(log.contains("\"dry_run\":true"));

        remove_test_dir(root);
    }

    #[test]
    fn kanban_doctor_command_reports_degraded_status() {
        let output = run_cli(&args(&["kanban", "doctor"])).expect("doctor should run");

        assert!(output.contains("HEPA kanban doctor: degraded"));
        assert!(output.contains("cli=missing"));
        assert!(output.contains("board=missing"));
    }

    #[test]
    fn adapter_list_command_prints_default_registry() {
        let output = run_cli(&args(&["adapter", "list"])).expect("adapter list should run");

        assert!(output.contains("HEPA adapter list:"));
        assert!(output.contains("fake"));
        assert!(output.contains("shell-command"));
        assert!(output.contains("sandbox="));
        assert!(output.contains("max_concurrency="));
    }

    #[test]
    fn adapter_doctor_command_reports_default_checks() {
        let output = run_cli(&args(&["adapter", "doctor"])).expect("adapter doctor should run");

        assert!(output.contains("HEPA adapter doctor:"));
        assert!(output.contains("fake=ok"));
        assert!(output.contains("shell-command="));
    }

    #[test]
    fn spec_import_command_reports_usage_for_missing_path() {
        let error = run_cli(&args(&["spec", "import"])).expect_err("path is required");

        assert_eq!(error, "unknown spec command");
    }

    #[test]
    fn timing_summary_command_prints_phase_breakdown() {
        let path = unique_test_file("timing-summary");
        let timing = HepaTimingRecord {
            schema_version: CONTRACT_SCHEMA_VERSION,
            run_id: "run-1".to_string(),
            phases: vec![HepaTimingPhase {
                name: "worker_attempt".to_string(),
                status: HepaPhaseStatus::Completed,
                duration_seconds: 1.25,
                round: Some(1),
                role: Some(HepaAgentRole::Worker),
                adapter_id: Some("fake".to_string()),
                routing_reason: Some("default fake adapter".to_string()),
                sandbox_posture: Some("host-worktree".to_string()),
            }],
            counters: HepaTimingCounters {
                agent_loops: 1,
                manager_passes: 1,
                reviewer_passes: 0,
                install_events: 0,
                container_count: 0,
            },
        };
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&timing).expect("timing serializes"),
        )
        .expect("timing file writes");

        let output = run_cli(&args(&[
            "timing",
            "summary",
            path.to_str().expect("test path is UTF-8"),
        ]))
        .expect("timing summary should run");

        assert!(output.contains("HEPA timing: run=run-1"));
        assert!(output.contains("agent_loops=1"));
        assert!(output.contains("worker_attempt=1.250s"));
        assert!(output.contains("routing=default fake adapter"));
        assert!(output.contains("sandbox=host-worktree"));

        std::fs::remove_file(path).expect("cleanup timing file");
    }

    #[test]
    fn run_timing_command_prints_fake_phase_breakdown() {
        let root = unique_test_dir("run-timing");
        let repo = root.join("repo");
        init_repo(&repo);

        let output = run_cli(&args(&[
            "run",
            repo.to_str().expect("test path is UTF-8"),
            "Update docs",
            "--timing",
        ]))
        .expect("fake timing run should complete");

        assert!(output.contains("HEPA timing: run=run-cli-fake"));
        assert!(output.contains("agent_loops=1"));
        assert!(output.contains("manager_passes=1"));
        assert!(output.contains("container_count=0"));
        assert!(output.contains("fake_worker=1.000s"));
        assert!(output.contains("fake_review=1.000s"));

        remove_test_dir(root);
    }

    fn init_repo(repo: &Path) {
        fs::create_dir_all(repo).expect("repo dir");
        git(repo, ["init"]);
        git(repo, ["config", "user.email", "hepa-test"]);
        git(repo, ["config", "user.name", "HEPA Test"]);
        fs::write(repo.join("README.md"), "fixture\n").expect("fixture write");
        git(repo, ["add", "README.md"]);
        git(repo, ["commit", "-m", "initial"]);
    }

    fn git<const N: usize>(repo: &Path, args: [&str; N]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn unique_test_file(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-cli-{label}-{nonce}.json"))
    }

    fn unique_test_dir(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-cli-{label}-{nonce}"))
    }

    fn remove_test_dir(root: std::path::PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
