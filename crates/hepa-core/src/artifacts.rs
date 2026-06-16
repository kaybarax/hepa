use crate::config::HepaConfig;
use std::{
    error::Error,
    fmt,
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
            timing_record: lane_dir.join("timing.json"),
            final_report: lane_dir.join("final-report.json"),
        })
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
    pub timing_record: PathBuf,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
