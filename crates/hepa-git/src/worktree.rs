use crate::branches::{HepaBranchError, HepaManagerBranch};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaWorktreeAllocator {
    repo_path: PathBuf,
    worktree_root: PathBuf,
}

impl HepaWorktreeAllocator {
    pub fn new(repo_path: impl Into<PathBuf>, worktree_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_path: repo_path.into(),
            worktree_root: worktree_root.into(),
        }
    }

    pub fn allocate_lane(
        &self,
        lane_id: impl Into<String>,
    ) -> Result<HepaWorktreeAllocation, HepaWorktreeError> {
        self.allocate_lane_with_metadata(lane_id, "unspecified")
    }

    pub fn allocate_lane_with_metadata(
        &self,
        lane_id: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Result<HepaWorktreeAllocation, HepaWorktreeError> {
        let lane_id = lane_id.into();
        let created_at = created_at.into();
        require_single_line("created_at", &created_at)?;
        let branch = HepaManagerBranch::for_lane(&lane_id)?;
        let worktree_path = self.worktree_root.join(&lane_id);
        if worktree_path.exists() {
            return Err(HepaWorktreeError::new(
                "worktree_path",
                "lane worktree already exists",
            ));
        }

        self.require_clean_tree()?;
        let base_commit = self.git_output(["rev-parse", "HEAD"])?;
        fs::create_dir_all(&self.worktree_root)?;
        self.git_output([
            "worktree",
            "add",
            "-b",
            branch.as_str(),
            path_to_str("worktree_path", &worktree_path)?,
            &base_commit,
        ])?;

        let mut allocation = HepaWorktreeAllocation {
            lane_id,
            branch: branch.as_str().to_string(),
            worktree_path,
            base_commit,
            metadata_path: PathBuf::new(),
        };
        allocation.metadata_path = allocation.write_metadata(created_at)?;
        Ok(allocation)
    }

    pub fn cleanup_lane(
        &self,
        lane_id: impl Into<String>,
        cleaned_at: impl Into<String>,
    ) -> Result<HepaWorktreeCleanupReport, HepaWorktreeError> {
        let lane_id = lane_id.into();
        let cleaned_at = cleaned_at.into();
        require_single_line("cleaned_at", &cleaned_at)?;
        let worktree_path = self.worktree_root.join(&lane_id);
        let metadata_path = worktree_path.join(WORKTREE_METADATA_FILE);
        let mut metadata = read_metadata(&metadata_path)?;
        require_lane_metadata("lane_id", &lane_id, &metadata)?;
        require_manager_branch(&metadata.branch)?;

        if !worktree_is_clean_except_metadata(&worktree_path)? {
            return Ok(HepaWorktreeCleanupReport {
                lane_id,
                branch: metadata.branch,
                worktree_path,
                status: HepaWorktreeCleanupStatus::PreservedDirty,
            });
        }

        metadata.cleanup.status = HepaWorktreeCleanupStatus::Cleaned;
        metadata.cleanup.cleaned_at = Some(cleaned_at);
        write_stable_json(&metadata_path, &metadata)?;

        self.git_output([
            "worktree",
            "remove",
            "--force",
            path_to_str("worktree_path", &worktree_path)?,
        ])?;
        self.git_output(["branch", "-D", &metadata.branch])?;

        Ok(HepaWorktreeCleanupReport {
            lane_id,
            branch: metadata.branch,
            worktree_path,
            status: HepaWorktreeCleanupStatus::Cleaned,
        })
    }

    pub fn mark_lane_stale(
        &self,
        lane_id: impl Into<String>,
        prune_after: impl Into<String>,
    ) -> Result<HepaWorktreeMetadata, HepaWorktreeError> {
        let lane_id = lane_id.into();
        let prune_after = prune_after.into();
        require_single_line("prune_after", &prune_after)?;
        let metadata_path = self
            .worktree_root
            .join(&lane_id)
            .join(WORKTREE_METADATA_FILE);
        let mut metadata = read_metadata(&metadata_path)?;
        require_lane_metadata("lane_id", &lane_id, &metadata)?;
        metadata.cleanup.status = HepaWorktreeCleanupStatus::Stale;
        metadata.cleanup.prune_after = Some(prune_after);
        write_stable_json(&metadata_path, &metadata)?;
        Ok(metadata)
    }

    pub fn prune_stale_leases(
        &self,
        now: impl Into<String>,
        cleaned_at: impl Into<String>,
    ) -> Result<Vec<HepaWorktreeCleanupReport>, HepaWorktreeError> {
        let now = now.into();
        let cleaned_at = cleaned_at.into();
        require_single_line("now", &now)?;
        require_single_line("cleaned_at", &cleaned_at)?;
        let mut reports = Vec::new();
        if !self.worktree_root.exists() {
            return Ok(reports);
        }
        for entry in fs::read_dir(&self.worktree_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let metadata_path = entry.path().join(WORKTREE_METADATA_FILE);
            if !metadata_path.exists() {
                continue;
            }
            let metadata = read_metadata(&metadata_path)?;
            if metadata.is_prunable_at(&now) {
                reports.push(self.cleanup_lane(metadata.lane_id, &cleaned_at)?);
            }
        }
        reports.sort_by(|left, right| left.lane_id.cmp(&right.lane_id));
        Ok(reports)
    }

    fn git_output<const N: usize>(&self, args: [&str; N]) -> Result<String, HepaWorktreeError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.repo_path)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(HepaWorktreeError::new(
                "git",
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn require_clean_tree(&self) -> Result<(), HepaWorktreeError> {
        let status = self.git_output(["status", "--porcelain=v1", "--untracked-files=all"])?;
        let dirty_user_status = status
            .lines()
            .filter(|line| !status_line_is_hepa_runtime(line))
            .collect::<Vec<_>>();
        if !dirty_user_status.is_empty() {
            return Err(HepaWorktreeError::new(
                "repo_status",
                "repository must be clean before lane launch",
            ));
        }
        Ok(())
    }
}

fn status_line_is_hepa_runtime(line: &str) -> bool {
    let path = line.get(3..).unwrap_or(line).trim();
    path == ".hepa" || path.starts_with(".hepa/")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaWorktreeAllocation {
    pub lane_id: String,
    pub branch: String,
    pub worktree_path: PathBuf,
    pub base_commit: String,
    pub metadata_path: PathBuf,
}

impl HepaWorktreeAllocation {
    pub fn metadata(&self, created_at: impl Into<String>) -> HepaWorktreeMetadata {
        HepaWorktreeMetadata {
            schema_version: WORKTREE_METADATA_SCHEMA_VERSION,
            lane_id: self.lane_id.clone(),
            branch: self.branch.clone(),
            base_commit: self.base_commit.clone(),
            worktree_ref: format!("worktree:{}", self.lane_id),
            cleanup: HepaWorktreeCleanupMetadata {
                status: HepaWorktreeCleanupStatus::Active,
                created_at: created_at.into(),
                cleaned_at: None,
                prune_after: None,
            },
        }
    }

    fn write_metadata(&self, created_at: impl Into<String>) -> Result<PathBuf, HepaWorktreeError> {
        let metadata_path = self.worktree_path.join(WORKTREE_METADATA_FILE);
        write_stable_json(&metadata_path, &self.metadata(created_at))?;
        Ok(metadata_path)
    }
}

pub const WORKTREE_METADATA_SCHEMA_VERSION: u32 = 1;
pub const WORKTREE_METADATA_FILE: &str = ".hepa-worktree.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaWorktreeMetadata {
    pub schema_version: u32,
    pub lane_id: String,
    pub branch: String,
    pub base_commit: String,
    pub worktree_ref: String,
    pub cleanup: HepaWorktreeCleanupMetadata,
}

impl HepaWorktreeMetadata {
    fn is_prunable_at(&self, now: &str) -> bool {
        matches!(self.cleanup.status, HepaWorktreeCleanupStatus::Stale)
            && self
                .cleanup
                .prune_after
                .as_deref()
                .is_some_and(|prune_after| prune_after <= now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaWorktreeCleanupMetadata {
    pub status: HepaWorktreeCleanupStatus,
    pub created_at: String,
    pub cleaned_at: Option<String>,
    pub prune_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaWorktreeCleanupStatus {
    Active,
    Cleaned,
    Stale,
    PreservedDirty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaWorktreeCleanupReport {
    pub lane_id: String,
    pub branch: String,
    pub worktree_path: PathBuf,
    pub status: HepaWorktreeCleanupStatus,
}

#[derive(Debug)]
pub struct HepaWorktreeError {
    pub field: String,
    pub message: String,
}

impl HepaWorktreeError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaWorktreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaWorktreeError {}

impl From<io::Error> for HepaWorktreeError {
    fn from(error: io::Error) -> Self {
        Self::new("io", error.to_string())
    }
}

impl From<serde_json::Error> for HepaWorktreeError {
    fn from(error: serde_json::Error) -> Self {
        Self::new("serde_json", error.to_string())
    }
}

impl From<HepaBranchError> for HepaWorktreeError {
    fn from(error: HepaBranchError) -> Self {
        Self::new(error.field, error.message)
    }
}

fn path_to_str<'a>(field: &str, path: &'a Path) -> Result<&'a str, HepaWorktreeError> {
    path.to_str()
        .ok_or_else(|| HepaWorktreeError::new(field, "must be UTF-8"))
}

fn require_single_line(field: &str, value: &str) -> Result<(), HepaWorktreeError> {
    if value.trim().is_empty() {
        return Err(HepaWorktreeError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaWorktreeError::new(field, "must be a single line"));
    }
    Ok(())
}

fn write_stable_json<T>(path: &Path, value: &T) -> Result<(), HepaWorktreeError>
where
    T: Serialize,
{
    let mut json = serde_json::to_string_pretty(value)?;
    if !json.ends_with('\n') {
        json.push('\n');
    }
    fs::write(path, json)?;
    Ok(())
}

fn read_metadata(path: &Path) -> Result<HepaWorktreeMetadata, HepaWorktreeError> {
    let metadata = serde_json::from_str(&fs::read_to_string(path)?)?;
    Ok(metadata)
}

fn require_lane_metadata(
    field: &str,
    expected_lane_id: &str,
    metadata: &HepaWorktreeMetadata,
) -> Result<(), HepaWorktreeError> {
    if metadata.schema_version != WORKTREE_METADATA_SCHEMA_VERSION {
        return Err(HepaWorktreeError::new(
            "schema_version",
            format!("must be {WORKTREE_METADATA_SCHEMA_VERSION}"),
        ));
    }
    if metadata.lane_id != expected_lane_id {
        return Err(HepaWorktreeError::new(
            field,
            "metadata lane does not match requested lane",
        ));
    }
    require_single_line("branch", &metadata.branch)?;
    require_single_line("base_commit", &metadata.base_commit)?;
    require_single_line("worktree_ref", &metadata.worktree_ref)?;
    require_single_line("cleanup.created_at", &metadata.cleanup.created_at)?;
    if let Some(cleaned_at) = &metadata.cleanup.cleaned_at {
        require_single_line("cleanup.cleaned_at", cleaned_at)?;
    }
    if let Some(prune_after) = &metadata.cleanup.prune_after {
        require_single_line("cleanup.prune_after", prune_after)?;
    }
    Ok(())
}

fn require_manager_branch(branch: &str) -> Result<(), HepaWorktreeError> {
    if !branch.starts_with("hepa/manager/") {
        return Err(HepaWorktreeError::new(
            "branch",
            "cleanup only manages HEPA manager-owned branches",
        ));
    }
    Ok(())
}

fn worktree_is_clean_except_metadata(worktree_path: &Path) -> Result<bool, HepaWorktreeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree_path)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output()?;
    if !output.status.success() {
        return Err(HepaWorktreeError::new(
            "git",
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let status = String::from_utf8_lossy(&output.stdout);
    Ok(status.lines().all(|line| {
        line.strip_prefix("?? ")
            .is_some_and(|path| path == WORKTREE_METADATA_FILE)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn allocate_lane_creates_manager_owned_branch_and_worktree() {
        let root = unique_test_dir("allocate");
        let repo = root.join("repo");
        let worktrees = root.join("worktrees");
        init_repo(&repo);

        let allocator = HepaWorktreeAllocator::new(&repo, &worktrees);
        let allocation = allocator
            .allocate_lane("lane-a")
            .expect("lane allocation should succeed");
        let branch = git_output(&allocation.worktree_path, ["branch", "--show-current"]);

        assert_eq!(allocation.lane_id, "lane-a");
        assert_eq!(allocation.branch, "hepa/manager/lane-a");
        assert_eq!(branch, "hepa/manager/lane-a");
        assert!(allocation.worktree_path.join("README.md").exists());
        assert_eq!(
            allocation.base_commit,
            git_output(&repo, ["rev-parse", "HEAD"])
        );
        assert!(allocation.metadata_path.exists());

        remove_test_dir(root);
    }

    #[test]
    fn allocate_lane_rejects_existing_lane_worktree() {
        let root = unique_test_dir("duplicate");
        let repo = root.join("repo");
        let worktrees = root.join("worktrees");
        init_repo(&repo);
        let allocator = HepaWorktreeAllocator::new(&repo, &worktrees);

        allocator
            .allocate_lane("lane-a")
            .expect("first allocation should succeed");
        let duplicate = allocator.allocate_lane("lane-a");

        assert!(duplicate.is_err());

        remove_test_dir(root);
    }

    #[test]
    fn allocate_lane_produces_unique_lane_branch_and_worktree_refs() {
        let root = unique_test_dir("unique");
        let repo = root.join("repo");
        let worktrees = root.join("worktrees");
        init_repo(&repo);
        let allocator = HepaWorktreeAllocator::new(&repo, &worktrees);

        let first = allocator
            .allocate_lane("lane-a")
            .expect("first allocation should succeed");
        let second = allocator
            .allocate_lane("lane-b")
            .expect("second allocation should succeed");

        assert_ne!(first.lane_id, second.lane_id);
        assert_ne!(first.branch, second.branch);
        assert_ne!(first.worktree_path, second.worktree_path);
        assert!(branch_exists(&repo, "hepa/manager/lane-a"));
        assert!(branch_exists(&repo, "hepa/manager/lane-b"));

        remove_test_dir(root);
    }

    #[test]
    fn allocate_lane_requires_clean_source_tree_and_preserves_changes() {
        let root = unique_test_dir("dirty");
        let repo = root.join("repo");
        let worktrees = root.join("worktrees");
        init_repo(&repo);
        fs::write(repo.join("notes.md"), "do not remove\n").expect("dirty file write");
        let allocator = HepaWorktreeAllocator::new(&repo, &worktrees);

        let allocation = allocator.allocate_lane("lane-a");

        assert!(matches!(
            allocation,
            Err(HepaWorktreeError {
                field,
                ..
            }) if field == "repo_status"
        ));
        assert_eq!(
            fs::read_to_string(repo.join("notes.md")).expect("dirty file remains"),
            "do not remove\n"
        );
        assert!(!worktrees.join("lane-a").exists());

        remove_test_dir(root);
    }

    #[test]
    fn allocate_lane_ignores_hepa_runtime_artifacts_in_source_tree() {
        let root = unique_test_dir("hepa-runtime-clean");
        let repo = root.join("repo");
        let worktrees = root.join("worktrees");
        init_repo(&repo);
        let runtime_dir = repo.join(".hepa/control/hermes-run-brief/run-1/lane-1");
        fs::create_dir_all(&runtime_dir).expect("runtime dir");
        fs::write(runtime_dir.join("context.json"), "{}\n").expect("runtime artifact");
        let allocator = HepaWorktreeAllocator::new(&repo, &worktrees);

        let allocation = allocator
            .allocate_lane("lane-a")
            .expect("HEPA runtime artifacts should not dirty the source tree");

        assert!(allocation.worktree_path.exists());
        remove_test_dir(root);
    }

    #[test]
    fn allocate_lane_records_branch_worktree_base_and_cleanup_metadata() {
        let root = unique_test_dir("metadata");
        let repo = root.join("repo");
        let worktrees = root.join("worktrees");
        init_repo(&repo);
        let allocator = HepaWorktreeAllocator::new(&repo, &worktrees);

        let allocation = allocator
            .allocate_lane_with_metadata("lane-a", "2026-06-16T00:00:00Z")
            .expect("allocation should succeed");
        let metadata_json =
            fs::read_to_string(&allocation.metadata_path).expect("metadata should exist");
        let metadata: HepaWorktreeMetadata =
            serde_json::from_str(&metadata_json).expect("metadata should parse");

        assert_eq!(metadata.lane_id, "lane-a");
        assert_eq!(metadata.branch, "hepa/manager/lane-a");
        assert_eq!(metadata.base_commit, allocation.base_commit);
        assert_eq!(metadata.worktree_ref, "worktree:lane-a");
        assert_eq!(metadata.cleanup.status, HepaWorktreeCleanupStatus::Active);
        assert_eq!(metadata.cleanup.created_at, "2026-06-16T00:00:00Z");
        assert!(!metadata_json.contains(root.to_string_lossy().as_ref()));

        remove_test_dir(root);
    }

    #[test]
    fn cleanup_lane_removes_worktree_and_manager_branch() {
        let root = unique_test_dir("cleanup");
        let repo = root.join("repo");
        let worktrees = root.join("worktrees");
        init_repo(&repo);
        let allocator = HepaWorktreeAllocator::new(&repo, &worktrees);
        let allocation = allocator
            .allocate_lane_with_metadata("lane-a", "2026-06-16T00:00:00Z")
            .expect("allocation should succeed");

        let report = allocator
            .cleanup_lane("lane-a", "2026-06-16T00:00:01Z")
            .expect("cleanup should succeed");

        assert_eq!(report.status, HepaWorktreeCleanupStatus::Cleaned);
        assert!(!allocation.worktree_path.exists());
        assert!(!branch_exists(&repo, "hepa/manager/lane-a"));

        remove_test_dir(root);
    }

    #[test]
    fn cleanup_lane_preserves_dirty_worktree_changes() {
        let root = unique_test_dir("cleanup-dirty");
        let repo = root.join("repo");
        let worktrees = root.join("worktrees");
        init_repo(&repo);
        let allocator = HepaWorktreeAllocator::new(&repo, &worktrees);
        let allocation = allocator
            .allocate_lane_with_metadata("lane-a", "2026-06-16T00:00:00Z")
            .expect("allocation should succeed");
        fs::write(allocation.worktree_path.join("notes.md"), "human change\n")
            .expect("dirty worktree file write");

        let report = allocator
            .cleanup_lane("lane-a", "2026-06-16T00:00:01Z")
            .expect("cleanup should inspect dirty worktree");

        assert_eq!(report.status, HepaWorktreeCleanupStatus::PreservedDirty);
        assert!(allocation.worktree_path.exists());
        assert!(branch_exists(&repo, "hepa/manager/lane-a"));
        assert_eq!(
            fs::read_to_string(allocation.worktree_path.join("notes.md"))
                .expect("dirty worktree file remains"),
            "human change\n"
        );

        remove_test_dir(root);
    }

    #[test]
    fn stale_lease_pruning_removes_due_clean_worktrees_only() {
        let root = unique_test_dir("prune");
        let repo = root.join("repo");
        let worktrees = root.join("worktrees");
        init_repo(&repo);
        let allocator = HepaWorktreeAllocator::new(&repo, &worktrees);
        let due = allocator
            .allocate_lane_with_metadata("lane-due", "2026-06-16T00:00:00Z")
            .expect("due allocation should succeed");
        let future = allocator
            .allocate_lane_with_metadata("lane-future", "2026-06-16T00:00:00Z")
            .expect("future allocation should succeed");
        let dirty = allocator
            .allocate_lane_with_metadata("lane-dirty", "2026-06-16T00:00:00Z")
            .expect("dirty allocation should succeed");
        fs::write(dirty.worktree_path.join("notes.md"), "preserve me\n")
            .expect("dirty worktree file write");
        allocator
            .mark_lane_stale("lane-due", "2026-06-16T00:00:01Z")
            .expect("mark due stale");
        allocator
            .mark_lane_stale("lane-future", "2026-06-16T00:01:00Z")
            .expect("mark future stale");
        allocator
            .mark_lane_stale("lane-dirty", "2026-06-16T00:00:01Z")
            .expect("mark dirty stale");

        let reports = allocator
            .prune_stale_leases("2026-06-16T00:00:30Z", "2026-06-16T00:00:31Z")
            .expect("prune should succeed");

        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].lane_id, "lane-dirty");
        assert_eq!(reports[0].status, HepaWorktreeCleanupStatus::PreservedDirty);
        assert_eq!(reports[1].lane_id, "lane-due");
        assert_eq!(reports[1].status, HepaWorktreeCleanupStatus::Cleaned);
        assert!(!due.worktree_path.exists());
        assert!(future.worktree_path.exists());
        assert!(dirty.worktree_path.exists());
        assert!(!branch_exists(&repo, "hepa/manager/lane-due"));
        assert!(branch_exists(&repo, "hepa/manager/lane-future"));
        assert!(branch_exists(&repo, "hepa/manager/lane-dirty"));

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

    fn branch_exists(repo: &Path, branch: &str) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--verify", branch])
            .output()
            .expect("git should run")
            .status
            .success()
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-git-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
