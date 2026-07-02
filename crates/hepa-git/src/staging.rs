use std::{
    error::Error,
    fmt, fs, io,
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
                continue;
            }
            if let Some(reason) = self.classify_staging_content(path)? {
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

    fn classify_staging_content(
        &self,
        path: &str,
    ) -> Result<Option<HepaStagingRejectionReason>, HepaStagingError> {
        let full_path = self.repo_root.join(path);
        if !full_path.exists() {
            // Deletions are safe to stage after path screening.
            return Ok(None);
        }
        let metadata = fs::metadata(&full_path)?;
        if !metadata.is_file() {
            return Ok(None);
        }
        let bytes = fs::read(&full_path)?;
        let content = String::from_utf8_lossy(&bytes);
        Ok(classify_staging_content_for_path(path, &content))
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
    /// File content contains a local absolute path, raw token, or secret assignment.
    ContentPrivacy,
}

impl HepaStagingRejectionReason {
    fn describe(self) -> &'static str {
        match self {
            HepaStagingRejectionReason::BlindStaging => "blind staging marker is not allowed",
            HepaStagingRejectionReason::PathTraversal => "path traversal is not allowed",
            HepaStagingRejectionReason::RuntimePath => "HEPA runtime paths must not be staged",
            HepaStagingRejectionReason::SecretLike => "secret-like paths must not be staged",
            HepaStagingRejectionReason::ContentPrivacy => {
                "file content contains private path or secret-like material"
            }
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

const FORBIDDEN_CONTENT_PATH_MARKERS: &[&str] =
    &["/Users/", "/home/", "C:\\Users\\", "C:/Users/", "\\Users\\"];

const FORBIDDEN_CONTENT_ARTIFACT_MARKERS: &[&str] = &[
    "</content>",
    "<parameter name=\"filePath\"",
    "<parameter name=\"file_path\"",
];

const FORBIDDEN_CONTENT_TOKEN_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "sk-",
    "AKIA",
];

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

fn classify_staging_content_for_path(
    path: &str,
    content: &str,
) -> Option<HepaStagingRejectionReason> {
    classify_staging_content_with_options(
        content,
        !is_dependency_lockfile(path),
        !is_test_or_fixture_path(path),
    )
}

fn classify_staging_content_with_options(
    content: &str,
    scan_secret_assignments: bool,
    scan_private_path_markers: bool,
) -> Option<HepaStagingRejectionReason> {
    if (scan_private_path_markers
        && FORBIDDEN_CONTENT_PATH_MARKERS
            .iter()
            .any(|marker| content.contains(marker)))
        || FORBIDDEN_CONTENT_ARTIFACT_MARKERS
            .iter()
            .any(|marker| content.contains(marker))
    {
        return Some(HepaStagingRejectionReason::ContentPrivacy);
    }

    if content.split_whitespace().any(token_has_forbidden_prefix) {
        return Some(HepaStagingRejectionReason::ContentPrivacy);
    }

    if scan_secret_assignments && content.lines().any(line_has_secret_assignment) {
        return Some(HepaStagingRejectionReason::ContentPrivacy);
    }

    None
}

fn is_dependency_lockfile(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    matches!(
        name,
        "pnpm-lock.yaml" | "package-lock.json" | "yarn.lock" | "bun.lock" | "bun.lockb"
    )
}

fn is_test_or_fixture_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/test/")
        || normalized.contains("/tests/")
        || normalized.contains("/__tests__/")
        || normalized.contains("/fixtures/")
        || normalized.ends_with(".test.ts")
        || normalized.ends_with(".test.tsx")
        || normalized.ends_with(".test.js")
        || normalized.ends_with(".test.jsx")
        || normalized.ends_with(".spec.ts")
        || normalized.ends_with(".spec.tsx")
        || normalized.ends_with(".spec.js")
        || normalized.ends_with(".spec.jsx")
}

fn token_has_forbidden_prefix(token: &str) -> bool {
    let core_start = token.find(|character: char| character.is_ascii_alphanumeric());
    let core_end = token
        .rfind(|character: char| character.is_ascii_alphanumeric())
        .map(|index| index + 1);
    let Some(core_start) = core_start else {
        return false;
    };
    let Some(core_end) = core_end else {
        return false;
    };
    if core_start >= core_end {
        return false;
    }
    let core = &token[core_start..core_end];
    FORBIDDEN_CONTENT_TOKEN_PREFIXES
        .iter()
        .any(|prefix| core.starts_with(prefix) && core.len() > prefix.len())
}

fn line_has_secret_assignment(line: &str) -> bool {
    for separator in ['=', ':'] {
        let Some(index) = line.find(separator) else {
            continue;
        };
        let key = line[..index].trim().trim_matches(|character: char| {
            matches!(character, '"' | '\'' | '`') || character.is_ascii_whitespace()
        });
        if !looks_like_secret_assignment_key(key) {
            continue;
        }
        if is_secret_assignment_key(key) {
            let raw_value = line[index + separator.len_utf8()..].trim();
            if separator == ':' && is_safe_type_annotation(raw_value) {
                continue;
            }
            if is_safe_auth_code_reference(raw_value) {
                continue;
            }
            if is_documented_secret_placeholder(raw_value) {
                continue;
            }
            let value = raw_value
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(|character: char| {
                    matches!(
                        character,
                        '"' | '\'' | '`' | ',' | ';' | ')' | ']' | '}' | '<' | '>'
                    )
                });
            if is_documented_secret_placeholder(value) {
                continue;
            }
            if is_safe_auth_code_reference(value) {
                continue;
            }
            return true;
        }
    }
    false
}

fn looks_like_secret_assignment_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 80
        && key.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}

fn is_secret_assignment_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    if matches!(
        normalized.as_str(),
        "api_key" | "apikey" | "authorization" | "private_key"
    ) {
        return true;
    }

    let parts: Vec<&str> = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect();

    if parts.as_slice() == ["api", "key"] || parts.as_slice() == ["private", "key"] {
        return true;
    }

    parts.iter().any(|part| {
        matches!(
            *part,
            "authorization" | "credential" | "credentials" | "secret" | "token"
        )
    })
}

fn is_documented_secret_placeholder(value: &str) -> bool {
    let normalized = value
        .trim()
        .trim_matches(|character: char| {
            matches!(
                character,
                '"' | '\'' | '`' | ',' | ';' | ')' | ']' | '}' | '<' | '>'
            )
        })
        .to_ascii_lowercase();
    normalized.starts_with("your-")
        || normalized.starts_with("<your-")
        || normalized == "test-secret-value"
        || normalized == "test-jwt-secret"
        || normalized == "bearer token"
        || normalized == "bearer <token>"
        || normalized == "bearer your-token"
        || normalized == "nope"
        || normalized == "none"
        || normalized == "dummy"
}

fn is_safe_auth_code_reference(value: &str) -> bool {
    if value.contains("${this.authToken}") || value.contains("${this.refreshToken}") {
        return true;
    }
    let normalized = value.trim_matches(|character: char| {
        matches!(
            character,
            '"' | '\'' | '`' | ',' | ';' | ')' | ']' | '}' | '<' | '>'
        )
    });
    if normalized.ends_with("_error") || normalized.ends_with("-error") {
        return true;
    }
    matches!(
        normalized,
        "token"
            | "refreshToken"
            | "authToken"
            | "this.token"
            | "this.refreshToken"
            | "this.authToken"
            | "null"
            | "undefined"
    )
}

fn is_safe_type_annotation(value: &str) -> bool {
    let normalized = value.trim_start_matches('?').trim_start();
    matches!(
        normalized.split_whitespace().next().unwrap_or_default(),
        "string"
            | "number"
            | "boolean"
            | "unknown"
            | "void"
            | "Promise<void>"
            | "Promise<string>"
            | "Promise<boolean>"
            | "string;"
            | "number;"
            | "boolean;"
    )
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
    fn stage_approved_files_blocks_private_paths_in_file_content() {
        let repo = unique_test_dir("stage-content-path");
        init_repo(&repo);
        fs::write(
            repo.join("AGENTS.md"),
            "Generated footer: /Users/example/workspace/project/AGENTS.md\n",
        )
        .expect("agents write");
        let staging = HepaSafeStaging::new(&repo);

        let error = staging
            .stage_approved_files(&["AGENTS.md".to_string()])
            .expect_err("private local path content must be refused");

        assert_eq!(error.rejections.len(), 1);
        assert_eq!(error.rejections[0].path, "AGENTS.md");
        assert_eq!(
            error.rejections[0].reason,
            HepaStagingRejectionReason::ContentPrivacy
        );
        assert!(staged_names(&repo).is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_blocks_generated_artifact_footer_content() {
        let repo = unique_test_dir("stage-content-footer");
        init_repo(&repo);
        fs::write(
            repo.join("AGENTS.md"),
            "Useful project instructions\n</content>\n<parameter name=\"filePath\">AGENTS.md\n",
        )
        .expect("agents write");
        let staging = HepaSafeStaging::new(&repo);

        let error = staging
            .stage_approved_files(&["AGENTS.md".to_string()])
            .expect_err("generated artifact footer content must be refused");

        assert_eq!(error.rejections.len(), 1);
        assert_eq!(error.rejections[0].path, "AGENTS.md");
        assert_eq!(
            error.rejections[0].reason,
            HepaStagingRejectionReason::ContentPrivacy
        );
        assert!(staged_names(&repo).is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_blocks_secret_assignments_in_file_content() {
        let repo = unique_test_dir("stage-content-secret");
        init_repo(&repo);
        fs::write(repo.join("config.md"), "api_key = example-value\n").expect("config write");
        let staging = HepaSafeStaging::new(&repo);

        let error = staging
            .stage_approved_files(&["config.md".to_string()])
            .expect_err("secret assignment content must be refused");

        assert_eq!(error.rejections.len(), 1);
        assert_eq!(error.rejections[0].path, "config.md");
        assert_eq!(
            error.rejections[0].reason,
            HepaStagingRejectionReason::ContentPrivacy
        );
        assert!(staged_names(&repo).is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_allows_documented_secret_placeholders() {
        let repo = unique_test_dir("stage-content-secret-placeholder");
        init_repo(&repo);
        fs::write(
            repo.join("README.md"),
            "Create `.env.local`:\n\nNEXTAUTH_SECRET=your-nextauth-secret\n",
        )
        .expect("readme write");
        let staging = HepaSafeStaging::new(&repo);

        let report = staging
            .stage_approved_files(&["README.md".to_string()])
            .expect("documented placeholder secret values should not block staging");

        assert_eq!(report.staged_files, vec!["README.md".to_string()]);

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_still_blocks_real_secret_values_in_docs() {
        let repo = unique_test_dir("stage-content-secret-real-doc");
        init_repo(&repo);
        fs::write(
            repo.join("README.md"),
            "Create `.env.local`:\n\nNEXTAUTH_SECRET=real-token-value\n",
        )
        .expect("readme write");
        let staging = HepaSafeStaging::new(&repo);

        let error = staging
            .stage_approved_files(&["README.md".to_string()])
            .expect_err("real secret-looking values should still block staging");

        assert_eq!(error.rejections.len(), 1);
        assert_eq!(
            error.rejections[0].reason,
            HepaStagingRejectionReason::ContentPrivacy
        );
        assert!(staged_names(&repo).is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_blocks_json_secret_assignment_keys() {
        let repo = unique_test_dir("stage-content-json-secret");
        init_repo(&repo);
        fs::write(
            repo.join("config.json"),
            "{\n  \"auth_token\": \"example-value\"\n}\n",
        )
        .expect("config write");
        let staging = HepaSafeStaging::new(&repo);

        let error = staging
            .stage_approved_files(&["config.json".to_string()])
            .expect_err("json secret assignment content must be refused");

        assert_eq!(error.rejections.len(), 1);
        assert_eq!(error.rejections[0].path, "config.json");
        assert_eq!(
            error.rejections[0].reason,
            HepaStagingRejectionReason::ContentPrivacy
        );
        assert!(staged_names(&repo).is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_allows_synthetic_private_paths_in_tests() {
        let repo = unique_test_dir("stage-content-test-path");
        init_repo(&repo);
        fs::create_dir_all(repo.join("src/main")).expect("src dir");
        fs::write(
            repo.join("src/main/config.test.ts"),
            "expect(configPath('/Users/tester/tmp/workspace')).toContain('/Users/tester/tmp');\n",
        )
        .expect("test write");
        let staging = HepaSafeStaging::new(&repo);

        let report = staging
            .stage_approved_files(&["src/main/config.test.ts".to_string()])
            .expect("synthetic test fixture paths should not block staging");

        assert_eq!(
            report.staged_files,
            vec!["src/main/config.test.ts".to_string()]
        );

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_allows_gateway_health_test_fixtures() {
        let repo = unique_test_dir("stage-content-gateway-health-test");
        init_repo(&repo);
        fs::create_dir_all(repo.join("apps/api-gateway/src/__tests__")).expect("test dir");
        fs::write(
            repo.join("apps/api-gateway/src/__tests__/health.test.ts"),
            "beforeAll(() => {\n  process.env.JWT_SECRET = 'test-secret-value';\n  process.env.CORS_ORIGIN = 'http://localhost:3000';\n});\nmockUpstreams({\n  'localhost:3001': healthyResponse(JSON.stringify({ status: 'ready' })),\n  'localhost:3002': unhealthyResponse(503, 'Service Unavailable'),\n});\nexpect(body.status).toBe('all-down');\n",
        )
        .expect("health test write");
        let staging = HepaSafeStaging::new(&repo);

        let report = staging
            .stage_approved_files(&["apps/api-gateway/src/__tests__/health.test.ts".to_string()])
            .expect("ordinary gateway health test fixtures should stage");

        assert_eq!(
            report.staged_files,
            vec!["apps/api-gateway/src/__tests__/health.test.ts".to_string()]
        );

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_allows_bearer_token_placeholders_in_tests() {
        let repo = unique_test_dir("stage-content-bearer-token-test");
        init_repo(&repo);
        fs::create_dir_all(repo.join("apps/api-gateway/src/__tests__")).expect("test dir");
        let content = "const request = new Request('http://gateway.local', {\n  headers: {\n    authorization: 'Bearer token',\n    'x-secret-debug': 'nope',\n  },\n});\nexpect(headers.get('authorization')).toBe('Bearer token');\nexpect(headers.get('x-secret-debug')).toBeNull();\n";
        assert_eq!(
            classify_staging_path("apps/api-gateway/src/__tests__/route-proxy.test.ts"),
            None
        );
        for line in content.lines() {
            assert!(
                !line_has_secret_assignment(line),
                "fixture line should not be a secret assignment: {line}"
            );
            assert!(
                !line.split_whitespace().any(token_has_forbidden_prefix),
                "fixture line should not contain a forbidden token prefix: {line}"
            );
        }
        assert_eq!(
            classify_staging_content_for_path(
                "apps/api-gateway/src/__tests__/route-proxy.test.ts",
                content
            ),
            None
        );
        fs::write(
            repo.join("apps/api-gateway/src/__tests__/route-proxy.test.ts"),
            content,
        )
        .expect("route proxy test write");
        let staging = HepaSafeStaging::new(&repo);

        let report = staging
            .stage_approved_files(&[
                "apps/api-gateway/src/__tests__/route-proxy.test.ts".to_string()
            ])
            .expect("ordinary bearer-token placeholders in tests should stage");

        assert_eq!(
            report.staged_files,
            vec!["apps/api-gateway/src/__tests__/route-proxy.test.ts".to_string()]
        );

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_still_blocks_secret_assignments_in_tests() {
        let repo = unique_test_dir("stage-content-test-secret");
        init_repo(&repo);
        fs::create_dir_all(repo.join("src/main")).expect("src dir");
        fs::write(
            repo.join("src/main/config.test.ts"),
            "api_key = 'real-value'\n",
        )
        .expect("test write");
        let staging = HepaSafeStaging::new(&repo);

        let error = staging
            .stage_approved_files(&["src/main/config.test.ts".to_string()])
            .expect_err("test files must still block secret assignments");

        assert_eq!(error.rejections.len(), 1);
        assert_eq!(
            error.rejections[0].reason,
            HepaStagingRejectionReason::ContentPrivacy
        );
        assert!(staged_names(&repo).is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_still_blocks_real_bearer_values_in_tests() {
        let repo = unique_test_dir("stage-content-real-bearer-test");
        init_repo(&repo);
        fs::create_dir_all(repo.join("src/main")).expect("src dir");
        fs::write(
            repo.join("src/main/config.test.ts"),
            "authorization: 'Bearer real-production-token-value'\n",
        )
        .expect("test write");
        let staging = HepaSafeStaging::new(&repo);

        let error = staging
            .stage_approved_files(&["src/main/config.test.ts".to_string()])
            .expect_err("real bearer-looking test values should still block");

        assert_eq!(error.rejections.len(), 1);
        assert_eq!(
            error.rejections[0].reason,
            HepaStagingRejectionReason::ContentPrivacy
        );
        assert!(staged_names(&repo).is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_allows_auth_token_code_references() {
        let repo = unique_test_dir("stage-content-auth-code");
        init_repo(&repo);
        fs::create_dir_all(repo.join("src/api")).expect("src dir");
        fs::write(
            repo.join("src/api/BaseApiClient.ts"),
            "enum ApiErrorCode {\n  AUTHORIZATION_ERROR = 'authorization_error',\n}\nsetAuthToken(token: string, refreshToken?: string): void {\n  this.authToken = token;\n  this.refreshToken = refreshToken ?? null;\n}\nconst body = { refreshToken: this.refreshToken };\nconfig.headers.Authorization = `Bearer ${this.authToken}`;\n",
        )
        .expect("api client write");
        let staging = HepaSafeStaging::new(&repo);

        let report = staging
            .stage_approved_files(&["src/api/BaseApiClient.ts".to_string()])
            .expect("ordinary auth token code references should stage");

        assert_eq!(
            report.staged_files,
            vec!["src/api/BaseApiClient.ts".to_string()]
        );

        remove_test_dir(repo);
    }

    #[test]
    fn auth_token_code_lines_are_not_secret_assignments() {
        for line in [
            "  AUTHORIZATION_ERROR = 'authorization_error',",
            "setAuthToken(token: string, refreshToken?: string): void {",
            "  this.authToken = token;",
            "  this.refreshToken = refreshToken ?? null;",
            "const body = { refreshToken: this.refreshToken };",
            "config.headers.Authorization = `Bearer ${this.authToken}`;",
        ] {
            assert!(
                !line_has_secret_assignment(line),
                "line should not be classified as a secret assignment: {line}"
            );
        }
    }

    #[test]
    fn stage_approved_files_allows_normal_package_metadata() {
        let repo = unique_test_dir("stage-package-metadata");
        init_repo(&repo);
        fs::write(
            repo.join("package.json"),
            r#"{
  "name": "fixture",
  "private": true,
  "scripts": {
    "tokens:build": "node scripts/build-tokens.mjs",
    "doctor": "node scripts/doctor.mjs"
  },
  "devDependencies": {
    "@types/node": "latest"
  }
}
"#,
        )
        .expect("package write");
        let staging = HepaSafeStaging::new(&repo);

        let report = staging
            .stage_approved_files(&["package.json".to_string()])
            .expect("normal package metadata should not be treated as a secret");

        assert_eq!(report.staged_files, vec!["package.json".to_string()]);

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_allows_dependency_lockfile_package_names() {
        let repo = unique_test_dir("stage-lockfile-package-names");
        init_repo(&repo);
        fs::write(
            repo.join("pnpm-lock.yaml"),
            r#"lockfileVersion: '9.0'

packages:
  '@octokit/auth-token@6.0.0': {}
  '@azure/keyvault-secrets@4.9.0': {}
  '@csstools/css-tokenizer@4.0.0': {}
"#,
        )
        .expect("lockfile write");
        let staging = HepaSafeStaging::new(&repo);

        let report = staging
            .stage_approved_files(&["pnpm-lock.yaml".to_string()])
            .expect("normal lockfile package names should not be treated as secrets");

        assert_eq!(report.staged_files, vec!["pnpm-lock.yaml".to_string()]);

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_still_blocks_private_paths_in_lockfiles() {
        let repo = unique_test_dir("stage-lockfile-private-path");
        init_repo(&repo);
        fs::write(
            repo.join("pnpm-lock.yaml"),
            "packages:\n  local-file: file:/Users/example/private/package.tgz\n",
        )
        .expect("lockfile write");
        let staging = HepaSafeStaging::new(&repo);

        let error = staging
            .stage_approved_files(&["pnpm-lock.yaml".to_string()])
            .expect_err("private local paths in lockfiles must still be refused");

        assert_eq!(error.rejections.len(), 1);
        assert_eq!(
            error.rejections[0].reason,
            HepaStagingRejectionReason::ContentPrivacy
        );
        assert!(staged_names(&repo).is_empty());

        remove_test_dir(repo);
    }

    #[test]
    fn stage_approved_files_allows_normal_route_docs() {
        let repo = unique_test_dir("stage-content-route");
        init_repo(&repo);
        fs::write(
            repo.join("AGENTS.md"),
            "Gateway routes REST under /api/v1/* and GraphQL under /graphql.\n\n- **Secrets**: Managed via AWS Secrets Manager and GitHub Environment Secrets\n",
        )
        .expect("agents write");
        let staging = HepaSafeStaging::new(&repo);

        let report = staging
            .stage_approved_files(&["AGENTS.md".to_string()])
            .expect("route docs should not look like private paths");

        assert_eq!(report.staged_files, vec!["AGENTS.md".to_string()]);

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
