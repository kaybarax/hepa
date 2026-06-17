use hepa_core::contracts::HepaLaneState;
use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

pub const CRATE_NAME: &str = "hepa-memory";

pub fn crate_name() -> &'static str {
    CRATE_NAME
}

/// Maximum bytes any single context pack may hold. Generation is bounded so a
/// pack can never grow without limit.
pub const CONTEXT_PACK_BYTE_BUDGET: usize = 16 * 1024;

/// Maximum number of pattern entries retained per learning pack. Older entries
/// are dropped so packs stay bounded.
pub const MAX_PATTERN_ENTRIES: usize = 200;

/// Lane states that conclude a lane and trigger learning write-back.
pub fn is_terminal_lane_state(state: &HepaLaneState) -> bool {
    matches!(
        state,
        HepaLaneState::Completed
            | HepaLaneState::Blocked
            | HepaLaneState::Failed
            | HepaLaneState::Cancelled
            | HepaLaneState::Cleaned
    )
}

/// The per-project context packs that live under the HEPA control memory root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HepaContextPack {
    ProjectSummary,
    ArchitectureMap,
    TestCommands,
    ReleasePolicy,
    PromptPatterns,
    FailurePatterns,
    AdapterLessons,
}

impl HepaContextPack {
    pub const ALL: [HepaContextPack; 7] = [
        HepaContextPack::ProjectSummary,
        HepaContextPack::ArchitectureMap,
        HepaContextPack::TestCommands,
        HepaContextPack::ReleasePolicy,
        HepaContextPack::PromptPatterns,
        HepaContextPack::FailurePatterns,
        HepaContextPack::AdapterLessons,
    ];

    pub fn file_name(self) -> &'static str {
        match self {
            HepaContextPack::ProjectSummary => "project-summary.md",
            HepaContextPack::ArchitectureMap => "architecture-map.md",
            HepaContextPack::TestCommands => "test-commands.md",
            HepaContextPack::ReleasePolicy => "release-policy.md",
            HepaContextPack::PromptPatterns => "prompt-patterns.md",
            HepaContextPack::FailurePatterns => "failure-patterns.md",
            HepaContextPack::AdapterLessons => "adapter-lessons.md",
        }
    }

    fn title(self) -> &'static str {
        match self {
            HepaContextPack::ProjectSummary => "Project Summary",
            HepaContextPack::ArchitectureMap => "Architecture Map",
            HepaContextPack::TestCommands => "Test Commands",
            HepaContextPack::ReleasePolicy => "Release Policy",
            HepaContextPack::PromptPatterns => "Prompt Patterns",
            HepaContextPack::FailurePatterns => "Failure Patterns",
            HepaContextPack::AdapterLessons => "Adapter Lessons",
        }
    }
}

/// Per-project memory rooted under a control memory directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaProjectMemory {
    root: PathBuf,
    project_id: String,
}

impl HepaProjectMemory {
    pub fn new(
        root: impl Into<PathBuf>,
        project_id: impl Into<String>,
    ) -> Result<Self, HepaMemoryError> {
        let project_id = project_id.into();
        require_safe_segment("project_id", &project_id)?;
        Ok(Self {
            root: root.into(),
            project_id,
        })
    }

    pub fn project_dir(&self) -> PathBuf {
        self.root.join(&self.project_id)
    }

    pub fn pack_path(&self, pack: HepaContextPack) -> PathBuf {
        self.project_dir().join(pack.file_name())
    }

    /// Create any missing context packs with a redacted header. Existing packs
    /// are left untouched so curated content is preserved.
    pub fn ensure_context_packs(&self) -> Result<Vec<PathBuf>, HepaMemoryError> {
        fs::create_dir_all(self.project_dir())?;
        let mut created = Vec::new();
        for pack in HepaContextPack::ALL {
            let path = self.pack_path(pack);
            if !path.exists() {
                let header = format!(
                    "# {}\n\n_Generated HEPA context pack. Keep entries short and redacted._\n",
                    pack.title()
                );
                write_bounded(&path, &header)?;
                created.push(path);
            }
        }
        Ok(created)
    }

    /// Read a pack's contents, returning `None` when it is absent so callers
    /// degrade gracefully instead of failing.
    pub fn read_pack(&self, pack: HepaContextPack) -> Option<String> {
        fs::read_to_string(self.pack_path(pack)).ok()
    }

    /// Append a successful prompt pattern, but only on a terminal lane state.
    /// Returns whether a new entry was written.
    pub fn append_prompt_pattern(
        &self,
        lane_state: &HepaLaneState,
        pattern: &str,
    ) -> Result<bool, HepaMemoryError> {
        self.append_pattern(HepaContextPack::PromptPatterns, lane_state, pattern)
    }

    /// Append a failure pattern, but only on a terminal lane state. Returns
    /// whether a new entry was written.
    pub fn append_failure_pattern(
        &self,
        lane_state: &HepaLaneState,
        pattern: &str,
    ) -> Result<bool, HepaMemoryError> {
        self.append_pattern(HepaContextPack::FailurePatterns, lane_state, pattern)
    }

    fn append_pattern(
        &self,
        pack: HepaContextPack,
        lane_state: &HepaLaneState,
        pattern: &str,
    ) -> Result<bool, HepaMemoryError> {
        if !is_terminal_lane_state(lane_state) {
            return Ok(false);
        }
        let redacted = redact(pattern);
        let redacted = redacted.trim();
        if redacted.is_empty() {
            return Ok(false);
        }
        self.ensure_context_packs()?;

        let path = self.pack_path(pack);
        let existing = fs::read_to_string(&path).unwrap_or_default();
        let entry = format!("- {redacted}");

        let mut header_lines = Vec::new();
        let mut entries = Vec::new();
        for line in existing.lines() {
            if line.starts_with("- ") {
                entries.push(line.to_string());
            } else {
                header_lines.push(line.to_string());
            }
        }
        if entries
            .iter()
            .any(|existing_entry| existing_entry == &entry)
        {
            return Ok(false);
        }
        entries.push(entry);
        if entries.len() > MAX_PATTERN_ENTRIES {
            let overflow = entries.len() - MAX_PATTERN_ENTRIES;
            entries.drain(0..overflow);
        }

        let mut content = header_lines.join("\n");
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push('\n');
        content.push_str(&entries.join("\n"));
        content.push('\n');
        write_bounded(&path, &content)?;
        Ok(true)
    }
}

/// Replace local absolute paths and home directories with placeholders so no
/// pack content leaks private paths.
pub fn redact(line: &str) -> String {
    let mut redacted = String::new();
    for token in line.split_whitespace() {
        if !redacted.is_empty() {
            redacted.push(' ');
        }
        if token.starts_with("/Users/") || token.starts_with("/home/") || token.starts_with('/') {
            redacted.push_str("<path>");
        } else if token.starts_with("~/") {
            redacted.push_str("<home>");
        } else {
            redacted.push_str(token);
        }
    }
    redacted
}

/// Write content, truncating to the context-pack byte budget on a char
/// boundary so packs stay bounded.
fn write_bounded(path: &Path, content: &str) -> Result<(), HepaMemoryError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bounded = bound_to_budget(content);
    fs::write(path, bounded)?;
    Ok(())
}

fn bound_to_budget(content: &str) -> &str {
    if content.len() <= CONTEXT_PACK_BYTE_BUDGET {
        return content;
    }
    let mut end = CONTEXT_PACK_BYTE_BUDGET;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[..end]
}

fn require_safe_segment(field: &str, value: &str) -> Result<(), HepaMemoryError> {
    if value.trim().is_empty() {
        return Err(HepaMemoryError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaMemoryError::new(field, "must be a single line"));
    }
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(HepaMemoryError::new(
            field,
            "must not contain path separators or traversal",
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub struct HepaMemoryError {
    pub field: String,
    pub message: String,
}

impl HepaMemoryError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaMemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaMemoryError {}

impl From<io::Error> for HepaMemoryError {
    fn from(error: io::Error) -> Self {
        Self::new("io", error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn ensure_context_packs_creates_all_seven_packs() {
        let root = unique_test_dir("packs");
        let memory = HepaProjectMemory::new(&root, "project-1").expect("valid project");

        let created = memory
            .ensure_context_packs()
            .expect("pack creation should succeed");

        assert_eq!(created.len(), 7);
        for pack in HepaContextPack::ALL {
            assert!(memory.pack_path(pack).exists(), "{:?} missing", pack);
        }
        // Re-running is idempotent: nothing new is created.
        let again = memory.ensure_context_packs().expect("idempotent");
        assert!(again.is_empty());

        remove_test_dir(root);
    }

    #[test]
    fn missing_pack_reads_degrade_gracefully() {
        let root = unique_test_dir("missing");
        let memory = HepaProjectMemory::new(&root, "project-1").expect("valid project");

        assert!(memory.read_pack(HepaContextPack::FailurePatterns).is_none());

        remove_test_dir(root);
    }

    #[test]
    fn project_id_rejects_path_traversal() {
        let root = unique_test_dir("traversal");
        assert!(HepaProjectMemory::new(&root, "../escape").is_err());
        assert!(HepaProjectMemory::new(&root, "a/b").is_err());
        remove_test_dir(root);
    }

    #[test]
    fn appends_patterns_only_on_terminal_lane_states() {
        let root = unique_test_dir("append-terminal");
        let memory = HepaProjectMemory::new(&root, "project-1").expect("valid project");

        // Non-terminal state is a no-op.
        assert!(
            !memory
                .append_prompt_pattern(&HepaLaneState::Running, "use focused diffs")
                .expect("running append")
        );
        // Terminal success appends a prompt pattern.
        assert!(
            memory
                .append_prompt_pattern(&HepaLaneState::Completed, "use focused diffs")
                .expect("completed append")
        );
        let prompt = memory
            .read_pack(HepaContextPack::PromptPatterns)
            .expect("prompt pack exists");
        assert!(prompt.contains("- use focused diffs"));

        // Terminal failure appends a failure pattern.
        assert!(
            memory
                .append_failure_pattern(&HepaLaneState::Blocked, "lockfile drift breaks install")
                .expect("blocked append")
        );
        let failures = memory
            .read_pack(HepaContextPack::FailurePatterns)
            .expect("failure pack exists");
        assert!(failures.contains("- lockfile drift breaks install"));

        remove_test_dir(root);
    }

    #[test]
    fn append_dedupes_and_redacts_patterns() {
        let root = unique_test_dir("append-dedupe");
        let memory = HepaProjectMemory::new(&root, "project-1").expect("valid project");

        assert!(
            memory
                .append_failure_pattern(&HepaLaneState::Failed, "test failed in /Users/x/app")
                .expect("first append")
        );
        // Identical (post-redaction) entry is deduped.
        assert!(
            !memory
                .append_failure_pattern(&HepaLaneState::Failed, "test failed in /Users/x/app")
                .expect("dedupe append")
        );

        let failures = memory
            .read_pack(HepaContextPack::FailurePatterns)
            .expect("failure pack exists");
        assert!(!failures.contains("/Users/"));
        assert_eq!(failures.matches("- test failed in").count(), 1);

        remove_test_dir(root);
    }

    #[test]
    fn redaction_strips_local_paths() {
        let redacted = redact("worker ran in /Users/someone/repo and ~/cache");
        assert!(!redacted.contains("/Users/"));
        assert!(!redacted.contains("~/"));
        assert!(redacted.contains("<path>"));
        assert!(redacted.contains("<home>"));
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-memory-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
