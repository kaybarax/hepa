use std::collections::{BTreeMap, BTreeSet};

/// The role an adapter process runs under for env scoping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HepaEnvRole {
    Manager,
    Worker,
    Reviewer,
}

/// Environment keys that are always safe to pass to any role.
pub const BASE_ENV_ALLOWLIST: &[&str] = &[
    "PATH", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "TERM", "TMPDIR", "TZ",
];

/// Credential keys that only the manager may ever receive. Worker and reviewer
/// adapters are never given these, even if an adapter declares them.
pub const MANAGER_ONLY_CREDENTIALS: &[&str] = &[
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "HEPA_GITHUB_TOKEN",
];

/// A default-deny environment allowlist scoped to one role and adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaEnvAllowlist {
    role: HepaEnvRole,
    adapter_allowed: BTreeSet<String>,
}

impl HepaEnvAllowlist {
    /// Build the allowlist for a role plus any adapter-declared required env
    /// keys. Adapter-declared keys never override the manager-only credential
    /// rule for worker/reviewer roles.
    pub fn for_role(role: HepaEnvRole, adapter_required_env: &[String]) -> Self {
        Self {
            role,
            adapter_allowed: adapter_required_env.iter().cloned().collect(),
        }
    }

    /// Whether a key may pass to this role's adapter process.
    pub fn is_allowed(&self, key: &str) -> bool {
        if is_manager_only_credential(key) {
            return self.role == HepaEnvRole::Manager;
        }
        BASE_ENV_ALLOWLIST.contains(&key) || self.adapter_allowed.contains(key)
    }

    /// Filter an environment to only the allowed keys (default-deny).
    pub fn filter(&self, env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        env.iter()
            .filter(|(key, _)| self.is_allowed(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

/// Case-insensitive check for a manager-only credential key.
pub fn is_manager_only_credential(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    MANAGER_ONLY_CREDENTIALS.contains(&upper.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("GITHUB_TOKEN".to_string(), "ghp_example".to_string()),
            ("OPENAI_API_KEY".to_string(), "sk-example".to_string()),
            ("HOME".to_string(), "/home/dev".to_string()),
        ])
    }

    #[test]
    fn worker_and_reviewer_never_receive_manager_credentials() {
        for role in [HepaEnvRole::Worker, HepaEnvRole::Reviewer] {
            let allowlist = HepaEnvAllowlist::for_role(role, &[]);
            let filtered = allowlist.filter(&env());

            assert!(!filtered.contains_key("GITHUB_TOKEN"));
            // Default-deny: non-allowlisted keys are dropped too.
            assert!(!filtered.contains_key("OPENAI_API_KEY"));
            assert!(filtered.contains_key("PATH"));
            assert!(filtered.contains_key("HOME"));
        }
    }

    #[test]
    fn adapter_required_env_is_allowed_but_credentials_are_not() {
        let allowlist =
            HepaEnvAllowlist::for_role(HepaEnvRole::Worker, &["OPENAI_API_KEY".to_string()]);
        let filtered = allowlist.filter(&env());

        // The adapter-declared key now passes.
        assert!(filtered.contains_key("OPENAI_API_KEY"));
        // But a manager-only credential still cannot, even if declared.
        let with_token =
            HepaEnvAllowlist::for_role(HepaEnvRole::Worker, &["GITHUB_TOKEN".to_string()]);
        assert!(!with_token.filter(&env()).contains_key("GITHUB_TOKEN"));
    }

    #[test]
    fn manager_keeps_credentials() {
        let allowlist = HepaEnvAllowlist::for_role(HepaEnvRole::Manager, &[]);
        let filtered = allowlist.filter(&env());

        assert_eq!(
            filtered.get("GITHUB_TOKEN").map(String::as_str),
            Some("ghp_example")
        );
    }

    #[test]
    fn credential_check_is_case_insensitive() {
        assert!(is_manager_only_credential("github_token"));
        assert!(is_manager_only_credential("GITHUB_TOKEN"));
        assert!(!is_manager_only_credential("PATH"));
    }
}
