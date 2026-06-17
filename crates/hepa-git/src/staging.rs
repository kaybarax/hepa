use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
    process::Command,
};

/// Manager-owned safe staging.
///
/// HEPA only ever stages an explicit, manager-approved list of changed files.
/// There is deliberately no API that stages "everything"; blind staging such as
/// `git add .` or `git add -A` cannot be expressed through this type, and every
/// approved entry is screened for blind-staging markers and secret-like paths
/// before a single `git add` runs.
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

        // Screen every path before any `git add` runs so a single unsafe entry
        // never leaves part of the list staged.
        let mut rejections = Vec::new();
        for path in approved_files {
            if let Some(reason) = classify_staging_path(path) {
                rejections.push(HepaStagingRejection {
                    path: path.clone(),
                    reason,
                });
            }
        }
        if !rejections.is_empty() {
            rejections.sort_by(|left, right| left.path.cmp(&right.path));
            return Err(HepaStagingError::rejected(rejections));
        }

        for path in approved_files {
            // `:(literal)` disables pathspec glob/magic so the explicit path is
            // matched verbatim and never expands to additional files.
            self.git_status(["add", "--", &literal_pathspec(path)])?;
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

/// Why a single approved-file entry was refused before staging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HepaStagingRejectionReason {
    /// Blind/global staging marker (`.`, `*`, a leading flag or pathspec magic).
    BlindStaging,
    /// Absolute path or `..` traversal segment.
    PathTraversal,
    /// HEPA runtime/control artifact that must never enter the target repo.
    RuntimePath,
    /// Secret-like filename, suffix, or credential directory.
    SecretLike,
}

impl HepaStagingRejectionReason {
    fn describe(self) -> &'static str {
        match self {
            HepaStagingRejectionReason::BlindStaging => "blind staging marker is not allowed",
            HepaStagingRejectionReason::PathTraversal => "path traversal is not allowed",
            HepaStagingRejectionReason::RuntimePath => "HEPA runtime paths must not be staged",
            HepaStagingRejectionReason::SecretLike => "secret-like paths must not be staged",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaStagingRejection {
    pub path: String,
    pub reason: HepaStagingRejectionReason,
}

#[derive(Debug)]
pub struct HepaStagingError {
    pub field: String,
    pub message: String,
    pub rejections: Vec<HepaStagingRejection>,
}

impl HepaStagingError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            rejections: Vec::new(),
        }
    }

    fn rejected(rejections: Vec<HepaStagingRejection>) -> Self {
        let summary = rejections
            .iter()
            .map(|rejection| format!("{}: {}", rejection.path, rejection.reason.describe()))
            .collect::<Vec<_>>()
            .join("; ");
        Self {
            field: "approved_files".to_string(),
            message: format!("refused unsafe staging paths: {summary}"),
            rejections,
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

fn literal_pathspec(path: &str) -> String {
    format!(":(literal){path}")
}

const BLIND_STAGING_TOKENS: &[&str] = &[".", "./", "*", ":/", ":", "-A", "-u"];

const FORBIDDEN_SECRET_FILENAMES: &[&str] = &[
    ".env",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".htpasswd",
    "credentials",
    "application_default_credentials.json",
    "cookies",
    "cookies.sqlite",
    "id_rsa",
    "id_ed25519",
];

const FORBIDDEN_SECRET_SUFFIXES: &[&str] = &[
    ".jks",
    ".keystore",
    ".key",
    ".pem",
    ".p12",
    ".pfx",
    ".kubeconfig",
];

const FORBIDDEN_SECRET_PATHS: &[&str] = &[
    ".aws/credentials",
    ".config/gcloud/application_default_credentials.json",
    ".docker/config.json",
    ".github/secrets",
    ".gnupg",
    ".ssh",
    ".azure",
];

const CREDENTIAL_STORE_DIRECTORIES: &[&str] = &[".aws", ".azure", ".docker", ".gnupg", ".ssh"];

/// Classify an approved-file path, returning the first refusal reason found.
fn classify_staging_path(path: &str) -> Option<HepaStagingRejectionReason> {
    let trimmed = path.trim();
    if is_blind_staging_marker(trimmed) {
        return Some(HepaStagingRejectionReason::BlindStaging);
    }
    if is_path_traversal(trimmed) {
        return Some(HepaStagingRejectionReason::PathTraversal);
    }
    if is_runtime_path(trimmed) {
        return Some(HepaStagingRejectionReason::RuntimePath);
    }
    if is_secret_like_path(trimmed) {
        return Some(HepaStagingRejectionReason::SecretLike);
    }
    None
}

fn is_blind_staging_marker(path: &str) -> bool {
    if BLIND_STAGING_TOKENS.contains(&path) {
        return true;
    }
    // Leading dash is option injection; leading colon is pathspec magic; `*`
    // and `?` are globs that can match unintended files even after `--`.
    path.starts_with('-')
        || path.starts_with(':')
        || path.contains('*')
        || path.contains('?')
        || path == ".."
}

fn is_path_traversal(path: &str) -> bool {
    let candidate = Path::new(path);
    candidate.is_absolute()
        || candidate
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn is_runtime_path(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|segment| segment == ".hepa" || segment.starts_with(".hepa-"))
    })
}

fn is_secret_like_path(path: &str) -> bool {
    let lower_path = path.replace('\\', "/").to_ascii_lowercase();
    let lower_path = lower_path.trim_matches('/');
    let candidate = Path::new(lower_path);
    let name = candidate
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let parts: Vec<&str> = candidate
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();

    if FORBIDDEN_SECRET_FILENAMES.contains(&name) {
        return true;
    }
    if name.starts_with(".env.") && name != ".env.example" {
        return true;
    }
    if FORBIDDEN_SECRET_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return true;
    }
    if FORBIDDEN_SECRET_PATHS.iter().any(|secret_path| {
        lower_path == *secret_path || lower_path.starts_with(&format!("{secret_path}/"))
    }) {
        return true;
    }
    parts
        .iter()
        .any(|part| CREDENTIAL_STORE_DIRECTORIES.contains(part))
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

    #[test]
    fn stage_approved_files_blocks_blind_staging_markers() {
        let repo = unique_test_dir("stage-blind");
        init_repo(&repo);
        fs::write(repo.join("real.txt"), "real change\n").expect("real write");
        let staging = HepaSafeStaging::new(&repo);

        for blind in [".", "./", "*", "-A", ":/", "src/*.rs"] {
            let error = staging
                .stage_approved_files(&[blind.to_string()])
                .expect_err("blind staging marker must be refused");
            assert_eq!(error.rejections.len(), 1);
            assert_eq!(
                error.rejections[0].reason,
                HepaStagingRejectionReason::BlindStaging
            );
        }
        // Nothing was staged by any of the refused attempts.
        assert!(staged_names(&repo).is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_blocks_secret_runtime_and_traversal_paths() {
        let repo = unique_test_dir("stage-secret");
        init_repo(&repo);
        let staging = HepaSafeStaging::new(&repo);

        let cases = [
            (".env", HepaStagingRejectionReason::SecretLike),
            (".env.production", HepaStagingRejectionReason::SecretLike),
            ("config/server.pem", HepaStagingRejectionReason::SecretLike),
            (".ssh/id_rsa", HepaStagingRejectionReason::SecretLike),
            (
                ".hepa/control/run.json",
                HepaStagingRejectionReason::RuntimePath,
            ),
            (
                ".hepa-worktree.json",
                HepaStagingRejectionReason::RuntimePath,
            ),
            ("../escape.txt", HepaStagingRejectionReason::PathTraversal),
            ("/etc/passwd", HepaStagingRejectionReason::PathTraversal),
        ];

        for (path, expected) in cases {
            let error = staging
                .stage_approved_files(&[path.to_string()])
                .expect_err("unsafe path must be refused");
            assert_eq!(error.rejections.len(), 1, "path {path} should be refused");
            assert_eq!(error.rejections[0].reason, expected, "path {path}");
        }
        assert!(staged_names(&repo).is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_refuses_whole_list_when_any_path_is_unsafe() {
        let repo = unique_test_dir("stage-mixed");
        init_repo(&repo);
        fs::write(repo.join("safe.txt"), "safe change\n").expect("safe write");
        fs::write(repo.join(".env"), "SECRET=1\n").expect("secret write");
        let staging = HepaSafeStaging::new(&repo);

        let error = staging
            .stage_approved_files(&["safe.txt".to_string(), ".env".to_string()])
            .expect_err("a mixed list with one unsafe path must be refused");

        assert_eq!(error.rejections.len(), 1);
        assert_eq!(error.rejections[0].path, ".env");
        // The safe file is not partially staged.
        assert!(staged_names(&repo).is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_allows_bracketed_dynamic_route_files() {
        let repo = unique_test_dir("stage-bracket");
        init_repo(&repo);
        fs::create_dir_all(repo.join("app")).expect("app dir");
        fs::write(repo.join("app/[id].tsx"), "export default 1\n").expect("route write");
        let staging = HepaSafeStaging::new(&repo);

        let report = staging
            .stage_approved_files(&["app/[id].tsx".to_string()])
            .expect("literal pathspec should stage bracketed route files");

        assert_eq!(report.staged_files, vec!["app/[id].tsx".to_string()]);

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
