use hepa_kanban::sync::{HepaKanbanSyncEngine, HepaNullHermesCardStore};

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
            let mut store = HepaNullHermesCardStore;
            let summary = HepaKanbanSyncEngine::new().sync_tasks(&[], &mut store)?;
            Ok(format!(
                "HEPA kanban sync completed: created={} updated={}",
                summary.created, summary.updated
            ))
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

        assert_eq!(output, "HEPA kanban sync completed: created=0 updated=0");
    }
}
