use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaBoardTransitionRequest {
    pub request_id: String,
    pub task_id: String,
    pub card_id: Option<String>,
    pub requested_by: String,
    pub action: HepaBoardTransitionAction,
    pub requested_at: String,
}

impl HepaBoardTransitionRequest {
    pub fn validate(&self) -> Result<(), HepaBoardTransitionError> {
        require_single_line("request_id", &self.request_id)?;
        require_single_line("task_id", &self.task_id)?;
        if let Some(card_id) = &self.card_id {
            require_single_line("card_id", card_id)?;
        }
        require_single_line("requested_by", &self.requested_by)?;
        self.action.validate()?;
        require_single_line("requested_at", &self.requested_at)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum HepaBoardTransitionAction {
    SetPriority { priority: u32 },
    MarkReady,
    Block { reason: String },
    Cancel { reason: String },
    Resume,
    SetDependencies { dependencies: Vec<String> },
}

impl HepaBoardTransitionAction {
    fn validate(&self) -> Result<(), HepaBoardTransitionError> {
        match self {
            Self::SetPriority { .. } | Self::MarkReady | Self::Resume => Ok(()),
            Self::Block { reason } => require_single_line("action.reason", reason),
            Self::Cancel { reason } => require_single_line("action.reason", reason),
            Self::SetDependencies { dependencies } => {
                let mut seen = std::collections::BTreeSet::new();
                for (index, dependency) in dependencies.iter().enumerate() {
                    require_single_line(format!("action.dependencies[{index}]"), dependency)?;
                    if !seen.insert(dependency) {
                        return Err(HepaBoardTransitionError::new(
                            "action.dependencies",
                            "must not contain duplicates",
                        ));
                    }
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaBoardTransitionError {
    pub field: String,
    pub message: String,
}

impl HepaBoardTransitionError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaBoardTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaBoardTransitionError {}

fn require_single_line(
    field: impl Into<String>,
    value: &str,
) -> Result<(), HepaBoardTransitionError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaBoardTransitionError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaBoardTransitionError::new(
            field,
            "must be a single line",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(action: HepaBoardTransitionAction) -> HepaBoardTransitionRequest {
        HepaBoardTransitionRequest {
            request_id: "request-1".to_string(),
            task_id: "task-1".to_string(),
            card_id: Some("hermes-card-1".to_string()),
            requested_by: "operator".to_string(),
            action,
            requested_at: "2026-06-16T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn board_transition_requests_cover_supported_actions() {
        for request in [
            request(HepaBoardTransitionAction::SetPriority { priority: 5 }),
            request(HepaBoardTransitionAction::MarkReady),
            request(HepaBoardTransitionAction::Block {
                reason: "Needs clarification".to_string(),
            }),
            request(HepaBoardTransitionAction::Cancel {
                reason: "Superseded".to_string(),
            }),
            request(HepaBoardTransitionAction::Resume),
            request(HepaBoardTransitionAction::SetDependencies {
                dependencies: vec!["task-0".to_string()],
            }),
        ] {
            request
                .validate()
                .expect("supported requests should validate");
        }
    }

    #[test]
    fn board_transition_requests_use_stable_action_names() {
        let json = serde_json::to_string(&request(HepaBoardTransitionAction::MarkReady))
            .expect("request should serialize");

        assert!(json.contains("\"type\":\"mark_ready\""));
    }

    #[test]
    fn board_transition_requests_reject_duplicate_dependencies() {
        let error = request(HepaBoardTransitionAction::SetDependencies {
            dependencies: vec!["task-0".to_string(), "task-0".to_string()],
        })
        .validate()
        .expect_err("duplicate dependencies must fail");

        assert_eq!(error.field, "action.dependencies");
    }
}
