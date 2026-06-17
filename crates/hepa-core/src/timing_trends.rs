use crate::contracts::{HepaTimingRecord, HepaValidate};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
};

pub const TIMING_TREND_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HepaTimingTrendReport {
    pub schema_version: u32,
    pub archive_ref: String,
    pub timing_record_count: usize,
    pub run_count: usize,
    pub total_duration_median_seconds: f64,
    pub agent_loops_median: f64,
    pub manager_passes_median: f64,
    pub reviewer_passes_median: f64,
    pub container_count_median: f64,
    pub runs: Vec<HepaTimingRunTrend>,
    pub phases: Vec<HepaTimingPhaseTrend>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HepaTimingRunTrend {
    pub run_id: String,
    pub timing_ref: String,
    pub total_duration_seconds: f64,
    pub agent_loops: u32,
    pub manager_passes: u32,
    pub reviewer_passes: u32,
    pub container_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HepaTimingPhaseTrend {
    pub name: String,
    pub sample_count: usize,
    pub median_seconds: f64,
    pub min_seconds: f64,
    pub max_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaTimingTrendError {
    pub field: String,
    pub message: String,
}

impl HepaTimingTrendError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaTimingTrendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaTimingTrendError {}

pub fn timing_trend_report(
    archive_root: impl AsRef<Path>,
) -> Result<HepaTimingTrendReport, HepaTimingTrendError> {
    let archive_root = archive_root.as_ref();
    validate_archive_root(archive_root)?;
    let mut timing_paths = Vec::new();
    collect_timing_paths(archive_root, archive_root, &mut timing_paths)?;
    if timing_paths.is_empty() {
        return Err(HepaTimingTrendError::new(
            "archive_root",
            "no archived timing.json records found",
        ));
    }
    timing_paths.sort();

    let mut runs = Vec::new();
    let mut total_durations = Vec::new();
    let mut agent_loops = Vec::new();
    let mut manager_passes = Vec::new();
    let mut reviewer_passes = Vec::new();
    let mut container_counts = Vec::new();
    let mut phase_samples: BTreeMap<String, Vec<f64>> = BTreeMap::new();

    for path in timing_paths {
        let text = fs::read_to_string(&path).map_err(|error| {
            HepaTimingTrendError::new(
                "timing_ref",
                format!("failed to read timing record: {error}"),
            )
        })?;
        let timing: HepaTimingRecord = serde_json::from_str(&text).map_err(|error| {
            HepaTimingTrendError::new(
                "timing_ref",
                format!("failed to parse timing record: {error}"),
            )
        })?;
        timing
            .validate()
            .map_err(|error| HepaTimingTrendError::new(error.field, error.message))?;
        let total_duration_seconds = timing
            .phases
            .iter()
            .map(|phase| phase.duration_seconds)
            .sum::<f64>();
        for phase in &timing.phases {
            phase_samples
                .entry(phase.name.clone())
                .or_default()
                .push(phase.duration_seconds);
        }
        total_durations.push(total_duration_seconds);
        agent_loops.push(timing.counters.agent_loops as f64);
        manager_passes.push(timing.counters.manager_passes as f64);
        reviewer_passes.push(timing.counters.reviewer_passes as f64);
        container_counts.push(timing.counters.container_count as f64);
        runs.push(HepaTimingRunTrend {
            run_id: timing.run_id,
            timing_ref: archive_ref(archive_root, &path)?,
            total_duration_seconds,
            agent_loops: timing.counters.agent_loops,
            manager_passes: timing.counters.manager_passes,
            reviewer_passes: timing.counters.reviewer_passes,
            container_count: timing.counters.container_count,
        });
    }
    runs.sort_by(|left, right| {
        left.run_id
            .cmp(&right.run_id)
            .then(left.timing_ref.cmp(&right.timing_ref))
    });

    let phases = phase_samples
        .into_iter()
        .map(|(name, mut samples)| {
            samples.sort_by(f64::total_cmp);
            HepaTimingPhaseTrend {
                name,
                sample_count: samples.len(),
                median_seconds: median_sorted(&samples),
                min_seconds: samples.first().copied().unwrap_or(0.0),
                max_seconds: samples.last().copied().unwrap_or(0.0),
            }
        })
        .collect::<Vec<_>>();

    Ok(HepaTimingTrendReport {
        schema_version: TIMING_TREND_SCHEMA_VERSION,
        archive_ref: "archive:".to_string(),
        timing_record_count: runs.len(),
        run_count: unique_run_count(&runs),
        total_duration_median_seconds: median(&mut total_durations),
        agent_loops_median: median(&mut agent_loops),
        manager_passes_median: median(&mut manager_passes),
        reviewer_passes_median: median(&mut reviewer_passes),
        container_count_median: median(&mut container_counts),
        runs,
        phases,
    })
}

fn validate_archive_root(path: &Path) -> Result<(), HepaTimingTrendError> {
    let value = path.to_string_lossy();
    if value.trim().is_empty() {
        return Err(HepaTimingTrendError::new(
            "archive_root",
            "must not be empty",
        ));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaTimingTrendError::new(
            "archive_root",
            "must be a single line",
        ));
    }
    if !path.exists() {
        return Err(HepaTimingTrendError::new(
            "archive_root",
            "archive root does not exist",
        ));
    }
    Ok(())
}

fn collect_timing_paths(
    archive_root: &Path,
    current: &Path,
    paths: &mut Vec<PathBuf>,
) -> Result<(), HepaTimingTrendError> {
    let entries = fs::read_dir(current).map_err(|error| {
        HepaTimingTrendError::new("archive_root", format!("failed to scan archive: {error}"))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            HepaTimingTrendError::new("archive_root", format!("failed to scan archive: {error}"))
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_timing_paths(archive_root, &path, paths)?;
        } else if path.file_name().and_then(|name| name.to_str()) == Some("timing.json")
            && path_has_runs_ancestor(archive_root, &path)
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn path_has_runs_ancestor(archive_root: &Path, path: &Path) -> bool {
    path.strip_prefix(archive_root)
        .ok()
        .and_then(|relative| relative.components().next())
        .and_then(|component| component.as_os_str().to_str())
        == Some("runs")
}

fn archive_ref(archive_root: &Path, path: &Path) -> Result<String, HepaTimingTrendError> {
    let relative = path.strip_prefix(archive_root).map_err(|_| {
        HepaTimingTrendError::new("timing_ref", "timing path is outside archive root")
    })?;
    let relative = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Ok(format!("archive:{relative}"))
}

fn unique_run_count(runs: &[HepaTimingRunTrend]) -> usize {
    runs.iter()
        .map(|run| run.run_id.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    median_sorted(values)
}

fn median_sorted(values: &[f64]) -> f64 {
    match values.len() {
        0 => 0.0,
        len if len % 2 == 1 => values[len / 2],
        len => (values[len / 2 - 1] + values[len / 2]) / 2.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{
        CONTRACT_SCHEMA_VERSION, HepaAgentRole, HepaPhaseStatus, HepaTimingCounters,
        HepaTimingPhase,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn timing_trends_aggregate_archived_run_records() {
        let root = unique_test_dir("timing-trends");
        let archive = root.join("archive");
        write_timing(
            &archive,
            "run-1",
            "task-1",
            "lane-a",
            timing("run-1", 4.0, 1.0, 1, 1, 1, 0),
        );
        write_timing(
            &archive,
            "run-2",
            "task-2",
            "lane-a",
            timing("run-2", 8.0, 2.0, 2, 2, 2, 1),
        );

        let report = timing_trend_report(&archive).expect("trend report should aggregate");

        assert_eq!(report.timing_record_count, 2);
        assert_eq!(report.run_count, 2);
        assert_eq!(report.total_duration_median_seconds, 7.5);
        assert_eq!(report.agent_loops_median, 1.5);
        assert_eq!(report.container_count_median, 0.5);
        assert_eq!(
            report.runs[0].timing_ref,
            "archive:runs/run-1/tasks/task-1/lanes/lane-a/timing.json"
        );
        assert_eq!(report.phases[0].name, "review");
        assert_eq!(report.phases[0].median_seconds, 1.5);
        assert_eq!(report.phases[1].name, "worker");
        assert_eq!(report.phases[1].median_seconds, 6.0);
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains(root.to_string_lossy().as_ref())
        );

        remove_test_dir(root);
    }

    #[test]
    fn timing_trends_reject_empty_archives() {
        let root = unique_test_dir("timing-empty");
        fs::create_dir_all(&root).expect("archive root");

        let error = timing_trend_report(&root).expect_err("empty archive should fail");

        assert_eq!(error.field, "archive_root");
        assert!(error.message.contains("no archived timing"));
        remove_test_dir(root);
    }

    fn timing(
        run_id: &str,
        worker_seconds: f64,
        review_seconds: f64,
        agent_loops: u32,
        manager_passes: u32,
        reviewer_passes: u32,
        container_count: u32,
    ) -> HepaTimingRecord {
        HepaTimingRecord {
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
                manager_passes,
                worker_profile_llm_calls: 0,
                reviewer_passes,
                install_events: 0,
                container_count,
            },
        }
    }

    fn write_timing(
        archive: &Path,
        run_id: &str,
        task_id: &str,
        lane_id: &str,
        timing: HepaTimingRecord,
    ) {
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

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-core-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
