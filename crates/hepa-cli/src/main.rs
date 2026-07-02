mod fleet;
mod run;

use hepa_adapters::{
    doctor::{HepaAdapterDoctorReport, HepaSystemAdapterDoctorProbe, format_adapter_list},
    interactive::{
        HepaInteractiveSessionRequest, HepaLaneSteeringRequest, HepaSystemTmux, HepaTmux,
        HepaTmuxInteractiveLauncher,
    },
    pi::HepaPiInstallPlan,
    registry::HepaAdapterRegistry,
};
use hepa_core::config::{HepaConfig, HepaConfigOverrides};
use hepa_core::contracts::{HepaHermesManagerIntakeArtifact, HepaLaneState, HepaTimingRecord};
use hepa_core::redaction::redact_secrets;
use hepa_core::timing_trends::{HepaTimingTrendReport, timing_trend_report};
use hepa_git::worktree::HepaWorktreeAllocator;
use hepa_kanban::doctor::system_kanban_doctor_report;
use hepa_kanban::github_webhook::{HepaGithubIssueWebhookRequest, import_github_issue_webhook};
use hepa_kanban::spec_import::{
    import_hermes_manager_intake, import_markdown_spec, imported_spec_to_draft_cards,
};
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
        [command, subcommand, lane_id] if command == "lane" && subcommand == "teardown" => {
            let receipt = HepaTmuxInteractiveLauncher
                .teardown(lane_id, tmux)
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "HEPA lane teardown: lane={} session={} killed=true",
                receipt.lane_id, receipt.session_id
            ))
        }
        [command, rest @ ..]
            if command == "lane"
                && matches!(
                    rest.first().map(String::as_str),
                    Some("list" | "show" | "logs" | "attach" | "stop")
                ) =>
        {
            fleet::lane_command(rest)
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
            let config = load_cli_config()?;
            let report = system_kanban_doctor_report(&config.hermes);
            Ok(report.to_redacted_summary())
        }
        [command, ..] if command == "kanban" => Err("unknown kanban command".to_string()),
        [command, rest @ ..] if command == "hermes" => fleet::hermes_command(rest),
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
            let report = HepaAdapterDoctorReport::from_registry_with_default(
                &registry,
                &HepaSystemAdapterDoctorProbe,
                &config.default_adapter,
            );
            Ok(report.to_redacted_summary())
        }
        [command, subcommand, adapter_id] if command == "adapter" && subcommand == "install" => {
            install_adapter(adapter_id)
        }
        [command, ..] if command == "adapter" => Err("unknown adapter command".to_string()),
        [command, subcommand, path, flags @ ..] if command == "spec" && subcommand == "import" => {
            let options = parse_spec_import_options(flags)?;
            let (imported, source, cards) =
                if let Some(command) = options.hermes_manager_intake_command {
                    let artifact = hermes_manager_intake_from_runtime_command(path, &command)?;
                    let project = artifact.project.clone();
                    let imported = import_hermes_manager_intake(artifact)
                        .map_err(|error| error.to_string())?;
                    let cards = imported_spec_to_draft_cards(project, &imported)
                        .map_err(|error| error.to_string())?;
                    (imported, "hermes-manager-runtime", cards.len())
                } else if let Some(artifact_path) = options.hermes_manager_intake {
                    let artifact = hermes_manager_intake_from_file(&artifact_path)?;
                    let project = artifact.project.clone();
                    let imported = import_hermes_manager_intake(artifact)
                        .map_err(|error| error.to_string())?;
                    let cards = imported_spec_to_draft_cards(project, &imported)
                        .map_err(|error| error.to_string())?;
                    (imported, "hermes-manager", cards.len())
                } else {
                    let text = std::fs::read_to_string(path)
                        .map_err(|error| format!("failed to read spec file: {error}"))?;
                    (
                        import_markdown_spec(&text).map_err(|error| error.to_string())?,
                        "markdown",
                        0,
                    )
                };
            if source.starts_with("hermes-manager") {
                Ok(format!(
                    "HEPA spec import completed: source={source} tasks={} cards={cards}",
                    imported.tasks.len()
                ))
            } else {
                Ok(format!(
                    "HEPA spec import completed: tasks={}",
                    imported.tasks.len()
                ))
            }
        }
        [command, ..] if command == "spec" => Err("unknown spec command".to_string()),
        [command, subcommand, path, flags @ ..]
            if command == "github" && subcommand == "issue-webhook" =>
        {
            let options = parse_github_issue_webhook_options(flags)?;
            let text = std::fs::read_to_string(path)
                .map_err(|error| format!("failed to read GitHub webhook payload: {error}"))?;
            let secret =
                match options.secret_env {
                    Some(secret_env) => Some(std::env::var(&secret_env).map_err(|_| {
                        format!("webhook secret env {secret_env} is not configured")
                    })?),
                    None => None,
                };
            let request = HepaGithubIssueWebhookRequest {
                event: options.event,
                delivery_id: options.delivery_id,
                signature_256: options.signature_256,
                project_id: options.project_id,
                secret,
            };
            let outcome =
                import_github_issue_webhook(&request, &text).map_err(|error| error.to_string())?;
            if let Some(imported) = outcome.imported {
                Ok(format!(
                    "HEPA GitHub issue webhook imported: delivery={} tasks={} verification={:?}",
                    outcome.delivery_id,
                    imported.tasks.len(),
                    outcome.verification
                ))
            } else {
                Ok(format!(
                    "HEPA GitHub issue webhook ignored: delivery={} reason={}",
                    outcome.delivery_id,
                    outcome
                        .ignored_reason
                        .unwrap_or_else(|| "unknown".to_string())
                ))
            }
        }
        [command, ..] if command == "github" => Err("unknown github command".to_string()),
        [command, rest @ ..] if command == "project" => fleet::project_command(rest),
        [command, rest @ ..] if command == "task" => fleet::task_command(rest),
        [command, rest @ ..] if command == "scheduler" => fleet::scheduler_command(rest),
        [command, rest @ ..] if command == "fleet" => fleet::fleet_command(rest),
        [command, subcommand, path] if command == "timing" && subcommand == "summary" => {
            let text = std::fs::read_to_string(path)
                .map_err(|error| format!("failed to read timing file: {error}"))?;
            let timing: HepaTimingRecord = serde_json::from_str(&text)
                .map_err(|error| format!("failed to parse timing file: {error}"))?;
            Ok(format_timing_summary(&timing))
        }
        [command, subcommand, archive_root] if command == "timing" && subcommand == "trends" => {
            let report = timing_trend_report(archive_root).map_err(|error| error.to_string())?;
            Ok(format_timing_trend_report(&report))
        }
        [command, ..] if command == "timing" => Err("unknown timing command".to_string()),
        [command] if command == "doctor" => {
            let config = load_cli_config()?;
            let registry = HepaAdapterRegistry::load_from_config(&config)
                .map_err(|error| format!("failed to load adapter registry: {error}"))?;
            let adapter_report = HepaAdapterDoctorReport::from_registry_with_default(
                &registry,
                &HepaSystemAdapterDoctorProbe,
                &config.default_adapter,
            );
            let kanban_report = system_kanban_doctor_report(&config.hermes);
            Ok(format!(
                "HEPA doctor:\n[adapters]\n{}\n[kanban]\n{}",
                adapter_report.to_redacted_summary(),
                kanban_report.to_redacted_summary()
            ))
        }
        [command, subcommand, path] if command == "bench" && subcommand == "--timing" => {
            let text = std::fs::read_to_string(path)
                .map_err(|error| format!("failed to read timing file: {error}"))?;
            let timing: HepaTimingRecord = serde_json::from_str(&text)
                .map_err(|error| format!("failed to parse timing file: {error}"))?;
            Ok(format!("HEPA bench:\n{}", format_timing_summary(&timing)))
        }
        [command, subcommand] if command == "bench" && subcommand == "reference" => {
            let lines = hepa_core::bench::hoca_reference_medians()
                .into_iter()
                .map(|reference| {
                    format!(
                        "{}: wall={:.2}s agent_loops={} containers={} installs={} peak_rss={:.2}MiB",
                        reference.benchmark_id,
                        reference.median_wall_seconds,
                        reference.median_agent_loops,
                        reference.median_containers,
                        reference.median_install_events,
                        reference.median_peak_rss_mib
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(format!(
                "HEPA bench reference (HOCA v1.1.0 medians):\n{lines}"
            ))
        }
        [command] if command == "bench" => Ok(
            "HEPA bench: provide --timing <file> to summarize a run's timing record".to_string(),
        ),
        [command, ..] if command == "bench" => Err("unknown bench command".to_string()),
        [command, repo_path, task_text, flags @ ..] if command == "run-interactive" => {
            let options = parse_interactive_run_options(flags)?;
            let repo_path = std::path::PathBuf::from(repo_path);
            let control_root = repo_path.join(".hepa/control");
            let worktree_root = repo_path.join(".hepa/worktrees");
            let lane_id = options.lane_id;
            let allocator = HepaWorktreeAllocator::new(&repo_path, &worktree_root);
            let allocation = allocator
                .allocate_lane_with_metadata(&lane_id, cli_timestamp())
                .map_err(|error| error.to_string())?;
            let artifact_dir = control_root
                .join("runs/run-cli-interactive/tasks/task-cli-interactive/lanes")
                .join(&lane_id);
            let command = options.command.unwrap_or_else(default_interactive_command);
            let receipt = HepaTmuxInteractiveLauncher
                .launch(
                    &HepaInteractiveSessionRequest {
                        lane_id: lane_id.clone(),
                        adapter_id: options.agent.clone(),
                        command,
                        workdir: allocation.worktree_path,
                        artifact_dir,
                    },
                    tmux,
                )
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "HEPA interactive run launched: agent={} task={} lane={} session={} record={} log={} worktree_ref=worktree:{}",
                options.agent,
                task_text,
                receipt.record.lane_id,
                receipt.record.session_id,
                receipt.record_path.display(),
                receipt.full_log_path.display(),
                lane_id
            ))
        }
        [command, ..] if command == "run-interactive" => {
            Err("unknown run-interactive command".to_string())
        }
        [command, repo_path, task_text, flags @ ..] if command == "run" => {
            let options = parse_run_options(flags)?;
            let repo_path = std::path::PathBuf::from(repo_path);
            let run_config = run::HepaFakeRunConfig {
                control_root: repo_path.join(".hepa/control"),
                worktree_root: repo_path.join(".hepa/worktrees"),
                archive_root: repo_path.join(".hepa/archive"),
                repo_path,
                run_id: "run-cli-fake".to_string(),
                task_id: "task-cli-fake".to_string(),
                lane_id: "lane-cli-fake".to_string(),
                task_text: task_text.clone(),
                timing: options.timing,
            };
            let result = if options.agent == "fake" {
                run::run_fake_task(&run_config)?
            } else {
                run::run_live_task(&run_config, &options.agent)?
            };
            format_run_result(&options, &result)
        }
        [command, ..] if command == "run" => Err("unknown run command".to_string()),
        _ => Err("unknown command".to_string()),
    }
}

fn format_run_result(
    options: &HepaRunOptions,
    result: &run::HepaFakeRunResult,
) -> Result<String, String> {
    let summary = if options.timing {
        format_timing_summary(&result.timing)
    } else {
        format!(
            "HEPA run completed: agent={} run={} lane={} status={} \
             (safe defaults: worktree sandbox, manager-owned git, auto-merge off)",
            options.agent, result.run_id, result.lane_id, result.status
        )
    };
    if result.status == "completed" {
        Ok(summary)
    } else {
        Err(format!(
            "HEPA run ended with status={}: run={} lane={}\n{}",
            result.status, result.run_id, result.lane_id, summary
        ))
    }
}

#[derive(Debug, Clone, Default)]
struct SpecImportOptions {
    hermes_manager_intake: Option<String>,
    hermes_manager_intake_command: Option<String>,
}

fn parse_spec_import_options(flags: &[String]) -> Result<SpecImportOptions, String> {
    let mut options = SpecImportOptions::default();
    let mut index = 0;
    while index < flags.len() {
        match flags[index].as_str() {
            "--hermes-manager-intake" => {
                let value = flags
                    .get(index + 1)
                    .ok_or_else(|| "--hermes-manager-intake requires a value".to_string())?;
                options.hermes_manager_intake = Some(value.clone());
                index += 2;
            }
            "--hermes-manager-intake-command" => {
                let value = flags.get(index + 1).ok_or_else(|| {
                    "--hermes-manager-intake-command requires a value".to_string()
                })?;
                options.hermes_manager_intake_command = Some(value.clone());
                index += 2;
            }
            flag => return Err(format!("unknown spec import option: {flag}")),
        }
    }
    Ok(options)
}

fn hermes_manager_intake_from_file(
    artifact_path: &str,
) -> Result<HepaHermesManagerIntakeArtifact, String> {
    let raw = std::fs::read_to_string(artifact_path)
        .map_err(|error| format!("failed to read Hermes manager intake artifact: {error}"))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("Hermes manager intake artifact JSON is invalid: {error}"))
}

fn hermes_manager_intake_from_runtime_command(
    spec_path: &str,
    command: &str,
) -> Result<HepaHermesManagerIntakeArtifact, String> {
    let spec_path = std::path::Path::new(spec_path);
    let artifact_dir = spec_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(".hepa-hermes-manager-intake");
    std::fs::create_dir_all(&artifact_dir).map_err(|error| error.to_string())?;
    let context_path = artifact_dir.join("context.json");
    let output_path = artifact_dir.join("manager-intake.runtime.json");
    let stdout_path = artifact_dir.join("runtime.stdout.log");
    let stderr_path = artifact_dir.join("runtime.stderr.log");
    let context = serde_json::json!({
        "schema_version": hepa_core::contracts::CONTRACT_SCHEMA_VERSION,
        "profile_id": "hepa-manager",
        "spec_path": spec_path.display().to_string(),
        "artifact_output": output_path.display().to_string(),
    });
    std::fs::write(
        &context_path,
        serde_json::to_string_pretty(&context).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("HEPA_HERMES_PROFILE_ID", "hepa-manager")
        .env("HEPA_HERMES_CONTEXT_FILE", &context_path)
        .env("HEPA_HERMES_ARTIFACT_OUT", &output_path)
        .output()
        .map_err(|error| format!("Hermes manager intake runtime could not start: {error}"))?;
    std::fs::write(
        &stdout_path,
        redact_secrets(&String::from_utf8_lossy(&output.stdout)),
    )
    .map_err(|error| error.to_string())?;
    std::fs::write(
        &stderr_path,
        redact_secrets(&String::from_utf8_lossy(&output.stderr)),
    )
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Hermes manager intake runtime exited {}",
            output.status.code().unwrap_or(-1)
        ));
    }
    hermes_manager_intake_from_file(
        output_path
            .to_str()
            .ok_or_else(|| "Hermes manager intake runtime output path is not UTF-8".to_string())?,
    )
}

#[derive(Debug, Clone)]
struct GithubIssueWebhookOptions {
    project_id: String,
    event: String,
    delivery_id: String,
    signature_256: Option<String>,
    secret_env: Option<String>,
}

fn parse_github_issue_webhook_options(
    flags: &[String],
) -> Result<GithubIssueWebhookOptions, String> {
    let mut project_id = None;
    let mut event = "issues".to_string();
    let mut delivery_id = None;
    let mut signature_256 = None;
    let mut secret_env = None;
    let mut index = 0;
    while index < flags.len() {
        let flag = flags[index].as_str();
        let value = flags
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?
            .clone();
        match flag {
            "--project" => project_id = Some(value),
            "--event" => event = value,
            "--delivery" => delivery_id = Some(value),
            "--signature-256" => signature_256 = Some(value),
            "--secret-env" => secret_env = Some(value),
            _ => return Err(format!("unknown github issue-webhook option: {flag}")),
        }
        index += 2;
    }
    Ok(GithubIssueWebhookOptions {
        project_id: project_id.ok_or_else(|| "--project is required".to_string())?,
        event,
        delivery_id: delivery_id.ok_or_else(|| "--delivery is required".to_string())?,
        signature_256,
        secret_env,
    })
}

fn cli_timestamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    format!("t{seconds}")
}

fn install_adapter(adapter_id: &str) -> Result<String, String> {
    if adapter_id != "pi" {
        return Err(format!("no built-in installer for adapter {adapter_id}"));
    }
    let plan = HepaPiInstallPlan::npm_global();
    let mut command = std::process::Command::new(&plan.command[0]);
    command.args(&plan.command[1..]);
    let output = command
        .output()
        .map_err(|error| format!("{}; failed to start installer: {error}", plan.action_line()))?;
    if output.status.success() {
        Ok(format!(
            "{}\nHEPA adapter install pi: ok",
            plan.action_line()
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "{}; installer failed with status {:?}: {}",
            plan.action_line(),
            output.status.code(),
            stderr.trim()
        ))
    }
}

struct HepaInteractiveRunOptions {
    agent: String,
    lane_id: String,
    command: Option<String>,
}

fn parse_interactive_run_options(flags: &[String]) -> Result<HepaInteractiveRunOptions, String> {
    let mut agent = "pi".to_string();
    let mut lane_id = "lane-cli-interactive".to_string();
    let mut command = None;
    let mut index = 0;
    while index < flags.len() {
        match flags[index].as_str() {
            "--agent" => {
                let Some(value) = flags.get(index + 1) else {
                    return Err("--agent requires a value".to_string());
                };
                if value.trim().is_empty() {
                    return Err("--agent value must not be empty".to_string());
                }
                agent = value.clone();
                index += 2;
            }
            "--lane-id" => {
                let Some(value) = flags.get(index + 1) else {
                    return Err("--lane-id requires a value".to_string());
                };
                if value.trim().is_empty() {
                    return Err("--lane-id value must not be empty".to_string());
                }
                lane_id = value.clone();
                index += 2;
            }
            "--command" => {
                let Some(value) = flags.get(index + 1) else {
                    return Err("--command requires a value".to_string());
                };
                if value.trim().is_empty() {
                    return Err("--command value must not be empty".to_string());
                }
                command = Some(value.clone());
                index += 2;
            }
            flag => return Err(format!("unknown run-interactive flag: {flag}")),
        }
    }
    Ok(HepaInteractiveRunOptions {
        agent,
        lane_id,
        command,
    })
}

fn default_interactive_command() -> String {
    "sh -lc 'printf \"HEPA interactive lane ready\\n\"; while IFS= read -r line; do printf \"%s\\n\" \"$line\" >> hepa-interactive-steering.log; printf \"steering-applied: %s\\n\" \"$line\"; done'"
        .to_string()
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

struct HepaRunOptions {
    agent: String,
    timing: bool,
}

fn parse_run_options(flags: &[String]) -> Result<HepaRunOptions, String> {
    // Safe defaults: the fake adapter, no timing dump. Worktree sandbox,
    // manager-owned git lifecycle, and no auto-merge are always enforced.
    let mut agent = "fake".to_string();
    let mut timing = false;
    let mut index = 0;
    while index < flags.len() {
        match flags[index].as_str() {
            "--timing" => {
                timing = true;
                index += 1;
            }
            "--fake" => {
                agent = "fake".to_string();
                index += 1;
            }
            "--agent" => {
                let Some(value) = flags.get(index + 1) else {
                    return Err("--agent requires a value".to_string());
                };
                if value.trim().is_empty() {
                    return Err("--agent value must not be empty".to_string());
                }
                agent = value.clone();
                index += 2;
            }
            flag => return Err(format!("unknown run flag: {flag}")),
        }
    }
    Ok(HepaRunOptions { agent, timing })
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
        "HEPA timing: run={} agent_loops={} manager_passes={} worker_profile_llm_calls={} install_events={} container_count={} phases=[{}]",
        timing.run_id,
        timing.counters.agent_loops,
        timing.counters.manager_passes,
        timing.counters.worker_profile_llm_calls,
        timing.counters.install_events,
        timing.counters.container_count,
        phases
    )
}

fn format_timing_trend_report(report: &HepaTimingTrendReport) -> String {
    let phases = report
        .phases
        .iter()
        .map(|phase| {
            format!(
                "{}:samples={} median={:.3}s min={:.3}s max={:.3}s",
                phase.name,
                phase.sample_count,
                phase.median_seconds,
                phase.min_seconds,
                phase.max_seconds
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let runs = report
        .runs
        .iter()
        .map(|run| {
            format!(
                "{} total={:.3}s loops={} managers={} reviewers={} containers={} ref={}",
                run.run_id,
                run.total_duration_seconds,
                run.agent_loops,
                run.manager_passes,
                run.reviewer_passes,
                run.container_count,
                run.timing_ref
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "HEPA timing trends: archive={} timing_records={} runs={} median_total={:.3}s median_agent_loops={:.1} median_manager_passes={:.1} median_reviewer_passes={:.1} median_containers={:.1} phases=[{}] runs=[{}]",
        report.archive_ref,
        report.timing_record_count,
        report.run_count,
        report.total_duration_median_seconds,
        report.agent_loops_median,
        report.manager_passes_median,
        report.reviewer_passes_median,
        report.container_count_median,
        phases,
        runs
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hepa_core::contracts::{
        CONTRACT_SCHEMA_VERSION, HepaAgentRole, HepaPhaseStatus, HepaTerminalStatus,
        HepaTerminalTaskReport, HepaTimingCounters, HepaTimingPhase,
    };
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::Path,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[derive(Default)]
    struct FakeTmux {
        launched: Vec<(String, String, String)>,
        captured: Vec<String>,
        sent: Vec<(String, String)>,
        killed: Vec<String>,
    }

    impl HepaTmux for FakeTmux {
        fn new_session(
            &mut self,
            session_id: &str,
            command: &str,
            workdir: &Path,
        ) -> Result<(), hepa_adapters::interactive::HepaInteractiveSessionError> {
            self.launched.push((
                session_id.to_string(),
                command.to_string(),
                workdir.display().to_string(),
            ));
            Ok(())
        }

        fn capture_pane(
            &mut self,
            session_id: &str,
        ) -> Result<String, hepa_adapters::interactive::HepaInteractiveSessionError> {
            self.captured.push(session_id.to_string());
            Ok("session ready".to_string())
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

        fn kill_session(
            &mut self,
            session_id: &str,
        ) -> Result<(), hepa_adapters::interactive::HepaInteractiveSessionError> {
            self.killed.push(session_id.to_string());
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
    fn run_interactive_launches_lane_worktree_and_records_session() {
        let root = unique_test_dir("run-interactive");
        let repo = root.join("repo");
        init_repo(&repo);
        let mut tmux = FakeTmux::default();

        let output = run_cli_with_tmux(
            &args(&[
                "run-interactive",
                repo.to_str().expect("test path is UTF-8"),
                "Long interactive fixture task",
                "--agent",
                "pi",
                "--lane-id",
                "lane-rs6",
                "--command",
                "sh -lc 'read line; printf %s \"$line\"'",
            ]),
            &mut tmux,
        )
        .expect("interactive launch should run");

        assert!(output.contains("HEPA interactive run launched: agent=pi"));
        assert!(output.contains("lane=lane-rs6 session=hepa-lane-rs6"));
        assert_eq!(tmux.launched.len(), 1);
        assert_eq!(tmux.launched[0].0, "hepa-lane-rs6");
        assert_eq!(tmux.captured, vec!["hepa-lane-rs6".to_string()]);
        let artifact_dir = repo
            .join(".hepa/control/runs/run-cli-interactive/tasks/task-cli-interactive/lanes")
            .join("lane-rs6");
        let record =
            fs::read_to_string(artifact_dir.join("interactive-session.json")).expect("record");
        assert!(record.contains("\"adapter_id\": \"pi\""));
        assert!(record.contains("\"workdir_ref\": \"<LANE_WORKTREE>\""));
        assert_eq!(
            run_cli_with_tmux(&args(&["lane", "teardown", "lane-rs6"]), &mut tmux)
                .expect("teardown should run"),
            "HEPA lane teardown: lane=lane-rs6 session=hepa-lane-rs6 killed=true"
        );
        assert_eq!(tmux.killed, vec!["hepa-lane-rs6".to_string()]);

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

        assert!(output.contains("HEPA kanban doctor:"));
        assert!(output.contains("cli="));
        assert!(
            output.contains("board=skipped") || output.contains("board=missing"),
            "board should be reported as either a configured-environment skip or a missing dependency"
        );
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
        assert!(output.contains("shell-command=skipped"));
    }

    #[test]
    fn spec_import_command_imports_tasks_from_a_markdown_file() {
        let root = unique_test_dir("spec-import");
        std::fs::create_dir_all(&root).expect("spec dir");
        let spec_path = root.join("spec.md");
        std::fs::write(
            &spec_path,
            "Project: project-1\n\n## Task: task-1: Write docs\nExplain the feature.\nAcceptance:\n- Docs describe usage.\nValidation:\n- cargo test\n",
        )
        .expect("write spec");

        let output = run_cli(&args(&[
            "spec",
            "import",
            spec_path.to_str().expect("path is UTF-8"),
        ]))
        .expect("spec import should run");

        assert_eq!(output, "HEPA spec import completed: tasks=1");

        remove_test_dir(root);
    }

    #[test]
    fn spec_import_command_consumes_hermes_manager_intake_artifact() {
        let root = unique_test_dir("spec-import-hermes-manager");
        std::fs::create_dir_all(&root).expect("spec dir");
        let spec_path = root.join("fallback.md");
        let intake_path = root.join("manager-intake.json");
        std::fs::write(&spec_path, "unused fallback spec").expect("write fallback");
        std::fs::write(
            &intake_path,
            r#"
{
  "schema_version": 1,
  "author_profile_id": "hepa-manager",
  "project": {
    "schema_version": 1,
    "project_id": "project-1",
    "display_name": "Project One",
    "repo_ref": "<PROJECT_REPO>",
    "default_branch": "main",
    "routing_policy_ref": null,
    "is_active": true,
    "created_at": "2026-06-16T00:00:00Z",
    "updated_at": "2026-06-16T00:00:00Z"
  },
  "tasks": [
    {
      "task_spec": {
        "schema_version": 1,
        "task_id": "task-1",
        "project_id": "project-1",
        "goal": "Update README.md with the requested workflow.",
        "non_goals": [],
        "expected_areas": ["README.md"],
        "acceptance_criteria": ["Workflow docs are present."],
        "validation_commands": ["cargo test"],
        "dependencies": [],
        "target_branch": "main",
        "risk_level": "low",
        "max_total_rounds": 3,
        "created_at": "2026-06-16T00:00:00Z"
      },
      "title": "Write workflow docs",
      "blocked_questions": [],
      "priority": 7
    }
  ]
}
"#,
        )
        .expect("write intake");

        let output = run_cli(&args(&[
            "spec",
            "import",
            spec_path.to_str().expect("path is UTF-8"),
            "--hermes-manager-intake",
            intake_path.to_str().expect("path is UTF-8"),
        ]))
        .expect("Hermes manager spec import should run");

        assert_eq!(
            output,
            "HEPA spec import completed: source=hermes-manager tasks=1 cards=1"
        );

        remove_test_dir(root);
    }

    #[test]
    fn spec_import_command_runs_hermes_manager_intake_runtime() {
        let root = unique_test_dir("spec-import-hermes-manager-runtime");
        std::fs::create_dir_all(&root).expect("spec dir");
        let spec_path = root.join("project-spec.md");
        let runtime = root.join("fake-hepa-manager");
        std::fs::write(&spec_path, "# Project spec\n").expect("write spec");
        write_executable(
            &runtime,
            r#"#!/usr/bin/env sh
printf '%s\n' "manager intake runtime invoked"
cat > "$HEPA_HERMES_ARTIFACT_OUT" <<'JSON'
{
  "schema_version": 1,
  "author_profile_id": "hepa-manager",
  "project": {
    "schema_version": 1,
    "project_id": "project-1",
    "display_name": "Project One",
    "repo_ref": "<PROJECT_REPO>",
    "default_branch": "main",
    "routing_policy_ref": null,
    "is_active": true,
    "created_at": "2026-06-16T00:00:00Z",
    "updated_at": "2026-06-16T00:00:00Z"
  },
  "tasks": [
    {
      "task_spec": {
        "schema_version": 1,
        "task_id": "task-1",
        "project_id": "project-1",
        "goal": "Update README.md with the requested workflow.",
        "non_goals": [],
        "expected_areas": ["README.md"],
        "acceptance_criteria": ["Workflow docs are present."],
        "validation_commands": ["cargo test"],
        "dependencies": [],
        "target_branch": "main",
        "risk_level": "low",
        "max_total_rounds": 3,
        "created_at": "2026-06-16T00:00:00Z"
      },
      "title": "Write workflow docs",
      "blocked_questions": [],
      "priority": 7
    }
  ]
}
JSON
"#,
        );

        let output = run_cli(&args(&[
            "spec",
            "import",
            spec_path.to_str().expect("path is UTF-8"),
            "--hermes-manager-intake-command",
            runtime.to_str().expect("path is UTF-8"),
        ]))
        .expect("Hermes manager runtime spec import should run");

        assert_eq!(
            output,
            "HEPA spec import completed: source=hermes-manager-runtime tasks=1 cards=1"
        );
        let stdout =
            std::fs::read_to_string(root.join(".hepa-hermes-manager-intake/runtime.stdout.log"))
                .expect("runtime stdout captured");
        assert!(stdout.contains("manager intake runtime invoked"));
        assert!(
            root.join(".hepa-hermes-manager-intake/manager-intake.runtime.json")
                .exists()
        );

        remove_test_dir(root);
    }

    #[test]
    fn spec_import_command_reports_usage_for_missing_path() {
        let error = run_cli(&args(&["spec", "import"])).expect_err("path is required");

        assert_eq!(error, "unknown spec command");
    }

    #[test]
    fn github_issue_webhook_command_imports_issue_payload() {
        let root = unique_test_dir("github-webhook");
        std::fs::create_dir_all(&root).expect("payload dir");
        let payload_path = root.join("payload.json");
        std::fs::write(
            &payload_path,
            r#"{
  "action": "opened",
  "issue": {
    "number": 42,
    "title": "Build webhook intake",
    "body": "Acceptance:\n- Draft task is created.\nValidation:\n- cargo test -p hepa-kanban",
    "labels": []
  },
  "repository": { "default_branch": "main" }
}"#,
        )
        .expect("write payload");

        let output = run_cli(&args(&[
            "github",
            "issue-webhook",
            payload_path.to_str().expect("path is UTF-8"),
            "--project",
            "project-1",
            "--delivery",
            "delivery-1",
        ]))
        .expect("webhook import should run");

        assert_eq!(
            output,
            "HEPA GitHub issue webhook imported: delivery=delivery-1 tasks=1 verification=NotConfigured"
        );

        remove_test_dir(root);
    }

    #[test]
    fn github_issue_webhook_command_requires_project_and_delivery() {
        let error = run_cli(&args(&[
            "github",
            "issue-webhook",
            "payload.json",
            "--project",
            "project-1",
        ]))
        .expect_err("delivery is required");

        assert_eq!(error, "--delivery is required");
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
                worker_profile_llm_calls: 0,
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
        assert!(output.contains("worker_profile_llm_calls=0"));
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
        assert!(output.contains("worker_profile_llm_calls=0"));
        assert!(output.contains("container_count=0"));
        assert!(output.contains("fake_worker=1.000s"));
        assert!(output.contains("fake_review=1.000s"));

        remove_test_dir(root);
    }

    #[test]
    fn blocked_run_result_is_error_even_with_timing_summary() {
        let timing = HepaTimingRecord {
            schema_version: CONTRACT_SCHEMA_VERSION,
            run_id: "run-blocked".to_string(),
            phases: vec![HepaTimingPhase {
                name: "live_staging_commit_pr".to_string(),
                status: HepaPhaseStatus::Blocked,
                duration_seconds: 0.0,
                round: Some(1),
                role: Some(HepaAgentRole::Manager),
                adapter_id: None,
                routing_reason: Some("manager-owned git lifecycle".to_string()),
                sandbox_posture: Some("host-worktree".to_string()),
            }],
            counters: HepaTimingCounters {
                agent_loops: 1,
                manager_passes: 1,
                worker_profile_llm_calls: 0,
                reviewer_passes: 1,
                install_events: 0,
                container_count: 0,
            },
        };
        let result = run::HepaFakeRunResult {
            run_id: "run-blocked".to_string(),
            lane_id: "lane-blocked".to_string(),
            status: "blocked".to_string(),
            timing: timing.clone(),
            terminal_report: HepaTerminalTaskReport {
                schema_version: CONTRACT_SCHEMA_VERSION,
                task_id: "task-blocked".to_string(),
                lane_id: "lane-blocked".to_string(),
                status: HepaTerminalStatus::Blocked,
                pr_url: None,
                validation: None,
                review_signals: Vec::new(),
                arbitration: None,
                timing: Some(timing),
                summary: vec!["blocked before PR creation".to_string()],
                human_attention_required: true,
                completed_at: "2026-06-16T00:00:04Z".to_string(),
            },
            cleanup_performed: false,
        };
        let options = HepaRunOptions {
            agent: "pi".to_string(),
            timing: true,
        };

        let error = format_run_result(&options, &result).expect_err("blocked run must error");

        assert!(error.contains("status=blocked"));
        assert!(error.contains("HEPA timing: run=run-blocked"));
        assert!(error.contains("live_staging_commit_pr=0.000s"));
    }

    #[test]
    fn timing_trends_command_summarizes_archived_runs() {
        let root = unique_test_dir("timing-trends");
        let archive = root.join("archive");
        write_timing_fixture(&archive, "run-1", "task-1", "lane-1", 1.0, 2.0, 1);
        write_timing_fixture(&archive, "run-2", "task-2", "lane-1", 3.0, 4.0, 2);

        let output = run_cli(&args(&[
            "timing",
            "trends",
            archive.to_str().expect("path is UTF-8"),
        ]))
        .expect("timing trends should run");

        assert!(output.contains("HEPA timing trends: archive=archive:"));
        assert!(output.contains("timing_records=2"));
        assert!(output.contains("runs=2"));
        assert!(output.contains("median_total=5.000s"));
        assert!(output.contains("median_agent_loops=1.5"));
        assert!(output.contains("worker:samples=2 median=2.000s"));
        assert!(output.contains("archive:runs/run-1/tasks/task-1/lanes/lane-1/timing.json"));
        assert!(!output.contains(root.to_string_lossy().as_ref()));

        remove_test_dir(root);
    }

    #[test]
    fn run_command_accepts_agent_flag_with_safe_defaults() {
        let root = unique_test_dir("run-agent");
        let repo = root.join("repo");
        init_repo(&repo);

        let output = run_cli(&args(&[
            "run",
            repo.to_str().expect("test path is UTF-8"),
            "Update docs",
            "--agent",
            "fake",
        ]))
        .expect("run with agent should complete");

        assert!(output.contains("HEPA run completed: agent=fake"));
        assert!(output.contains("status=completed"));
        assert!(output.contains("auto-merge off"));

        // A missing agent value is rejected.
        let error = run_cli(&args(&[
            "run",
            repo.to_str().expect("test path is UTF-8"),
            "Update docs",
            "--agent",
        ]))
        .expect_err("missing agent value must error");
        assert!(error.contains("--agent requires a value"));

        remove_test_dir(root);
    }

    #[test]
    fn run_command_routes_non_fake_agents_to_live_adapter() {
        let root = unique_test_dir("run-custom-live");
        let repo = root.join("repo");
        init_repo(&repo);

        let error = run_cli(&args(&[
            "run",
            repo.to_str().expect("test path is UTF-8"),
            "Update docs",
            "--agent",
            "custom",
        ]))
        .expect_err("custom should attempt live adapter execution");

        assert!(error.contains("status=blocked"));
        assert!(
            repo.join(
                ".hepa/control/runs/run-cli-fake/tasks/task-cli-fake/lanes/lane-cli-fake/final-report.json"
            )
            .exists()
        );

        remove_test_dir(root);
    }

    #[test]
    fn doctor_command_aggregates_adapter_and_kanban_health() {
        let output = run_cli(&args(&["doctor"])).expect("doctor should run");
        assert!(output.contains("HEPA doctor:"));
        assert!(output.contains("[adapters]"));
        assert!(output.contains("[kanban]"));
    }

    #[test]
    fn bench_command_reports_usage_and_reads_timing() {
        let usage = run_cli(&args(&["bench"])).expect("bench usage should run");
        assert!(usage.contains("provide --timing"));

        let root = unique_test_dir("bench");
        let repo = root.join("repo");
        init_repo(&repo);
        // Produce a timing artifact via a fake run, then summarize it via bench.
        let result = run::run_fake_task(&run::HepaFakeRunConfig {
            control_root: repo.join(".hepa/control"),
            worktree_root: repo.join(".hepa/worktrees"),
            archive_root: repo.join(".hepa/archive"),
            repo_path: repo.clone(),
            run_id: "run-bench".to_string(),
            task_id: "task-bench".to_string(),
            lane_id: "lane-bench".to_string(),
            task_text: "Update docs".to_string(),
            timing: true,
        })
        .expect("fake run");
        let timing_path = root.join("timing.json");
        std::fs::write(
            &timing_path,
            serde_json::to_string(&result.timing).expect("serialize timing"),
        )
        .expect("write timing");

        let bench = run_cli(&args(&[
            "bench",
            "--timing",
            timing_path.to_str().expect("path is UTF-8"),
        ]))
        .expect("bench should summarize timing");
        assert!(bench.contains("HEPA bench:"));
        assert!(bench.contains("agent_loops=1"));

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

    fn write_executable(path: &Path, contents: &str) {
        fs::write(path, contents).expect("script write");
        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod");
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

    fn write_timing_fixture(
        archive: &Path,
        run_id: &str,
        task_id: &str,
        lane_id: &str,
        worker_seconds: f64,
        review_seconds: f64,
        agent_loops: u32,
    ) {
        let timing = HepaTimingRecord {
            schema_version: CONTRACT_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            phases: vec![
                HepaTimingPhase {
                    name: "worker".to_string(),
                    status: HepaPhaseStatus::Completed,
                    duration_seconds: worker_seconds,
                    round: Some(1),
                    role: Some(HepaAgentRole::Worker),
                    adapter_id: Some("fake".to_string()),
                    routing_reason: Some("test route".to_string()),
                    sandbox_posture: Some("host-worktree".to_string()),
                },
                HepaTimingPhase {
                    name: "review".to_string(),
                    status: HepaPhaseStatus::Completed,
                    duration_seconds: review_seconds,
                    round: Some(1),
                    role: Some(HepaAgentRole::Reviewer),
                    adapter_id: Some("fake".to_string()),
                    routing_reason: Some("test route".to_string()),
                    sandbox_posture: Some("host-worktree".to_string()),
                },
            ],
            counters: HepaTimingCounters {
                agent_loops,
                manager_passes: agent_loops,
                worker_profile_llm_calls: 0,
                reviewer_passes: 1,
                install_events: 0,
                container_count: 0,
            },
        };
        let path = archive
            .join("runs")
            .join(run_id)
            .join("tasks")
            .join(task_id)
            .join("lanes")
            .join(lane_id)
            .join("timing.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
        fs::write(
            path,
            serde_json::to_string_pretty(&timing).expect("timing serializes"),
        )
        .expect("timing writes");
    }
}
