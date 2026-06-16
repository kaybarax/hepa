pub mod artifacts;
pub mod config;
pub mod contracts;
pub mod lane_state;

pub const CRATE_NAME: &str = "hepa-core";

pub fn crate_name() -> &'static str {
    CRATE_NAME
}
