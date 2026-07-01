use crate::contracts::{CONTRACT_SCHEMA_VERSION, HepaAgentRole, HepaContractResult, HepaValidate};
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaHermesProfile {
    pub schema_version: u32,
    pub profile_id: String,
    pub display_name: String,
    pub role: HepaAgentRole,
    pub model_env: String,
    pub responsibilities: Vec<String>,
    pub activation: String,
}

impl HepaValidate for HepaHermesProfile {
    fn validate(&self) -> HepaContractResult {
        validate_profile(self).map_err(|error| crate::contracts::HepaContractError {
            field: error.field,
            message: error.message,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaHermesProfileError {
    pub field: String,
    pub message: String,
}

impl HepaHermesProfileError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaHermesProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaHermesProfileError {}

pub fn default_hermes_profiles() -> Vec<HepaHermesProfile> {
    vec![
        HepaHermesProfile {
            schema_version: CONTRACT_SCHEMA_VERSION,
            profile_id: "hepa-manager".to_string(),
            display_name: "HEPA Manager".to_string(),
            role: HepaAgentRole::Manager,
            model_env: "HEPA_HERMES_MANAGER_MODEL".to_string(),
            responsibilities: vec![
                "intake project specs and tasks into Hermes Kanban".to_string(),
                "prioritize work, assign lanes, and mediate worker/reviewer cycles".to_string(),
                "author project-specific PR intent after gates pass".to_string(),
                "stop after bounded rounds and request human help when needed".to_string(),
            ],
            activation: "always-on orchestration profile for Hermes-led HEPA runs".to_string(),
        },
        HepaHermesProfile {
            schema_version: CONTRACT_SCHEMA_VERSION,
            profile_id: "hepa-worker".to_string(),
            display_name: "HEPA Worker".to_string(),
            role: HepaAgentRole::Worker,
            model_env: "HEPA_HERMES_WORKER_MODEL".to_string(),
            responsibilities: vec![
                "refine Kanban tasks into scoped HEPA run briefs".to_string(),
                "break broad work into finite subtasks when needed".to_string(),
                "prepare repair briefs from accepted review findings".to_string(),
                "delegate code implementation to the configured coding adapter; Pi performs code edits by default".to_string(),
            ],
            activation: "task refinement and repair-brief profile; Pi performs code edits".to_string(),
        },
        HepaHermesProfile {
            schema_version: CONTRACT_SCHEMA_VERSION,
            profile_id: "hepa-reviewer".to_string(),
            display_name: "HEPA Reviewer".to_string(),
            role: HepaAgentRole::Reviewer,
            model_env: "HEPA_HERMES_REVIEWER_MODEL".to_string(),
            responsibilities: vec![
                "review diffs against task brief, validation output, and project standards".to_string(),
                "write actionable QA feedback into review artifacts".to_string(),
                "avoid commits, pushes, branches, pull requests, or direct code changes".to_string(),
            ],
            activation: "one or more fanout review profiles after implementation and validation".to_string(),
        },
        HepaHermesProfile {
            schema_version: CONTRACT_SCHEMA_VERSION,
            profile_id: "hepa-review-manager".to_string(),
            display_name: "HEPA Review Manager".to_string(),
            role: HepaAgentRole::Manager,
            model_env: "HEPA_HERMES_REVIEW_MANAGER_MODEL".to_string(),
            responsibilities: vec![
                "arbitrate multi-reviewer disagreement".to_string(),
                "decide which findings are relevant, accepted, downgraded, or blocking".to_string(),
                "return accepted findings to the manager for repair or PR intent".to_string(),
            ],
            activation: "only when review fanout has findings or disagreement".to_string(),
        },
    ]
}

pub fn validate_default_hermes_profiles() -> Result<(), HepaHermesProfileError> {
    let profiles = default_hermes_profiles();
    for profile in &profiles {
        validate_profile(profile)?;
    }
    for required in [
        "hepa-manager",
        "hepa-worker",
        "hepa-reviewer",
        "hepa-review-manager",
    ] {
        if !profiles
            .iter()
            .any(|profile| profile.profile_id == required)
        {
            return Err(HepaHermesProfileError::new(
                "profile_id",
                format!("missing required Hermes profile {required}"),
            ));
        }
    }
    Ok(())
}

fn validate_profile(profile: &HepaHermesProfile) -> Result<(), HepaHermesProfileError> {
    if profile.schema_version != CONTRACT_SCHEMA_VERSION {
        return Err(HepaHermesProfileError::new(
            "schema_version",
            format!("must be {CONTRACT_SCHEMA_VERSION}"),
        ));
    }
    require_single_line("profile_id", &profile.profile_id)?;
    require_single_line("display_name", &profile.display_name)?;
    require_single_line("model_env", &profile.model_env)?;
    if !profile.model_env.starts_with("HEPA_HERMES_") || !profile.model_env.ends_with("_MODEL") {
        return Err(HepaHermesProfileError::new(
            "model_env",
            "must be a HEPA_HERMES_*_MODEL environment key",
        ));
    }
    if profile.responsibilities.is_empty() {
        return Err(HepaHermesProfileError::new(
            "responsibilities",
            "must not be empty",
        ));
    }
    for (index, responsibility) in profile.responsibilities.iter().enumerate() {
        require_single_line(format!("responsibilities[{index}]"), responsibility)?;
    }
    require_single_line("activation", &profile.activation)?;
    Ok(())
}

fn require_single_line(
    field: impl Into<String>,
    value: &str,
) -> Result<(), HepaHermesProfileError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaHermesProfileError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaHermesProfileError::new(field, "must be a single line"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profiles_include_manager_worker_reviewer_and_review_manager() {
        validate_default_hermes_profiles().expect("default profiles must validate");
        let profiles = default_hermes_profiles();

        assert_eq!(profiles.len(), 4);
        assert!(profiles.iter().any(|profile| {
            profile.profile_id == "hepa-manager"
                && profile
                    .responsibilities
                    .iter()
                    .any(|line| line.contains("PR intent"))
        }));
        assert!(profiles.iter().any(|profile| {
            profile.profile_id == "hepa-worker"
                && profile
                    .responsibilities
                    .iter()
                    .any(|line| line.contains("Pi performs code edits"))
        }));
        assert!(profiles.iter().any(|profile| {
            profile.profile_id == "hepa-reviewer"
                && profile
                    .responsibilities
                    .iter()
                    .any(|line| line.contains("review artifacts"))
        }));
    }
}
