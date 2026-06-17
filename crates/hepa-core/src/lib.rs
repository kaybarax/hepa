pub mod artifacts;
pub mod config;
pub mod contracts;
pub mod fleet_registry;
pub mod lane_state;
pub mod monitor;
pub mod notifications;
pub mod readiness;
pub mod scheduler;

pub const CRATE_NAME: &str = "hepa-core";

pub fn crate_name() -> &'static str {
    CRATE_NAME
}
