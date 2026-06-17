use crate::config::HepaConfig;
use crate::contracts::{HepaTimingRecord, HepaValidate};
use crate::cost_accounting::HepaLaneCostReport;
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

pub const ARTIFACT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaArtifactLayout {
    control_root: PathBuf,
    archive_root: PathBuf,
}

impl HepaArtifactLayout {
    pub fn new(
        control_root: impl Into<PathBuf>,
        archive_root: impl Into<PathBuf>,
    ) -> Result<Self, HepaArtifactPathError> {
        let layout = Self {
            control_root: control_root.into(),
            archive_root: archive_root.into(),
        };
        layout.validate_root("control_root", &layout.control_root)?;
        layout.validate_root("archive_root", &layout.archive_root)?;
        Ok(layout)
    }

    pub fn from_config(config: &HepaConfig) -> Result<Self, HepaArtifactPathError> {
        Self::new(&config.control_root, &config.archive_root)
    }

    pub fn run(
        &self,
        run_id: impl Into<String>,
        task_id: impl Into<String>,
    ) -> Result<HepaRunArtifactPaths, HepaArtifactPathError> {
        let run_id = HepaArtifactId::new("run_id", run_id)?;
        let task_id = HepaArtifactId::new("task_id", task_id)?;
        let run_fragment = run_id.as_str();
        let run_dir = self.control_root.join("runs").join(run_fragment);
        let task_dir = run_dir.join("tasks").join(task_id.as_str());
        let archive_dir = self.archive_root.join("runs").join(run_fragment);

        Ok(HepaRunArtifactPaths {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            run_id,
            task_id,
            run_dir: run_dir.clone(),
            task_dir: task_dir.clone(),
            run_state: run_dir.join("run.json"),
            task_state: task_dir.join("task.json"),
            archive_dir: archive_dir.clone(),
            archive_manifest: archive_dir.join("manifest.json"),
        })
    }

    fn validate_root(&self, field: &str, root: &Path) -> Result<(), HepaArtifactPathError> {
        let root = root.to_string_lossy();
        if root.trim().is_empty() {
            return Err(HepaArtifactPathError::new(field, "must not be empty"));
        }
        if root.contains('\n') || root.contains('\r') {
            return Err(HepaArtifactPathError::new(field, "must be a single line"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaRunArtifactPaths {
    pub schema_version: u32,
    pub run_id: HepaArtifactId,
    pub task_id: HepaArtifactId,
    pub run_dir: PathBuf,
    pub task_dir: PathBuf,
    pub run_state: PathBuf,
    pub task_state: PathBuf,
    pub archive_dir: PathBuf,
    pub archive_manifest: PathBuf,
}

impl HepaRunArtifactPaths {
    pub fn lane(
        &self,
        lane_id: impl Into<String>,
    ) -> Result<HepaLaneArtifactPaths, HepaArtifactPathError> {
        let lane_id = HepaArtifactId::new("lane_id", lane_id)?;
        let lane_dir = self.task_dir.join("lanes").join(lane_id.as_str());
        Ok(HepaLaneArtifactPaths {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            task_id: self.task_id.clone(),
            lane_id,
            lane_dir: lane_dir.clone(),
            lane_state: lane_dir.join("lane.json"),
            attempts_dir: lane_dir.join("attempts"),
            validation_summary: lane_dir.join("validation").join("summary.json"),
            review_dir: lane_dir.join("review"),
            state_dir: lane_dir.join("state"),
            transition_dir: lane_dir.join("state").join("transitions"),
            current_state: lane_dir.join("state").join("current.json"),
            timing_record: lane_dir.join("timing.json"),
            cost_report: lane_dir.join("cost.json"),
            final_report: lane_dir.join("final-report.json"),
        })
    }

    pub fn archive_on_exit(
        &self,
        archived_at: impl Into<String>,
        outcome: HepaArchiveOutcome,
    ) -> Result<HepaArchiveManifest, HepaArtifactWriteError> {
        let archived_at = archived_at.into();
        require_single_line_record("archived_at", Some(&archived_at))?;
        fs::create_dir_all(&self.archive_dir)?;
        copy_dir_contents(&self.run_dir, &self.archive_dir)?;
        let manifest = HepaArchiveManifest {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            run_id: self.run_id.as_str().to_string(),
            task_id: self.task_id.as_str().to_string(),
            outcome,
            control_ref: format!("control:runs/{}", self.run_id.as_str()),
            archive_ref: format!("archive:runs/{}", self.run_id.as_str()),
            archived_at,
        };
        write_stable_json(&self.archive_manifest, &manifest)?;
        Ok(manifest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaLaneArtifactPaths {
    pub schema_version: u32,
    pub run_id: HepaArtifactId,
    pub task_id: HepaArtifactId,
    pub lane_id: HepaArtifactId,
    pub lane_dir: PathBuf,
    pub lane_state: PathBuf,
    pub attempts_dir: PathBuf,
    pub validation_summary: PathBuf,
    pub review_dir: PathBuf,
    pub state_dir: PathBuf,
    pub transition_dir: PathBuf,
    pub current_state: PathBuf,
    pub timing_record: PathBuf,
    pub cost_report: PathBuf,
    pub final_report: PathBuf,
}

impl HepaLaneArtifactPaths {
    pub fn attempt(
        &self,
        attempt_id: impl Into<String>,
    ) -> Result<HepaAttemptArtifactPaths, HepaArtifactPathError> {
        let attempt_id = HepaArtifactId::new("attempt_id", attempt_id)?;
        let attempt_dir = self.attempts_dir.join(attempt_id.as_str());
        Ok(HepaAttemptArtifactPaths {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            task_id: self.task_id.clone(),
            lane_id: self.lane_id.clone(),
            attempt_id,
            attempt_dir: attempt_dir.clone(),
            attempt_report: attempt_dir.join("attempt.json"),
            stdout_log: attempt_dir.join("stdout.log"),
            stderr_log: attempt_dir.join("stderr.log"),
        })
    }

    pub fn review_signal(
        &self,
        review_id: impl Into<String>,
    ) -> Result<PathBuf, HepaArtifactPathError> {
        let review_id = HepaArtifactId::new("review_id", review_id)?;
        Ok(self
            .review_dir
            .join("signals")
            .join(format!("{}.json", review_id.as_str())))
    }

    pub fn transition_record(
        &self,
        transition_id: impl Into<String>,
    ) -> Result<PathBuf, HepaArtifactPathError> {
        let transition_id = HepaArtifactId::new("transition_id", transition_id)?;
        Ok(self
            .transition_dir
            .join(format!("{}.json", transition_id.as_str())))
    }

    pub fn write_transition_state(
        &self,
        record: &HepaStateTransitionRecord,
    ) -> Result<HepaStateWriteReceipt, HepaArtifactWriteError> {
        self.require_record_matches(record)?;
        let transition_path = self.transition_record(record.transition_id.as_str())?;
        write_stable_json(&transition_path, record)?;
        write_stable_json(&self.current_state, record)?;
        Ok(HepaStateWriteReceipt {
            transition_path,
            current_state_path: self.current_state.clone(),
        })
    }

    pub fn write_timing_record(
        &self,
        timing: &HepaTimingRecord,
    ) -> Result<PathBuf, HepaArtifactWriteError> {
        timing
            .validate()
            .map_err(|error| HepaArtifactWriteError::InvalidRecord {
                field: error.field,
                message: error.message,
            })?;
        require_matching_id("run_id", &self.run_id, &timing.run_id)?;
        write_stable_json(&self.timing_record, timing)?;
        Ok(self.timing_record.clone())
    }

    pub fn write_cost_report(
        &self,
        report: &HepaLaneCostReport,
    ) -> Result<PathBuf, HepaArtifactWriteError> {
        report
            .validate()
            .map_err(|error| HepaArtifactWriteError::InvalidRecord {
                field: error.field,
                message: error.message,
            })?;
        require_matching_id("run_id", &self.run_id, &report.run_id)?;
        require_matching_id("task_id", &self.task_id, &report.task_id)?;
        require_matching_id("lane_id", &self.lane_id, &report.lane_id)?;
        write_stable_json(&self.cost_report, report)?;
        Ok(self.cost_report.clone())
    }

    fn require_record_matches(
        &self,
        record: &HepaStateTransitionRecord,
    ) -> Result<(), HepaArtifactWriteError> {
        if record.schema_version != ARTIFACT_SCHEMA_VERSION {
            return Err(HepaArtifactWriteError::InvalidRecord {
                field: "schema_version".to_string(),
                message: format!("must be {ARTIFACT_SCHEMA_VERSION}"),
            });
        }
        require_matching_id("run_id", &self.run_id, &record.run_id)?;
        require_matching_id("task_id", &self.task_id, &record.task_id)?;
        require_matching_id("lane_id", &self.lane_id, &record.lane_id)?;
        HepaArtifactId::new("transition_id", &record.transition_id)?;
        require_single_line_record("from_state", record.from_state.as_deref())?;
        require_single_line_record("to_state", Some(&record.to_state))?;
        require_single_line_record("reason", record.reason.as_deref())?;
        require_single_line_record("occurred_at", Some(&record.occurred_at))?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaAttemptArtifactPaths {
    pub schema_version: u32,
    pub run_id: HepaArtifactId,
    pub task_id: HepaArtifactId,
    pub lane_id: HepaArtifactId,
    pub attempt_id: HepaArtifactId,
    pub attempt_dir: PathBuf,
    pub attempt_report: PathBuf,
    pub stdout_log: PathBuf,
    pub stderr_log: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaStateTransitionRecord {
    pub schema_version: u32,
    pub run_id: String,
    pub task_id: String,
    pub lane_id: String,
    pub transition_id: String,
    pub entity: HepaStateEntity,
    pub from_state: Option<String>,
    pub to_state: String,
    pub reason: Option<String>,
    pub occurred_at: String,
}

impl HepaStateTransitionRecord {
    pub fn lane(
        run_id: impl Into<String>,
        task_id: impl Into<String>,
        lane_id: impl Into<String>,
        transition_id: impl Into<String>,
        from_state: Option<impl Into<String>>,
        to_state: impl Into<String>,
        occurred_at: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            run_id: run_id.into(),
            task_id: task_id.into(),
            lane_id: lane_id.into(),
            transition_id: transition_id.into(),
            entity: HepaStateEntity::Lane,
            from_state: from_state.map(Into::into),
            to_state: to_state.into(),
            reason: None,
            occurred_at: occurred_at.into(),
        }
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaStateEntity {
    Run,
    Task,
    Lane,
    Attempt,
    Validation,
    Review,
    Timing,
    FinalReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaStateWriteReceipt {
    pub transition_path: PathBuf,
    pub current_state_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaArchiveManifest {
    pub schema_version: u32,
    pub run_id: String,
    pub task_id: String,
    pub outcome: HepaArchiveOutcome,
    pub control_ref: String,
    pub archive_ref: String,
    pub archived_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaArchiveOutcome {
    Completed,
    Blocked,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HepaArtifactId(String);

impl HepaArtifactId {
    pub fn new(field: &str, value: impl Into<String>) -> Result<Self, HepaArtifactPathError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(HepaArtifactPathError::new(field, "must not be empty"));
        }
        if value.contains('\n') || value.contains('\r') {
            return Err(HepaArtifactPathError::new(field, "must be a single line"));
        }
        if value == "." || value == ".." {
            return Err(HepaArtifactPathError::new(
                field,
                "must not be a relative path segment",
            ));
        }
        if value.contains('/') || value.contains('\\') || value.contains("..") {
            return Err(HepaArtifactPathError::new(
                field,
                "must not contain path traversal characters",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HepaArtifactId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaArtifactPathError {
    pub field: String,
    pub message: String,
}

impl HepaArtifactPathError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaArtifactPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaArtifactPathError {}

#[derive(Debug)]
pub enum HepaArtifactWriteError {
    Path(HepaArtifactPathError),
    InvalidRecord { field: String, message: String },
    Io(io::Error),
    Serde(serde_json::Error),
}

impl fmt::Display for HepaArtifactWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(error) => write!(formatter, "{error}"),
            Self::InvalidRecord { field, message } => write!(formatter, "{field}: {message}"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Serde(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for HepaArtifactWriteError {}

impl From<HepaArtifactPathError> for HepaArtifactWriteError {
    fn from(error: HepaArtifactPathError) -> Self {
        Self::Path(error)
    }
}

impl From<io::Error> for HepaArtifactWriteError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for HepaArtifactWriteError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serde(error)
    }
}

fn require_matching_id(
    field: &str,
    expected: &HepaArtifactId,
    actual: &str,
) -> Result<(), HepaArtifactWriteError> {
    HepaArtifactId::new(field, actual)?;
    if expected.as_str() != actual {
        return Err(HepaArtifactWriteError::InvalidRecord {
            field: field.to_string(),
            message: "must match artifact path IDs".to_string(),
        });
    }
    Ok(())
}

fn require_single_line_record(
    field: &str,
    value: Option<&str>,
) -> Result<(), HepaArtifactWriteError> {
    if let Some(value) = value {
        if value.trim().is_empty() {
            return Err(HepaArtifactWriteError::InvalidRecord {
                field: field.to_string(),
                message: "must not be empty".to_string(),
            });
        }
        if value.contains('\n') || value.contains('\r') {
            return Err(HepaArtifactWriteError::InvalidRecord {
                field: field.to_string(),
                message: "must be a single line".to_string(),
            });
        }
    }
    Ok(())
}

fn write_stable_json<T>(path: &Path, value: &T) -> Result<(), HepaArtifactWriteError>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut json = serde_json::to_string_pretty(value)?;
    if !json.ends_with('\n') {
        json.push('\n');
    }
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, json)?;
    fs::rename(temp_path, path)?;
    Ok(())
}

fn copy_dir_contents(source: &Path, destination: &Path) -> Result<(), HepaArtifactWriteError> {
    if !source.exists() {
        return Ok(());
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_contents(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
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
    fn artifact_layout_defines_expected_control_and_archive_paths() {
        let layout =
            HepaArtifactLayout::new(".hepa/control", ".hepa/archive").expect("valid roots");
        let run = layout
            .run("run-20260616-001", "task-docs")
            .expect("valid run paths");
        let lane = run.lane("lane-a").expect("valid lane paths");
        let attempt = lane.attempt("attempt-001").expect("valid attempt paths");
        let review_signal = lane
            .review_signal("review-static")
            .expect("valid review signal path");

        assert_eq!(
            run.run_dir,
            PathBuf::from(".hepa/control/runs/run-20260616-001")
        );
        assert_eq!(
            run.run_state,
            PathBuf::from(".hepa/control/runs/run-20260616-001/run.json")
        );
        assert_eq!(
            run.task_state,
            PathBuf::from(".hepa/control/runs/run-20260616-001/tasks/task-docs/task.json")
        );
        assert_eq!(
            run.archive_manifest,
            PathBuf::from(".hepa/archive/runs/run-20260616-001/manifest.json")
        );
        assert_eq!(
            lane.lane_state,
            PathBuf::from(
                ".hepa/control/runs/run-20260616-001/tasks/task-docs/lanes/lane-a/lane.json"
            )
        );
        assert_eq!(
            attempt.attempt_report,
            PathBuf::from(
                ".hepa/control/runs/run-20260616-001/tasks/task-docs/lanes/lane-a/attempts/attempt-001/attempt.json"
            )
        );
        assert_eq!(
            lane.validation_summary,
            PathBuf::from(
                ".hepa/control/runs/run-20260616-001/tasks/task-docs/lanes/lane-a/validation/summary.json"
            )
        );
        assert_eq!(
            review_signal,
            PathBuf::from(
                ".hepa/control/runs/run-20260616-001/tasks/task-docs/lanes/lane-a/review/signals/review-static.json"
            )
        );
        assert_eq!(
            lane.current_state,
            PathBuf::from(
                ".hepa/control/runs/run-20260616-001/tasks/task-docs/lanes/lane-a/state/current.json"
            )
        );
        assert_eq!(
            lane.timing_record,
            PathBuf::from(
                ".hepa/control/runs/run-20260616-001/tasks/task-docs/lanes/lane-a/timing.json"
            )
        );
        assert_eq!(
            lane.final_report,
            PathBuf::from(
                ".hepa/control/runs/run-20260616-001/tasks/task-docs/lanes/lane-a/final-report.json"
            )
        );
    }

    #[test]
    fn artifact_layout_uses_configured_roots() {
        let config = HepaConfig {
            control_root: "tmp-control".to_string(),
            archive_root: "tmp-archive".to_string(),
            ..HepaConfig::default()
        };
        let layout = HepaArtifactLayout::from_config(&config).expect("valid config layout");
        let run = layout.run("run-1", "task-1").expect("valid run");

        assert_eq!(run.run_dir, PathBuf::from("tmp-control/runs/run-1"));
        assert_eq!(run.archive_dir, PathBuf::from("tmp-archive/runs/run-1"));
    }

    #[test]
    fn artifact_ids_reject_path_traversal() {
        let layout =
            HepaArtifactLayout::new(".hepa/control", ".hepa/archive").expect("valid roots");

        assert!(layout.run("../run", "task-1").is_err());
        assert!(layout.run("run-1", "task/1").is_err());
        assert!(layout.run("run-1", "task\\1").is_err());
        assert!(layout.run("run-1", "task\n1").is_err());
    }

    #[test]
    fn transition_state_writes_machine_readable_current_and_history() {
        let root = unique_test_dir("state-writes");
        let control = root.join("control");
        let archive = root.join("archive");
        let layout = HepaArtifactLayout::new(&control, &archive).expect("valid layout");
        let lane = layout
            .run("run-1", "task-1")
            .expect("valid run")
            .lane("lane-1")
            .expect("valid lane");

        let first = HepaStateTransitionRecord::lane(
            "run-1",
            "task-1",
            "lane-1",
            "001-allocated",
            None::<String>,
            "allocated",
            "2026-06-16T00:00:00Z",
        );
        let second = HepaStateTransitionRecord::lane(
            "run-1",
            "task-1",
            "lane-1",
            "002-running",
            Some("allocated"),
            "running",
            "2026-06-16T00:00:01Z",
        )
        .with_reason("worker started");

        lane.write_transition_state(&first)
            .expect("first transition writes");
        let receipt = lane
            .write_transition_state(&second)
            .expect("second transition writes");

        let first_json = fs::read_to_string(lane.transition_record("001-allocated").unwrap())
            .expect("first transition exists");
        let second_json =
            fs::read_to_string(receipt.transition_path).expect("second transition exists");
        let current_json =
            fs::read_to_string(receipt.current_state_path).expect("current state exists");

        assert!(first_json.contains("\"to_state\": \"allocated\""));
        assert!(second_json.contains("\"reason\": \"worker started\""));
        assert_eq!(current_json, second_json);

        remove_test_dir(root);
    }

    #[test]
    fn transition_state_rejects_records_for_other_lanes() {
        let root = unique_test_dir("state-rejects");
        let layout =
            HepaArtifactLayout::new(root.join("control"), root.join("archive")).expect("valid");
        let lane = layout
            .run("run-1", "task-1")
            .expect("valid run")
            .lane("lane-1")
            .expect("valid lane");
        let record = HepaStateTransitionRecord::lane(
            "run-1",
            "task-1",
            "lane-2",
            "001-allocated",
            None::<String>,
            "allocated",
            "2026-06-16T00:00:00Z",
        );

        assert!(matches!(
            lane.write_transition_state(&record),
            Err(HepaArtifactWriteError::InvalidRecord { .. })
        ));

        remove_test_dir(root);
    }

    #[test]
    fn archive_on_exit_copies_run_artifacts_with_portable_manifest() {
        let root = unique_test_dir("archive");
        let layout =
            HepaArtifactLayout::new(root.join("control"), root.join("archive")).expect("valid");
        let run = layout.run("run-1", "task-1").expect("valid run");
        let lane = run.lane("lane-1").expect("valid lane");
        let record = HepaStateTransitionRecord::lane(
            "run-1",
            "task-1",
            "lane-1",
            "001-completed",
            Some("running"),
            "completed",
            "2026-06-16T00:00:02Z",
        );
        lane.write_transition_state(&record)
            .expect("transition should write before archive");

        let manifest = run
            .archive_on_exit("2026-06-16T00:00:03Z", HepaArchiveOutcome::Completed)
            .expect("archive should succeed");
        let archived_state = run
            .archive_dir
            .join("tasks")
            .join("task-1")
            .join("lanes")
            .join("lane-1")
            .join("state")
            .join("current.json");
        let manifest_json =
            fs::read_to_string(&run.archive_manifest).expect("manifest should be written");

        assert_eq!(manifest.control_ref, "control:runs/run-1");
        assert_eq!(manifest.archive_ref, "archive:runs/run-1");
        assert!(archived_state.exists());
        assert!(!manifest_json.contains(root.to_string_lossy().as_ref()));
        assert!(manifest_json.contains("\"outcome\": \"completed\""));

        remove_test_dir(root);
    }

    #[test]
    fn archive_on_exit_preserves_interrupted_state_for_reconcile_and_cleanup() {
        let root = unique_test_dir("archive-interrupted");
        let layout =
            HepaArtifactLayout::new(root.join("control"), root.join("archive")).expect("valid");
        let run = layout.run("run-1", "task-1").expect("valid run");
        let lane = run.lane("lane-1").expect("valid lane");
        fs::create_dir_all(&run.task_dir).expect("task dir should write");
        fs::write(
            &run.task_state,
            "{\"task_id\":\"task-1\",\"status\":\"running\"}\n",
        )
        .expect("task state should write");
        let record = HepaStateTransitionRecord::lane(
            "run-1",
            "task-1",
            "lane-1",
            "001-running",
            Some("starting"),
            "running",
            "2026-06-16T00:00:02Z",
        )
        .with_reason("interrupted during worker attempt");
        lane.write_transition_state(&record)
            .expect("current lane state should write");

        let manifest = run
            .archive_on_exit("2026-06-16T00:00:03Z", HepaArchiveOutcome::Interrupted)
            .expect("interrupted run archive should preserve state");
        let archived_current_state = run
            .archive_dir
            .join("tasks/task-1/lanes/lane-1/state/current.json");
        let archived_task_state = run.archive_dir.join("tasks/task-1/task.json");
        let manifest_json =
            fs::read_to_string(&run.archive_manifest).expect("manifest should exist");

        assert_eq!(manifest.outcome, HepaArchiveOutcome::Interrupted);
        assert!(lane.current_state.exists());
        assert!(archived_current_state.exists());
        assert!(archived_task_state.exists());
        assert!(run.archive_manifest.exists());
        assert!(manifest_json.contains("\"outcome\": \"interrupted\""));
        assert!(!manifest_json.contains(root.to_string_lossy().as_ref()));

        remove_test_dir(root);
    }

    #[test]
    fn archive_artifacts_are_deterministic_and_path_redacted() {
        let first = archived_fixture_json("deterministic-a");
        let second = archived_fixture_json("deterministic-b");

        assert_eq!(first.manifest_json, second.manifest_json);
        assert_eq!(first.current_state_json, second.current_state_json);
        assert!(
            !first
                .manifest_json
                .contains(first.root.to_string_lossy().as_ref())
        );
        assert!(
            !first
                .current_state_json
                .contains(first.root.to_string_lossy().as_ref())
        );
        assert!(
            !second
                .manifest_json
                .contains(second.root.to_string_lossy().as_ref())
        );
        assert!(
            !second
                .current_state_json
                .contains(second.root.to_string_lossy().as_ref())
        );

        remove_test_dir(first.root);
        remove_test_dir(second.root);
    }

    #[test]
    fn timing_records_write_to_lane_artifacts() {
        let root = unique_test_dir("timing");
        let layout =
            HepaArtifactLayout::new(root.join("control"), root.join("archive")).expect("valid");
        let lane = layout
            .run("run-1", "task-1")
            .expect("valid run")
            .lane("lane-1")
            .expect("valid lane");
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

        let timing_path = lane
            .write_timing_record(&timing)
            .expect("timing should write");
        let timing_json = fs::read_to_string(timing_path).expect("timing artifact exists");

        assert!(timing_json.contains("\"run_id\": \"run-1\""));
        assert!(timing_json.contains("\"routing_reason\": \"default fake adapter\""));
        assert!(timing_json.contains("\"sandbox_posture\": \"host-worktree\""));

        remove_test_dir(root);
    }

    #[test]
    fn cost_reports_write_to_lane_artifacts() {
        let root = unique_test_dir("cost");
        let layout =
            HepaArtifactLayout::new(root.join("control"), root.join("archive")).expect("valid");
        let lane = layout
            .run("run-1", "task-1")
            .expect("valid run")
            .lane("lane-1")
            .expect("valid lane");
        let report = crate::cost_accounting::HepaLaneCostReport::from_entries(
            "run-1",
            "task-1",
            "lane-1",
            vec![crate::cost_accounting::HepaAdapterUsageEntry {
                adapter_id: "pi".to_string(),
                invocation_id: "attempt-1".to_string(),
                cost_class: crate::cost_accounting::HepaUsageCostClass::PaidCloud,
                input_tokens: Some(10),
                output_tokens: Some(5),
                total_tokens: Some(15),
                cost_micros: Some(100),
                currency: Some("USD".to_string()),
                source: crate::cost_accounting::HepaUsageSource::AdapterReported,
            }],
            "2026-06-18T00:00:00Z",
        )
        .expect("cost report should validate");

        let cost_path = lane
            .write_cost_report(&report)
            .expect("cost report should write");
        let cost_json = fs::read_to_string(cost_path).expect("cost artifact exists");

        assert!(cost_json.contains("\"total_cost_micros\": 100"));
        assert!(cost_json.contains("\"currency_totals\""));
        assert!(cost_json.contains("\"USD\": 100"));

        remove_test_dir(root);
    }

    struct ArchivedFixtureJson {
        root: PathBuf,
        manifest_json: String,
        current_state_json: String,
    }

    fn archived_fixture_json(label: &str) -> ArchivedFixtureJson {
        let root = unique_test_dir(label);
        let layout =
            HepaArtifactLayout::new(root.join("control"), root.join("archive")).expect("valid");
        let run = layout.run("run-1", "task-1").expect("valid run");
        let lane = run.lane("lane-1").expect("valid lane");
        let record = HepaStateTransitionRecord::lane(
            "run-1",
            "task-1",
            "lane-1",
            "001-completed",
            Some("running"),
            "completed",
            "2026-06-16T00:00:02Z",
        )
        .with_reason("deterministic fixture");
        lane.write_transition_state(&record)
            .expect("transition should write");
        run.archive_on_exit("2026-06-16T00:00:03Z", HepaArchiveOutcome::Completed)
            .expect("archive should write");

        ArchivedFixtureJson {
            manifest_json: fs::read_to_string(&run.archive_manifest).expect("manifest exists"),
            current_state_json: fs::read_to_string(
                run.archive_dir
                    .join("tasks/task-1/lanes/lane-1/state/current.json"),
            )
            .expect("current state exists"),
            root,
        }
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-artifacts-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
