use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaManagerBranch {
    name: String,
}

impl HepaManagerBranch {
    pub fn for_lane(lane_id: impl Into<String>) -> Result<Self, HepaBranchError> {
        let lane_id = lane_id.into();
        validate_lane_id(&lane_id)?;
        Ok(Self {
            name: format!("hepa/manager/{lane_id}"),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaBranchError {
    pub field: String,
    pub message: String,
}

impl HepaBranchError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaBranchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaBranchError {}

fn validate_lane_id(lane_id: &str) -> Result<(), HepaBranchError> {
    if lane_id.trim().is_empty() {
        return Err(HepaBranchError::new("lane_id", "must not be empty"));
    }
    if lane_id.contains('\n') || lane_id.contains('\r') {
        return Err(HepaBranchError::new("lane_id", "must be a single line"));
    }
    if lane_id == "." || lane_id == ".." {
        return Err(HepaBranchError::new(
            "lane_id",
            "must not be a relative path segment",
        ));
    }
    if lane_id.contains('/') || lane_id.contains('\\') || lane_id.contains("..") {
        return Err(HepaBranchError::new(
            "lane_id",
            "must not contain path traversal characters",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_branch_names_are_lane_scoped() {
        let branch = HepaManagerBranch::for_lane("lane-a").expect("valid lane");

        assert_eq!(branch.as_str(), "hepa/manager/lane-a");
    }

    #[test]
    fn manager_branch_names_reject_path_like_lane_ids() {
        assert!(HepaManagerBranch::for_lane("../lane").is_err());
        assert!(HepaManagerBranch::for_lane("lane/a").is_err());
        assert!(HepaManagerBranch::for_lane("lane\na").is_err());
    }
}
