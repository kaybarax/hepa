use std::{error::Error, fmt, io, path::PathBuf, process::Command};

/// Manager-owned safe staging.
///
/// HEPA only ever stages an explicit, manager-approved list of changed files.
/// There is deliberately no API that stages "everything"; blind staging such as
/// `git add .` or `git add -A` cannot be expressed through this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaSafeStaging {
    repo_root: PathBuf,
}

impl HepaSafeStaging {
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
        }
    }

    /// Stage exactly the approved files, one explicit `git add -- <path>` per
    /// entry. Any other changed files in the worktree are left unstaged.
    pub fn stage_approved_files(
        &self,
        approved_files: &[String],
    ) -> Result<HepaStagingReport, HepaStagingError> {
        if approved_files.is_empty() {
            return Err(HepaStagingError::new(
                "approved_files",
                "safe staging requires at least one approved file",
            ));
        }

        for path in approved_files {
            require_single_line("approved_files", path)?;
        }

        for path in approved_files {
            self.git_status(["add", "--", path])?;
        }

        Ok(HepaStagingReport {
            staged_files: self.staged_paths()?,
        })
    }

    fn staged_paths(&self) -> Result<Vec<String>, HepaStagingError> {
        let output = self.git_output(["diff", "--cached", "--name-only"])?;
        let mut staged: Vec<String> = output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        staged.sort();
        staged.dedup();
        Ok(staged)
    }

    fn git_status<const N: usize>(&self, args: [&str; N]) -> Result<(), HepaStagingError> {
        self.git_output(args).map(|_| ())
    }

    fn git_output<const N: usize>(&self, args: [&str; N]) -> Result<String, HepaStagingError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(HepaStagingError::new(
                "git",
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaStagingReport {
    pub staged_files: Vec<String>,
}

#[derive(Debug)]
pub struct HepaStagingError {
    pub field: String,
    pub message: String,
}

impl HepaStagingError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaStagingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaStagingError {}

impl From<io::Error> for HepaStagingError {
    fn from(error: io::Error) -> Self {
        Self::new("io", error.to_string())
    }
}

fn require_single_line(field: &str, value: &str) -> Result<(), HepaStagingError> {
    if value.trim().is_empty() {
        return Err(HepaStagingError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaStagingError::new(field, "must be a single line"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn stage_approved_files_stages_only_the_listed_files() {
        let repo = unique_test_dir("stage-approved");
        init_repo(&repo);
        fs::write(repo.join("approved.txt"), "approved change\n").expect("approved write");
        fs::write(repo.join("other.txt"), "unrelated change\n").expect("other write");

        let staging = HepaSafeStaging::new(&repo);
        let report = staging
            .stage_approved_files(&["approved.txt".to_string()])
            .expect("approved staging should succeed");

        assert_eq!(report.staged_files, vec!["approved.txt".to_string()]);
        assert_eq!(staged_names(&repo), vec!["approved.txt".to_string()]);

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_rejects_empty_and_multiline_paths() {
        let repo = unique_test_dir("stage-empty");
        init_repo(&repo);
        let staging = HepaSafeStaging::new(&repo);

        let empty = staging
            .stage_approved_files(&[])
            .expect_err("empty list must be rejected");
        assert_eq!(empty.field, "approved_files");

        let multiline = staging
            .stage_approved_files(&["a.txt\nb.txt".to_string()])
            .expect_err("multiline path must be rejected");
        assert_eq!(multiline.field, "approved_files");

        remove_test_dir(repo);
    }

    fn staged_names(repo: &Path) -> Vec<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["diff", "--cached", "--name-only"])
            .output()
            .expect("git diff should run");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
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
