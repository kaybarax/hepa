use hepa_core::contracts::{
    HepaAgentRole, HepaFindingSeverity, HepaHermesPrIntent, HepaLane, HepaReviewStatus,
    HepaRiskLevel, HepaTaskSpec, HepaTerminalStatus, HepaTerminalTaskReport, HepaValidate,
    HepaValidationStatus,
};
use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
    process::Command,
};

/// The role a git/PR operation is being attempted under.
///
/// Only the manager may run repository lifecycle actions (commit, push, PR
/// creation). Worker and reviewer adapters are constructed with their own roles
/// and are refused here, complementing the deterministic monitor that blocks the
/// same commands in adapter-composed shell strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HepaGitRole {
    Manager,
    Worker,
    Reviewer,
}

impl HepaGitRole {
    fn label(self) -> &'static str {
        match self {
            HepaGitRole::Manager => "manager",
            HepaGitRole::Worker => "worker",
            HepaGitRole::Reviewer => "reviewer",
        }
    }
}

/// Output of an external process invocation (git push, gh).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaProcessOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl HepaProcessOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }
}

/// Injectable runner for network-touching commands so PR/push flows can be
/// proven with a fake `gh`/`git` and never require a live remote in tests.
pub trait HepaProcessRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        cwd: &Path,
    ) -> Result<HepaProcessOutput, HepaPrError>;
}

/// Default runner that shells out to real binaries.
#[derive(Debug, Default, Clone, Copy)]
pub struct HepaSystemProcessRunner;

impl HepaProcessRunner for HepaSystemProcessRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        cwd: &Path,
    ) -> Result<HepaProcessOutput, HepaPrError> {
        let output = Command::new(program).args(args).current_dir(cwd).output()?;
        Ok(HepaProcessOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// A manager commit message: a single-line title plus optional body lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaCommitMessage {
    pub title: String,
    pub body: Vec<String>,
}

impl HepaCommitMessage {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: Vec::new(),
        }
    }

    pub fn with_body(mut self, body: Vec<String>) -> Self {
        self.body = body;
        self
    }
}

/// A manager-side pull-request request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaPrRequest {
    pub title: String,
    pub body: String,
    pub base_branch: String,
    pub head_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaCommitOutcome {
    pub commit_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaPrHandle {
    pub url: String,
}

pub fn pr_request_from_hermes_intent(
    intent: &HepaHermesPrIntent,
    base_branch: impl Into<String>,
    head_branch: impl Into<String>,
) -> Result<HepaPrRequest, HepaPrError> {
    intent
        .validate()
        .map_err(|error| HepaPrError::new(error.field, error.message))?;

    Ok(HepaPrRequest {
        title: intent.title.clone(),
        body: intent.body.trim_end().to_string(),
        base_branch: base_branch.into(),
        head_branch: head_branch.into(),
    })
}

pub fn pr_request_from_hermes_intent_with_run_evidence(
    intent: &HepaHermesPrIntent,
    evidence: &HepaPrBodyInput<'_>,
    base_branch: impl Into<String>,
    head_branch: impl Into<String>,
) -> Result<HepaPrRequest, HepaPrError> {
    intent
        .validate()
        .map_err(|error| HepaPrError::new(error.field, error.message))?;
    let _ = evidence;

    Ok(HepaPrRequest {
        title: intent.title.clone(),
        body: intent.body.trim_end().to_string(),
        base_branch: base_branch.into(),
        head_branch: head_branch.into(),
    })
}

/// Manager-owned git lifecycle. The only type exposing commit/push/PR creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaManagerGitLifecycle {
    repo_root: PathBuf,
    role: HepaGitRole,
}

impl HepaManagerGitLifecycle {
    /// Construct the manager-authorized lifecycle.
    pub fn manager(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
            role: HepaGitRole::Manager,
        }
    }

    /// Construct a lifecycle under an explicit role. Used to prove that
    /// worker/reviewer roles are refused before any lifecycle command runs.
    pub fn for_role(repo_root: impl Into<PathBuf>, role: HepaGitRole) -> Self {
        Self {
            repo_root: repo_root.into(),
            role,
        }
    }

    /// Commit already-staged changes. Refuses empty commits and non-manager
    /// roles, and never composes `--author`/co-author trailers.
    pub fn commit_staged(
        &self,
        message: &HepaCommitMessage,
    ) -> Result<HepaCommitOutcome, HepaPrError> {
        self.require_manager()?;
        require_single_line("title", &message.title)?;
        for line in &message.body {
            if line.contains('\r') {
                return Err(HepaPrError::new(
                    "body",
                    "must not contain carriage returns",
                ));
            }
        }
        if !self.has_staged_changes()? {
            return Err(HepaPrError::new(
                "staged",
                "refusing to create an empty commit with no staged changes",
            ));
        }

        let mut args = vec![
            "commit".to_string(),
            "--no-verify".to_string(),
            "-m".to_string(),
            message.title.clone(),
        ];
        if !message.body.is_empty() {
            args.push("-m".to_string());
            args.push(message.body.join("\n"));
        }
        self.git(&args)?;
        let commit_sha = self.git(&["rev-parse".to_string(), "HEAD".to_string()])?;
        Ok(HepaCommitOutcome { commit_sha })
    }

    /// Push the manager branch through the injected runner.
    pub fn push_branch(
        &self,
        remote: &str,
        branch: &str,
        runner: &dyn HepaProcessRunner,
    ) -> Result<HepaProcessOutput, HepaPrError> {
        self.require_manager()?;
        require_single_line("remote", remote)?;
        require_manager_branch(branch)?;
        let args = vec![
            "push".to_string(),
            "--set-upstream".to_string(),
            remote.to_string(),
            branch.to_string(),
        ];
        let output = runner.run("git", &args, &self.repo_root)?;
        if !output.success() {
            return Err(HepaPrError::new("push", output.stderr));
        }
        Ok(output)
    }

    /// Create a pull request through the injected runner (real `gh` by default).
    pub fn create_pr(
        &self,
        request: &HepaPrRequest,
        runner: &dyn HepaProcessRunner,
    ) -> Result<HepaPrHandle, HepaPrError> {
        self.require_manager()?;
        require_single_line("title", &request.title)?;
        require_single_line("base_branch", &request.base_branch)?;
        require_manager_branch(&request.head_branch)?;
        if request.body.trim().is_empty() {
            return Err(HepaPrError::new("body", "PR body must not be empty"));
        }
        self.require_base_not_ahead_of_remote(&request.base_branch)?;

        let args = vec![
            "pr".to_string(),
            "create".to_string(),
            "--title".to_string(),
            request.title.clone(),
            "--body".to_string(),
            request.body.clone(),
            "--base".to_string(),
            request.base_branch.clone(),
            "--head".to_string(),
            request.head_branch.clone(),
        ];
        let output = runner.run("gh", &args, &self.repo_root)?;
        if !output.success() {
            return Err(HepaPrError::new("gh", output.stderr));
        }
        let url = output
            .stdout
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("http"))
            .map(str::to_string)
            .ok_or_else(|| HepaPrError::new("gh", "gh pr create did not return a PR URL"))?;
        Ok(HepaPrHandle { url })
    }

    fn require_manager(&self) -> Result<(), HepaPrError> {
        if self.role != HepaGitRole::Manager {
            return Err(HepaPrError::new(
                "role",
                format!(
                    "git lifecycle actions are manager-owned; {} role is refused",
                    self.role.label()
                ),
            ));
        }
        Ok(())
    }

    fn has_staged_changes(&self) -> Result<bool, HepaPrError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .args(["diff", "--cached", "--name-only"])
            .output()?;
        if !output.status.success() {
            return Err(HepaPrError::new(
                "git",
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
    }

    fn require_base_not_ahead_of_remote(&self, base_branch: &str) -> Result<(), HepaPrError> {
        let remote_ref = format!("origin/{base_branch}");
        if !self.git_ref_exists(&remote_ref)? || !self.git_ref_exists(base_branch)? {
            return Ok(());
        }
        let ahead = self.git(&[
            "rev-list".to_string(),
            "--count".to_string(),
            format!("{remote_ref}..{base_branch}"),
        ])?;
        let ahead = ahead.trim().parse::<u64>().unwrap_or(0);
        if ahead > 0 {
            return Err(HepaPrError::new(
                "base_branch",
                format!(
                    "local base branch `{base_branch}` is {ahead} commit(s) ahead of `{remote_ref}`; refusing to create a polluted PR"
                ),
            ));
        }
        Ok(())
    }

    fn git_ref_exists(&self, reference: &str) -> Result<bool, HepaPrError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .args(["rev-parse", "--verify", "--quiet", reference])
            .output()?;
        Ok(output.status.success())
    }

    fn git(&self, args: &[String]) -> Result<String, HepaPrError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(HepaPrError::new(
                "git",
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

/// Inputs needed to honestly reconstruct the run in a PR body.
#[derive(Debug, Clone, Copy)]
pub struct HepaPrBodyInput<'a> {
    pub task_spec: &'a HepaTaskSpec,
    pub terminal_report: &'a HepaTerminalTaskReport,
    pub lane: &'a HepaLane,
    pub external_card_id: Option<&'a str>,
}

/// Build a deterministic, sanitized PR body that reconstructs the run:
/// summary, validation, review, risk, adapter, timing, and the Hermes card link.
pub fn build_pr_body(input: &HepaPrBodyInput) -> String {
    let mut lines = vec![
        "## Fallback Evidence Artifact".to_string(),
        "- This body is HEPA's deterministic headless/degraded fallback evidence artifact, not Hermes-authored project PR intent."
            .to_string(),
        "- In Hermes-present release runs, `hepa-manager` must author `HepaHermesPrIntent` for project-specific PR content."
            .to_string(),
        String::new(),
    ];
    lines.extend(run_evidence_lines(input));

    let mut body = lines.join("\n");
    body.push('\n');
    body
}

pub fn build_hermes_run_evidence_body(input: &HepaPrBodyInput) -> String {
    let mut lines = Vec::new();
    lines.push("## HEPA run evidence".to_string());
    lines.push(
        "HEPA reconstructed these details from deterministic lane artifacts before publishing the manager-owned PR."
            .to_string(),
    );
    lines.push(String::new());
    lines.extend(run_evidence_lines(input));

    let mut body = lines.join("\n");
    body.push('\n');
    body
}

fn run_evidence_lines(input: &HepaPrBodyInput) -> Vec<String> {
    let report = input.terminal_report;
    let mut lines = Vec::new();

    lines.push("## Summary".to_string());
    lines.push(format!("- Status: {}", terminal_status_label(report)));
    lines.push(format!("- Task: {}", input.task_spec.goal));
    if !input.task_spec.expected_areas.is_empty() {
        lines.push(format!(
            "- Expected areas: {}",
            input.task_spec.expected_areas.join(", ")
        ));
    }
    if !input.task_spec.acceptance_criteria.is_empty() {
        lines.push("- Acceptance criteria:".to_string());
        for criterion in input.task_spec.acceptance_criteria.iter().take(8) {
            lines.push(format!("  - {criterion}"));
        }
        if input.task_spec.acceptance_criteria.len() > 8 {
            lines.push(format!(
                "  - ({} more omitted)",
                input.task_spec.acceptance_criteria.len() - 8
            ));
        }
    }
    if report.summary.is_empty() {
        lines.push("- No summary recorded.".to_string());
    } else {
        for entry in &report.summary {
            lines.push(format!("- {entry}"));
        }
    }

    lines.push(String::new());
    lines.push("## Validation".to_string());
    match &report.validation {
        Some(validation) => {
            lines.push(format!(
                "- Result: {}",
                validation_status_label(&validation.status)
            ));
            if validation.no_tests_detected {
                lines.push("- No tests detected (honestly recorded).".to_string());
            }
            if let Some(failure_type) = &validation.failure_type {
                lines.push(format!("- Failure type: {failure_type}"));
            }
            for command in &validation.commands {
                lines.push(format!(
                    "- `{}` exited {}",
                    command.command, command.exit_code
                ));
            }
        }
        None => lines.push("- No validation summary recorded.".to_string()),
    }

    lines.push(String::new());
    lines.push("## Review".to_string());
    if report.review_signals.is_empty() {
        lines.push("- No review signals recorded.".to_string());
    } else {
        for signal in &report.review_signals {
            lines.push(format!(
                "- {} via `{}`: {} ({} finding(s))",
                signal.review_id,
                signal.adapter_id,
                review_status_label(&signal.status),
                signal.findings.len()
            ));
            for finding in &signal.findings {
                lines.push(format!(
                    "  - {} [{}] {} (accepted={})",
                    finding.finding_id,
                    severity_label(&finding.severity),
                    finding.message,
                    finding.accepted
                ));
            }
        }
    }

    lines.push(String::new());
    lines.push("## Risk".to_string());
    lines.push(format!("- Declared risk: {}", risk_label(input)));
    lines.push(format!(
        "- Human attention required: {}",
        report.human_attention_required
    ));
    match &report.arbitration {
        Some(arbitration) => {
            lines.push(format!("- Arbitration: {}", arbitration.status));
            for entry in &arbitration.pr_body_lines {
                lines.push(format!("  {entry}"));
            }
        }
        None => lines.push("- Arbitration: none required.".to_string()),
    }

    lines.push(String::new());
    lines.push("## Adapter".to_string());
    lines.push(format!("- Lane adapter: {}", input.lane.adapter_id));
    for adapter_line in adapter_phase_lines(report) {
        lines.push(adapter_line);
    }
    let postures = sandbox_postures(report);
    if postures.is_empty() {
        lines.push("- Sandbox posture: not recorded".to_string());
    } else {
        lines.push(format!("- Sandbox posture: {}", postures.join(", ")));
    }

    lines.push(String::new());
    lines.push("## Timing".to_string());
    match &report.timing {
        Some(timing) => {
            let total: f64 = timing
                .phases
                .iter()
                .map(|phase| phase.duration_seconds)
                .sum();
            lines.push(format!(
                "- Wall time: {total:.2}s across {} phase(s)",
                timing.phases.len()
            ));
            let counters = &timing.counters;
            lines.push(format!(
                "- Agent loops: {} | manager passes: {} | reviewer passes: {}",
                counters.agent_loops, counters.manager_passes, counters.reviewer_passes
            ));
            lines.push(format!(
                "- Worker-profile calls: {} | installs: {} | containers: {}",
                counters.worker_profile_llm_calls,
                counters.install_events,
                counters.container_count
            ));
        }
        None => lines.push("- No timing record captured.".to_string()),
    }

    lines.push(String::new());
    lines.push("## Hermes card".to_string());
    match input.external_card_id {
        Some(card_id) if !card_id.trim().is_empty() => {
            lines.push(format!("- Card: {card_id}"));
        }
        _ => lines.push("- No Hermes card linked.".to_string()),
    }

    lines
}

fn terminal_status_label(report: &HepaTerminalTaskReport) -> &'static str {
    match report.status {
        HepaTerminalStatus::Completed => "completed",
        HepaTerminalStatus::Blocked => "blocked",
        HepaTerminalStatus::Failed => "failed",
        HepaTerminalStatus::Cancelled => "cancelled",
    }
}

fn validation_status_label(status: &HepaValidationStatus) -> &'static str {
    match status {
        HepaValidationStatus::Passed => "passed",
        HepaValidationStatus::Failed => "failed",
        HepaValidationStatus::Skipped => "skipped",
        HepaValidationStatus::NoTestsDetected => "no tests detected",
    }
}

fn review_status_label(status: &HepaReviewStatus) -> &'static str {
    match status {
        HepaReviewStatus::Approved => "approved",
        HepaReviewStatus::ChangesRequested => "changes requested",
        HepaReviewStatus::Blocked => "blocked",
        HepaReviewStatus::Failed => "failed",
    }
}

fn severity_label(severity: &HepaFindingSeverity) -> &'static str {
    match severity {
        HepaFindingSeverity::Low => "low",
        HepaFindingSeverity::Medium => "medium",
        HepaFindingSeverity::High => "high",
        HepaFindingSeverity::Critical => "critical",
    }
}

fn risk_label(input: &HepaPrBodyInput) -> &'static str {
    match input.task_spec.risk_level {
        HepaRiskLevel::Low => "low",
        HepaRiskLevel::Medium => "medium",
        HepaRiskLevel::High => "high",
    }
}

fn adapter_phase_lines(report: &HepaTerminalTaskReport) -> Vec<String> {
    let Some(timing) = &report.timing else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut lines = Vec::new();
    for phase in &timing.phases {
        if let Some(adapter_id) = &phase.adapter_id {
            let role = phase.role.as_ref().map(role_label).unwrap_or("unspecified");
            let key = format!("{role}:{adapter_id}");
            if seen.insert(key.clone()) {
                lines.push(format!("- {role} adapter: {adapter_id}"));
            }
        }
    }
    lines
}

fn role_label(role: &HepaAgentRole) -> &'static str {
    match role {
        HepaAgentRole::Manager => "manager",
        HepaAgentRole::Worker => "worker",
        HepaAgentRole::Reviewer => "reviewer",
    }
}

fn sandbox_postures(report: &HepaTerminalTaskReport) -> Vec<String> {
    let Some(timing) = &report.timing else {
        return Vec::new();
    };
    let mut postures: Vec<String> = timing
        .phases
        .iter()
        .filter_map(|phase| phase.sandbox_posture.clone())
        .collect();
    postures.sort();
    postures.dedup();
    postures
}

#[derive(Debug)]
pub struct HepaPrError {
    pub field: String,
    pub message: String,
}

impl HepaPrError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaPrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaPrError {}

impl From<io::Error> for HepaPrError {
    fn from(error: io::Error) -> Self {
        Self::new("io", error.to_string())
    }
}

fn require_single_line(field: &str, value: &str) -> Result<(), HepaPrError> {
    if value.trim().is_empty() {
        return Err(HepaPrError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaPrError::new(field, "must be a single line"));
    }
    Ok(())
}

fn require_manager_branch(branch: &str) -> Result<(), HepaPrError> {
    require_single_line("head_branch", branch)?;
    if !branch.starts_with("hepa/manager/") {
        return Err(HepaPrError::new(
            "head_branch",
            "lifecycle only operates on HEPA manager-owned branches",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hepa_core::contracts::{
        CONTRACT_SCHEMA_VERSION, HepaLaneState, HepaPhaseStatus, HepaReviewFinding,
        HepaReviewSignal, HepaTimingCounters, HepaTimingPhase, HepaTimingRecord,
        HepaValidationCommandResult, HepaValidationSummary,
    };
    use std::{
        cell::RefCell,
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[derive(Default)]
    struct FakeRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
        stdout: String,
        status: i32,
    }

    impl FakeRunner {
        fn ok(stdout: &str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                stdout: stdout.to_string(),
                status: 0,
            }
        }
    }

    impl HepaProcessRunner for FakeRunner {
        fn run(
            &self,
            program: &str,
            args: &[String],
            _cwd: &Path,
        ) -> Result<HepaProcessOutput, HepaPrError> {
            self.calls
                .borrow_mut()
                .push((program.to_string(), args.to_vec()));
            Ok(HepaProcessOutput {
                status: self.status,
                stdout: self.stdout.clone(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn manager_commits_staged_changes_and_reports_sha() {
        let repo = unique_test_dir("commit");
        init_repo(&repo);
        fs::write(repo.join("change.txt"), "content\n").expect("change write");
        git(&repo, ["add", "change.txt"]);
        let lifecycle = HepaManagerGitLifecycle::manager(&repo);

        let outcome = lifecycle
            .commit_staged(
                &HepaCommitMessage::new("feat: change").with_body(vec!["Body line.".to_string()]),
            )
            .expect("manager commit should succeed");

        assert_eq!(outcome.commit_sha, git_output(&repo, ["rev-parse", "HEAD"]));
        let body = git_output(&repo, ["log", "-1", "--pretty=%b"]);
        assert!(body.contains("Body line."));

        remove_test_dir(repo);
    }

    #[test]
    fn manager_commit_preserves_configured_git_identity() {
        let repo = unique_test_dir("commit-identity");
        init_repo(&repo);
        git(
            &repo,
            [
                "config",
                "user.email",
                "14350830+kaybarax@users.noreply.github.com",
            ],
        );
        git(&repo, ["config", "user.name", "kaybarax"]);
        fs::write(repo.join("change.txt"), "content\n").expect("change write");
        git(&repo, ["add", "change.txt"]);
        let lifecycle = HepaManagerGitLifecycle::manager(&repo);

        lifecycle
            .commit_staged(&HepaCommitMessage::new("Implement readiness checks"))
            .expect("manager commit should succeed");

        let author = git_output(&repo, ["log", "-1", "--pretty=%an <%ae>"]);
        let committer = git_output(&repo, ["log", "-1", "--pretty=%cn <%ce>"]);
        assert_eq!(
            author,
            "kaybarax <14350830+kaybarax@users.noreply.github.com>"
        );
        assert_eq!(
            committer,
            "kaybarax <14350830+kaybarax@users.noreply.github.com>"
        );

        remove_test_dir(repo);
    }

    #[test]
    fn manager_commit_bypasses_repo_hooks_after_hepa_validation() {
        let repo = unique_test_dir("commit-no-verify");
        init_repo(&repo);
        let hooks_dir = repo.join(".git/hooks");
        fs::create_dir_all(&hooks_dir).expect("hooks dir");
        let hook_path = hooks_dir.join("pre-commit");
        fs::write(
            &hook_path,
            "#!/usr/bin/env sh\necho project hook should not run >&2\nexit 1\n",
        )
        .expect("hook write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&hook_path)
                .expect("hook metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&hook_path, permissions).expect("hook permissions");
        }
        fs::write(repo.join("change.txt"), "content\n").expect("change write");
        git(&repo, ["add", "change.txt"]);
        let lifecycle = HepaManagerGitLifecycle::manager(&repo);

        lifecycle
            .commit_staged(&HepaCommitMessage::new("feat: validated change"))
            .expect("manager commit should not be blocked by repo-local hooks");

        remove_test_dir(repo);
    }

    #[test]
    fn manager_commit_refuses_empty_staging() {
        let repo = unique_test_dir("commit-empty");
        init_repo(&repo);
        let lifecycle = HepaManagerGitLifecycle::manager(&repo);

        let error = lifecycle
            .commit_staged(&HepaCommitMessage::new("feat: nothing"))
            .expect_err("empty commit must be refused");

        assert_eq!(error.field, "staged");

        remove_test_dir(repo);
    }

    #[test]
    fn worker_and_reviewer_roles_cannot_commit_push_or_open_prs() {
        let repo = unique_test_dir("role-boundary");
        init_repo(&repo);
        let runner = FakeRunner::ok("https://example.invalid/pr/1");

        for role in [HepaGitRole::Worker, HepaGitRole::Reviewer] {
            let lifecycle = HepaManagerGitLifecycle::for_role(&repo, role);

            let commit = lifecycle
                .commit_staged(&HepaCommitMessage::new("feat: x"))
                .expect_err("non-manager commit must be refused");
            assert_eq!(commit.field, "role");

            let push = lifecycle
                .push_branch("origin", "hepa/manager/lane-a", &runner)
                .expect_err("non-manager push must be refused");
            assert_eq!(push.field, "role");

            let pr = lifecycle
                .create_pr(
                    &HepaPrRequest {
                        title: "feat: x".to_string(),
                        body: "body".to_string(),
                        base_branch: "main".to_string(),
                        head_branch: "hepa/manager/lane-a".to_string(),
                    },
                    &runner,
                )
                .expect_err("non-manager PR must be refused");
            assert_eq!(pr.field, "role");
        }
        // No lifecycle command reached the runner for non-manager roles.
        assert!(runner.calls.borrow().is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn manager_creates_pr_through_injected_runner() {
        let repo = unique_test_dir("pr-create");
        init_repo(&repo);
        let runner = FakeRunner::ok("https://example.invalid/org/repo/pull/7");
        let lifecycle = HepaManagerGitLifecycle::manager(&repo);

        let handle = lifecycle
            .create_pr(
                &HepaPrRequest {
                    title: "feat: change".to_string(),
                    body: "## Summary\nDid the thing.".to_string(),
                    base_branch: "main".to_string(),
                    head_branch: "hepa/manager/lane-a".to_string(),
                },
                &runner,
            )
            .expect("manager PR creation should succeed");

        assert_eq!(handle.url, "https://example.invalid/org/repo/pull/7");
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "gh");
        assert!(calls[0].1.iter().any(|arg| arg == "create"));
        assert!(calls[0].1.iter().any(|arg| arg == "--head"));

        remove_test_dir(repo);
    }

    #[test]
    fn pr_creation_refuses_local_base_ahead_of_remote_base() {
        let repo = unique_test_dir("pr-base-ahead");
        init_repo(&repo);
        git(&repo, ["branch", "-M", "main"]);
        let initial = git_output(&repo, ["rev-parse", "HEAD"]);
        git(&repo, ["update-ref", "refs/remotes/origin/main", &initial]);
        fs::write(repo.join("local-only.txt"), "local base commit\n").expect("local write");
        git(&repo, ["add", "local-only.txt"]);
        git(&repo, ["commit", "-m", "local base only"]);
        git(&repo, ["checkout", "-b", "hepa/manager/lane-a"]);

        let runner = FakeRunner::ok("https://example.invalid/org/repo/pull/7");
        let lifecycle = HepaManagerGitLifecycle::manager(&repo);

        let error = lifecycle
            .create_pr(
                &HepaPrRequest {
                    title: "feat: change".to_string(),
                    body: "## Summary\nDid the thing.".to_string(),
                    base_branch: "main".to_string(),
                    head_branch: "hepa/manager/lane-a".to_string(),
                },
                &runner,
            )
            .expect_err("polluted local base must be refused");

        assert_eq!(error.field, "base_branch");
        assert!(error.message.contains("ahead of `origin/main`"));
        assert!(runner.calls.borrow().is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn pr_creation_requires_manager_owned_head_branch() {
        let repo = unique_test_dir("pr-branch");
        init_repo(&repo);
        let runner = FakeRunner::ok("https://example.invalid/pr/1");
        let lifecycle = HepaManagerGitLifecycle::manager(&repo);

        let error = lifecycle
            .create_pr(
                &HepaPrRequest {
                    title: "feat: change".to_string(),
                    body: "body".to_string(),
                    base_branch: "main".to_string(),
                    head_branch: "feature/not-manager".to_string(),
                },
                &runner,
            )
            .expect_err("non-manager head branch must be refused");

        assert_eq!(error.field, "head_branch");
        assert!(runner.calls.borrow().is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn hermes_pr_intent_builds_manager_owned_pr_request() {
        let intent = HepaHermesPrIntent {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            author_profile_id: "hepa-manager".to_string(),
            title: "Add starter template readiness badge".to_string(),
            body: "## Summary\nAdds the readiness badge requested by the app task.\n\n## Task\nAcceptance criteria:\n- Show readiness clearly in the README.\n\n## Changes\n- Updated the app README badge area.\n\n## Validation\n- yarn test:e2e passed\n\n## Review\n- Reviewer approved the scoped change.\n\n## Risk\n- Low risk; documentation-only change.\n".to_string(),
            audit_summary: vec![
                "HEPA checked safe staging before PR creation.".to_string(),
                "Human review remains required before merge.".to_string(),
            ],
            human_review_required: true,
        };

        let request = pr_request_from_hermes_intent(&intent, "main", "hepa/manager/lane-1")
            .expect("valid Hermes intent should build a PR request");

        assert_eq!(request.title, intent.title);
        assert_eq!(request.base_branch, "main");
        assert_eq!(request.head_branch, "hepa/manager/lane-1");
        assert!(request.body.contains("Adds the readiness badge"));
        assert_eq!(request.body, intent.body.trim_end());
        assert!(!request.body.contains("## HEPA audit"));
        assert!(!request.body.contains("Fallback Evidence Artifact"));
        assert!(
            !request
                .body
                .contains("not Hermes-authored project PR intent")
        );
        assert!(
            !request
                .body
                .contains("HEPA checked safe staging before PR creation.")
        );
        assert!(!request.body.contains("PR intent author: hepa-manager"));
    }

    #[test]
    fn hermes_pr_intent_rejects_non_manager_author_before_pr_request() {
        let intent = HepaHermesPrIntent {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            author_profile_id: "hepa-reviewer".to_string(),
            title: "Add starter template readiness badge".to_string(),
            body: "## Summary\nAdds the readiness badge requested by the app task.\n\n## Task\nAcceptance criteria:\n- Show readiness clearly in the README.\n\n## Changes\n- Updated the app README badge area.\n\n## Validation\n- yarn test:e2e passed\n\n## Review\n- Reviewer approved the scoped change.\n\n## Risk\n- Low risk; documentation-only change.\n".to_string(),
            audit_summary: vec!["HEPA checked safe staging before PR creation.".to_string()],
            human_review_required: true,
        };

        let error = pr_request_from_hermes_intent(&intent, "main", "hepa/manager/lane-1")
            .expect_err("non-manager PR intent must be rejected");

        assert_eq!(error.field, "author_profile_id");
    }

    #[test]
    fn hermes_pr_intent_rejects_generic_hepa_template_before_pr_request() {
        let intent = HepaHermesPrIntent {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            author_profile_id: "hepa-manager".to_string(),
            title: "HEPA validation: Update app".to_string(),
            body: "## Summary\n- Task: HEPA validation: update app\n\n## Changes\n- changed files\n\n## Validation\n- passed\n\n## Review\n- approved\n\n## Risk\n- low\n"
                .to_string(),
            audit_summary: vec!["validation passed".to_string()],
            human_review_required: true,
        };

        let error = pr_request_from_hermes_intent(&intent, "main", "hepa/manager/lane-1")
            .expect_err("generic HEPA PR template must be rejected");

        assert_eq!(error.field, "body");
    }

    #[test]
    fn pr_body_reconstructs_the_run_honestly() {
        let task_spec = HepaTaskSpec {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            goal: "Fix the login redirect".to_string(),
            non_goals: Vec::new(),
            expected_areas: vec!["src/login.rs".to_string()],
            acceptance_criteria: vec!["redirect works".to_string()],
            validation_commands: vec!["cargo test".to_string()],
            dependencies: Vec::new(),
            target_branch: Some("main".to_string()),
            risk_level: HepaRiskLevel::Medium,
            max_total_rounds: 2,
            created_at: "2026-06-16T00:00:00Z".to_string(),
        };
        let report = HepaTerminalTaskReport {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            status: HepaTerminalStatus::Blocked,
            pr_url: None,
            validation: Some(HepaValidationSummary {
                schema_version: CONTRACT_SCHEMA_VERSION,
                status: HepaValidationStatus::Failed,
                commands: vec![HepaValidationCommandResult {
                    command: "cargo test".to_string(),
                    exit_code: 101,
                    duration_ms: 1200,
                }],
                no_tests_detected: false,
                failure_type: Some("test_failure".to_string()),
                summary: vec!["1 test failed".to_string()],
            }),
            review_signals: vec![HepaReviewSignal {
                schema_version: CONTRACT_SCHEMA_VERSION,
                review_id: "review-1".to_string(),
                lane_id: "lane-1".to_string(),
                adapter_id: "reviewer-fake".to_string(),
                status: HepaReviewStatus::ChangesRequested,
                findings: vec![HepaReviewFinding {
                    finding_id: "finding-1".to_string(),
                    severity: HepaFindingSeverity::High,
                    category: "correctness".to_string(),
                    evidence: "evidence".to_string(),
                    in_scope: true,
                    release_risk: true,
                    recommended_action: "fix it".to_string(),
                    file_ref: Some("src/login.rs".to_string()),
                    line: Some(10),
                    message: "Redirect loops on empty session".to_string(),
                    accepted: false,
                }],
                summary: vec!["changes requested".to_string()],
                completed_at: "2026-06-16T00:00:05Z".to_string(),
            }],
            arbitration: None,
            timing: Some(HepaTimingRecord {
                schema_version: CONTRACT_SCHEMA_VERSION,
                run_id: "run-1".to_string(),
                phases: vec![
                    HepaTimingPhase {
                        name: "worker_attempt".to_string(),
                        status: HepaPhaseStatus::Completed,
                        duration_seconds: 4.0,
                        round: Some(1),
                        role: Some(HepaAgentRole::Worker),
                        adapter_id: Some("worker-fake".to_string()),
                        routing_reason: Some("default".to_string()),
                        sandbox_posture: Some("host-worktree".to_string()),
                    },
                    HepaTimingPhase {
                        name: "review".to_string(),
                        status: HepaPhaseStatus::Completed,
                        duration_seconds: 2.0,
                        round: Some(1),
                        role: Some(HepaAgentRole::Reviewer),
                        adapter_id: Some("reviewer-fake".to_string()),
                        routing_reason: Some("fanout".to_string()),
                        sandbox_posture: Some("host-worktree".to_string()),
                    },
                ],
                counters: HepaTimingCounters {
                    agent_loops: 1,
                    manager_passes: 2,
                    worker_profile_llm_calls: 0,
                    reviewer_passes: 1,
                    install_events: 0,
                    container_count: 0,
                },
            }),
            summary: vec!["Blocked by failing validation.".to_string()],
            human_attention_required: true,
            completed_at: "2026-06-16T00:00:07Z".to_string(),
        };
        let lane = HepaLane {
            schema_version: CONTRACT_SCHEMA_VERSION,
            lane_id: "lane-1".to_string(),
            project_id: "project-1".to_string(),
            task_id: "task-1".to_string(),
            adapter_id: "worker-fake".to_string(),
            state: HepaLaneState::Blocked,
            worktree_ref: "worktree:lane-1".to_string(),
            branch: "hepa/manager/lane-1".to_string(),
            run_dir_ref: "control:runs/run-1".to_string(),
            attempt_count: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:07Z".to_string(),
            completed_at: None,
        };

        let body = build_pr_body(&HepaPrBodyInput {
            task_spec: &task_spec,
            terminal_report: &report,
            lane: &lane,
            external_card_id: Some("hermes-card-1"),
        });

        for section in [
            "## Fallback Evidence Artifact",
            "## Summary",
            "## Validation",
            "## Review",
            "## Risk",
            "## Adapter",
            "## Timing",
            "## Hermes card",
        ] {
            assert!(body.contains(section), "missing section {section}");
        }
        // Honest reconstruction: failed validation, the real finding, declared
        // risk, adapters, manager passes, and the card link all appear.
        assert!(body.contains("not Hermes-authored project PR intent"));
        assert!(body.contains("HepaHermesPrIntent"));
        assert!(body.contains("Status: blocked"));
        assert!(body.contains("Result: failed"));
        assert!(body.contains("`cargo test` exited 101"));
        assert!(body.contains("Redirect loops on empty session"));
        assert!(body.contains("Declared risk: medium"));
        assert!(body.contains("worker adapter: worker-fake"));
        assert!(body.contains("reviewer adapter: reviewer-fake"));
        assert!(body.contains("Sandbox posture: host-worktree"));
        assert!(body.contains("manager passes: 2"));
        assert!(body.contains("Card: hermes-card-1"));
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

    fn git_output<const N: usize>(repo: &Path, args: [&str; N]) -> String {
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
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-git-pr-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
