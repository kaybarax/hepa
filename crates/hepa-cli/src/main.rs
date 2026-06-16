use hepa_kanban::doctor::{HepaKanbanDoctorCheck, HepaKanbanDoctorReport};
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
    match args {
        [] => Ok(format!(
            "HEPA workspace initialized ({})",
            hepa_core::crate_name()
        )),
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
        _ => Err("unknown command".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
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
    fn kanban_doctor_command_reports_degraded_status() {
        let output = run_cli(&args(&["kanban", "doctor"])).expect("doctor should run");

        assert!(output.contains("HEPA kanban doctor: degraded"));
        assert!(output.contains("cli=missing"));
        assert!(output.contains("board=missing"));
    }
}
