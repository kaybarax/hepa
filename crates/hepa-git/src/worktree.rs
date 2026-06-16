use crate::branches::{HepaBranchError, HepaManagerBranch};
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
        let lane_id = lane_id.into();
        let branch = HepaManagerBranch::for_lane(&lane_id)?;
        let worktree_path = self.worktree_root.join(&lane_id);
        if worktree_path.exists() {
            return Err(HepaWorktreeError::new(
                "worktree_path",
                "lane worktree already exists",
            ));
        }

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

        Ok(HepaWorktreeAllocation {
            lane_id,
            branch: branch.as_str().to_string(),
            worktree_path,
            base_commit,
        })
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaWorktreeAllocation {
    pub lane_id: String,
    pub branch: String,
    pub worktree_path: PathBuf,
    pub base_commit: String,
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

impl From<HepaBranchError> for HepaWorktreeError {
    fn from(error: HepaBranchError) -> Self {
        Self::new(error.field, error.message)
    }
}

fn path_to_str<'a>(field: &str, path: &'a Path) -> Result<&'a str, HepaWorktreeError> {
    path.to_str()
        .ok_or_else(|| HepaWorktreeError::new(field, "must be UTF-8"))
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
        std::env::temp_dir().join(format!("hepa-git-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
