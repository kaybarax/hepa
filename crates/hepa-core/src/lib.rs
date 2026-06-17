pub mod artifacts;
pub mod config;
pub mod conflict_planner;
pub mod contracts;
pub mod env_allowlist;
pub mod fleet_monitor;
pub mod fleet_registry;
pub mod hard_blockers;
pub mod lane_state;
pub mod monitor;
pub mod notifications;
pub mod readiness;
pub mod redaction;
pub mod resource_governor;
pub mod scheduler;

pub const CRATE_NAME: &str = "hepa-core";

pub fn crate_name() -> &'static str {
    CRATE_NAME
}
