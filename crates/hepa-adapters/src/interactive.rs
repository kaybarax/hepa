use hepa_core::{contracts::HepaLaneState, lane_state::HepaLaneStateExt};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

pub const INTERACTIVE_SESSION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaInteractiveSessionRequest {
    pub lane_id: String,
    pub adapter_id: String,
    pub command: String,
    pub workdir: PathBuf,
    pub artifact_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaInteractiveSessionRecord {
    pub schema_version: u32,
    pub lane_id: String,
    pub adapter_id: String,
    pub session_id: String,
    pub command: String,
    pub workdir_ref: String,
    pub full_log_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaInteractiveSessionReceipt {
    pub record: HepaInteractiveSessionRecord,
    pub record_path: PathBuf,
    pub full_log_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaLaneSteeringRequest {
    pub lane_id: String,
    pub message: String,
    pub manager_approved: bool,
    pub dry_run: bool,
    pub lane_state: HepaLaneState,
    pub artifact_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaLaneSteeringReceipt {
    pub lane_id: String,
    pub session_id: String,
    pub sent: bool,
    pub log_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaLaneSteeringRecord {
    pub schema_version: u32,
    pub lane_id: String,
    pub session_id: String,
    pub message: String,
    pub manager_approved: bool,
    pub dry_run: bool,
    pub lane_state: HepaLaneState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaInteractiveTeardownReceipt {
    pub lane_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HepaTmuxInteractiveLauncher;

impl HepaTmuxInteractiveLauncher {
    pub fn launch(
        &self,
        request: &HepaInteractiveSessionRequest,
        tmux: &mut impl HepaTmux,
    ) -> Result<HepaInteractiveSessionReceipt, HepaInteractiveSessionError> {
        request.validate()?;
        fs::create_dir_all(&request.artifact_dir).map_err(|error| {
            HepaInteractiveSessionError::new("artifact_dir", format!("failed to create: {error}"))
        })?;
        let session_id = interactive_session_id(&request.lane_id);
        tmux.new_session(&session_id, &request.command, &request.workdir)?;
        let full_log = tmux.capture_pane(&session_id)?;
        let full_log_path = request.artifact_dir.join("interactive-full.log");
        fs::write(&full_log_path, full_log).map_err(|error| {
            HepaInteractiveSessionError::new("full_log", format!("failed to write: {error}"))
        })?;
        let record = HepaInteractiveSessionRecord {
            schema_version: INTERACTIVE_SESSION_SCHEMA_VERSION,
            lane_id: request.lane_id.clone(),
            adapter_id: request.adapter_id.clone(),
            session_id,
            command: request.command.clone(),
            workdir_ref: "<LANE_WORKTREE>".to_string(),
            full_log_ref: "interactive-full.log".to_string(),
        };
        let record_path = request.artifact_dir.join("interactive-session.json");
        write_stable_json(&record_path, &record)?;
        Ok(HepaInteractiveSessionReceipt {
            record,
            record_path,
            full_log_path,
        })
    }

    pub fn send(
        &self,
        request: &HepaLaneSteeringRequest,
        tmux: &mut impl HepaTmux,
    ) -> Result<HepaLaneSteeringReceipt, HepaInteractiveSessionError> {
        request.validate()?;
        let session_id = interactive_session_id(&request.lane_id);
        fs::create_dir_all(&request.artifact_dir).map_err(|error| {
            HepaInteractiveSessionError::new("artifact_dir", format!("failed to create: {error}"))
        })?;
        let record = HepaLaneSteeringRecord {
            schema_version: INTERACTIVE_SESSION_SCHEMA_VERSION,
            lane_id: request.lane_id.clone(),
            session_id: session_id.clone(),
            message: request.message.clone(),
            manager_approved: request.manager_approved,
            dry_run: request.dry_run,
            lane_state: request.lane_state.clone(),
        };
        let log_path = request.artifact_dir.join("steering-log.jsonl");
        append_stable_json_line(&log_path, &record)?;
        if !request.dry_run {
            tmux.send_keys(&session_id, &request.message)?;
        }
        Ok(HepaLaneSteeringReceipt {
            lane_id: request.lane_id.clone(),
            session_id,
            sent: !request.dry_run,
            log_path,
        })
    }

    pub fn teardown(
        &self,
        lane_id: &str,
        tmux: &mut impl HepaTmux,
    ) -> Result<HepaInteractiveTeardownReceipt, HepaInteractiveSessionError> {
        require_artifact_id("lane_id", lane_id)?;
        let session_id = interactive_session_id(lane_id);
        tmux.kill_session(&session_id)?;
        Ok(HepaInteractiveTeardownReceipt {
            lane_id: lane_id.to_string(),
            session_id,
        })
    }
}

pub trait HepaTmux {
    fn new_session(
        &mut self,
        session_id: &str,
        command: &str,
        workdir: &Path,
    ) -> Result<(), HepaInteractiveSessionError>;

    fn capture_pane(&mut self, session_id: &str) -> Result<String, HepaInteractiveSessionError>;

    fn send_keys(
        &mut self,
        session_id: &str,
        message: &str,
    ) -> Result<(), HepaInteractiveSessionError>;

    fn kill_session(&mut self, session_id: &str) -> Result<(), HepaInteractiveSessionError>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HepaSystemTmux;

impl HepaTmux for HepaSystemTmux {
    fn new_session(
        &mut self,
        session_id: &str,
        command: &str,
        workdir: &Path,
    ) -> Result<(), HepaInteractiveSessionError> {
        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", session_id, command])
            .current_dir(workdir)
            .status()
            .map_err(|error| HepaInteractiveSessionError::tmux("new-session", error))?;
        if status.success() {
            Ok(())
        } else {
            Err(HepaInteractiveSessionError::new(
                "tmux",
                format!("new-session exited with {status}"),
            ))
        }
    }

    fn capture_pane(&mut self, session_id: &str) -> Result<String, HepaInteractiveSessionError> {
        let output = Command::new("tmux")
            .args(["capture-pane", "-p", "-S", "-", "-t", session_id])
            .output()
            .map_err(|error| HepaInteractiveSessionError::tmux("capture-pane", error))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(HepaInteractiveSessionError::new(
                "tmux",
                format!("capture-pane exited with {}", output.status),
            ))
        }
    }

    fn send_keys(
        &mut self,
        session_id: &str,
        message: &str,
    ) -> Result<(), HepaInteractiveSessionError> {
        let status = Command::new("tmux")
            .args(["send-keys", "-t", session_id, message, "Enter"])
            .status()
            .map_err(|error| HepaInteractiveSessionError::tmux("send-keys", error))?;
        if status.success() {
            Ok(())
        } else {
            Err(HepaInteractiveSessionError::new(
                "tmux",
                format!("send-keys exited with {status}"),
            ))
        }
    }

    fn kill_session(&mut self, session_id: &str) -> Result<(), HepaInteractiveSessionError> {
        let status = Command::new("tmux")
            .args(["kill-session", "-t", session_id])
            .status()
            .map_err(|error| HepaInteractiveSessionError::tmux("kill-session", error))?;
        if status.success() {
            Ok(())
        } else {
            Err(HepaInteractiveSessionError::new(
                "tmux",
                format!("kill-session exited with {status}"),
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaInteractiveSessionError {
    pub field: String,
    pub message: String,
}

impl HepaInteractiveSessionError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }

    fn tmux(action: &str, error: io::Error) -> Self {
        Self::new("tmux", format!("{action} failed: {error}"))
    }
}

impl fmt::Display for HepaInteractiveSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaInteractiveSessionError {}

impl HepaInteractiveSessionRequest {
    fn validate(&self) -> Result<(), HepaInteractiveSessionError> {
        require_artifact_id("lane_id", &self.lane_id)?;
        require_artifact_id("adapter_id", &self.adapter_id)?;
        require_single_line("command", &self.command)?;
        if !self.workdir.is_dir() {
            return Err(HepaInteractiveSessionError::new(
                "workdir",
                "lane worktree must exist before interactive launch",
            ));
        }
        Ok(())
    }
}

impl HepaLaneSteeringRequest {
    fn validate(&self) -> Result<(), HepaInteractiveSessionError> {
        require_artifact_id("lane_id", &self.lane_id)?;
        require_single_line("message", &self.message)?;
        reject_secret_content("message", &self.message)?;
        if !self.manager_approved {
            return Err(HepaInteractiveSessionError::new(
                "manager_approved",
                "manager approval is required before lane steering",
            ));
        }
        if self.lane_state.is_terminal() {
            return Err(HepaInteractiveSessionError::new(
                "lane_state",
                "terminal lanes cannot be steered",
            ));
        }
        if self.artifact_dir.as_os_str().is_empty() {
            return Err(HepaInteractiveSessionError::new(
                "artifact_dir",
                "artifact logging is required before lane steering",
            ));
        }
        Ok(())
    }
}

pub fn interactive_session_id(lane_id: &str) -> String {
    format!("hepa-{lane_id}")
}

fn require_artifact_id(field: &str, value: &str) -> Result<(), HepaInteractiveSessionError> {
    require_single_line(field, value)?;
    if value
        .chars()
        .any(|character| !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_')))
    {
        return Err(HepaInteractiveSessionError::new(
            field,
            "must contain only ASCII letters, digits, hyphen, or underscore",
        ));
    }
    Ok(())
}

fn require_single_line(field: &str, value: &str) -> Result<(), HepaInteractiveSessionError> {
    if value.trim().is_empty() {
        return Err(HepaInteractiveSessionError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaInteractiveSessionError::new(
            field,
            "must be a single line",
        ));
    }
    Ok(())
}

fn reject_secret_content(field: &str, value: &str) -> Result<(), HepaInteractiveSessionError> {
    let lowered = value.to_ascii_lowercase();
    let token_prefix = ["ghp", "_"].concat();
    if lowered.contains(&token_prefix)
        || [
            ".env",
            "api_key",
            "apikey",
            "credential",
            "id_rsa",
            "password",
            "private_key",
            "secret",
            "token=",
        ]
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return Err(HepaInteractiveSessionError::new(
            field,
            "secret-like steering content is rejected",
        ));
    }
    Ok(())
}

fn write_stable_json<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), HepaInteractiveSessionError> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|error| HepaInteractiveSessionError::new("record", error.to_string()))?;
    let json = format!("{json}\n");
    fs::write(path, json).map_err(|error| {
        HepaInteractiveSessionError::new("record", format!("failed to write: {error}"))
    })
}

fn append_stable_json_line<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), HepaInteractiveSessionError> {
    let json = serde_json::to_string(value)
        .map_err(|error| HepaInteractiveSessionError::new("steering_log", error.to_string()))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            HepaInteractiveSessionError::new("steering_log", format!("failed to open: {error}"))
        })?;
    writeln!(file, "{json}").map_err(|error| {
        HepaInteractiveSessionError::new("steering_log", format!("failed to write: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct FakeTmux {
        launched: Vec<(String, String, PathBuf)>,
        captured: Vec<String>,
        sent: Vec<(String, String)>,
        killed: Vec<String>,
        capture_output: String,
    }

    impl HepaTmux for FakeTmux {
        fn new_session(
            &mut self,
            session_id: &str,
            command: &str,
            workdir: &Path,
        ) -> Result<(), HepaInteractiveSessionError> {
            self.launched.push((
                session_id.to_string(),
                command.to_string(),
                workdir.to_path_buf(),
            ));
            Ok(())
        }

        fn capture_pane(
            &mut self,
            session_id: &str,
        ) -> Result<String, HepaInteractiveSessionError> {
            self.captured.push(session_id.to_string());
            Ok(self.capture_output.clone())
        }

        fn send_keys(
            &mut self,
            session_id: &str,
            message: &str,
        ) -> Result<(), HepaInteractiveSessionError> {
            self.sent
                .push((session_id.to_string(), message.to_string()));
            Ok(())
        }

        fn kill_session(&mut self, session_id: &str) -> Result<(), HepaInteractiveSessionError> {
            self.killed.push(session_id.to_string());
            Ok(())
        }
    }

    #[test]
    fn tmux_interactive_launch_records_session_id_and_full_log() {
        let root = unique_test_dir("tmux-launch");
        let workdir = root.join("worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&workdir).expect("workdir");
        let mut tmux = FakeTmux {
            capture_output: "full transcript\nwith prompt and output\n".to_string(),
            ..FakeTmux::default()
        };
        let request = HepaInteractiveSessionRequest {
            lane_id: "lane_1".to_string(),
            adapter_id: "user-worker".to_string(),
            command: "agent --prompt-file prompt.md".to_string(),
            workdir: workdir.clone(),
            artifact_dir: artifact_dir.clone(),
        };

        let receipt = HepaTmuxInteractiveLauncher
            .launch(&request, &mut tmux)
            .expect("interactive launch should record artifacts");

        assert_eq!(receipt.record.session_id, "hepa-lane_1");
        assert_eq!(
            tmux.launched,
            vec![(
                "hepa-lane_1".to_string(),
                "agent --prompt-file prompt.md".to_string(),
                workdir
            )]
        );
        assert_eq!(tmux.captured, vec!["hepa-lane_1"]);
        assert_eq!(
            fs::read_to_string(&receipt.full_log_path).expect("full log"),
            "full transcript\nwith prompt and output\n"
        );
        let record_json = fs::read_to_string(&receipt.record_path).expect("session record");
        assert!(record_json.contains("\"session_id\": \"hepa-lane_1\""));
        assert!(record_json.contains("\"full_log_ref\": \"interactive-full.log\""));
        assert!(record_json.contains("\"workdir_ref\": \"<LANE_WORKTREE>\""));

        remove_test_dir(root);
    }

    #[test]
    fn tmux_interactive_launch_rejects_invalid_request_before_tmux() {
        let root = unique_test_dir("tmux-reject");
        let workdir = root.join("worktree");
        fs::create_dir_all(&workdir).expect("workdir");
        let mut tmux = FakeTmux::default();
        let request = HepaInteractiveSessionRequest {
            lane_id: "../lane".to_string(),
            adapter_id: "user-worker".to_string(),
            command: "agent".to_string(),
            workdir,
            artifact_dir: root.join("artifacts"),
        };

        let error = HepaTmuxInteractiveLauncher
            .launch(&request, &mut tmux)
            .expect_err("invalid lane id must fail");

        assert_eq!(error.field, "lane_id");
        assert!(tmux.launched.is_empty());
        assert!(tmux.captured.is_empty());

        remove_test_dir(root);
    }

    #[test]
    fn lane_send_uses_lane_id_derived_tmux_session() {
        let root = unique_test_dir("tmux-send");
        let mut tmux = FakeTmux::default();
        let request = HepaLaneSteeringRequest {
            lane_id: "lane_1".to_string(),
            message: "continue with the focused fix".to_string(),
            manager_approved: true,
            dry_run: false,
            lane_state: HepaLaneState::Running,
            artifact_dir: root.join("artifacts"),
        };

        let receipt = HepaTmuxInteractiveLauncher
            .send(&request, &mut tmux)
            .expect("lane send should target tmux session");

        assert_eq!(receipt.lane_id, "lane_1");
        assert_eq!(receipt.session_id, "hepa-lane_1");
        assert!(receipt.sent);
        assert_eq!(
            tmux.sent,
            vec![(
                "hepa-lane_1".to_string(),
                "continue with the focused fix".to_string()
            )]
        );
        let log = fs::read_to_string(receipt.log_path).expect("steering log");
        assert!(log.contains("\"manager_approved\":true"));
        assert!(log.contains("\"message\":\"continue with the focused fix\""));

        remove_test_dir(root);
    }

    #[test]
    fn lane_send_dry_run_logs_without_sending_to_tmux() {
        let root = unique_test_dir("tmux-send-dry-run");
        let mut tmux = FakeTmux::default();
        let request = HepaLaneSteeringRequest {
            lane_id: "lane_1".to_string(),
            message: "show current status".to_string(),
            manager_approved: true,
            dry_run: true,
            lane_state: HepaLaneState::Running,
            artifact_dir: root.join("artifacts"),
        };

        let receipt = HepaTmuxInteractiveLauncher
            .send(&request, &mut tmux)
            .expect("dry-run should log steering");

        assert!(!receipt.sent);
        assert!(tmux.sent.is_empty());
        let log = fs::read_to_string(receipt.log_path).expect("steering log");
        assert!(log.contains("\"dry_run\":true"));

        remove_test_dir(root);
    }

    #[test]
    fn lane_send_rejects_unapproved_secret_and_terminal_messages_before_tmux() {
        let root = unique_test_dir("tmux-send-reject");
        let mut tmux = FakeTmux::default();
        let base = HepaLaneSteeringRequest {
            lane_id: "lane_1".to_string(),
            message: "continue".to_string(),
            manager_approved: true,
            dry_run: false,
            lane_state: HepaLaneState::Running,
            artifact_dir: root.join("artifacts"),
        };

        let mut unapproved = base.clone();
        unapproved.manager_approved = false;
        assert_eq!(
            HepaTmuxInteractiveLauncher
                .send(&unapproved, &mut tmux)
                .expect_err("unapproved send must fail")
                .field,
            "manager_approved"
        );

        let mut secret = base.clone();
        secret.message = "password=blocked".to_string();
        assert_eq!(
            HepaTmuxInteractiveLauncher
                .send(&secret, &mut tmux)
                .expect_err("secret-like content must fail")
                .field,
            "message"
        );

        let mut terminal = base;
        terminal.lane_state = HepaLaneState::Completed;
        assert_eq!(
            HepaTmuxInteractiveLauncher
                .send(&terminal, &mut tmux)
                .expect_err("terminal lane must fail")
                .field,
            "lane_state"
        );
        assert!(tmux.sent.is_empty());
        assert!(!root.join("artifacts").exists());

        remove_test_dir(root);
    }

    #[test]
    fn tmux_interactive_teardown_kills_lane_session() {
        let mut tmux = FakeTmux::default();

        let receipt = HepaTmuxInteractiveLauncher
            .teardown("lane_1", &mut tmux)
            .expect("teardown should kill the lane session");

        assert_eq!(receipt.lane_id, "lane_1");
        assert_eq!(receipt.session_id, "hepa-lane_1");
        assert_eq!(tmux.killed, vec!["hepa-lane_1"]);
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-interactive-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
