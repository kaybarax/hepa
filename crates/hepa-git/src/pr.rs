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
