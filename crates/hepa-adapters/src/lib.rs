pub mod builtin;
pub mod container;
pub mod custom;
pub mod doctor;
pub mod engine;
pub mod external_worker;
pub mod fake;
pub mod interactive;
pub mod local_worker;
pub mod pi;
pub mod registry;
pub mod routing;
pub mod shell_command;
pub mod spec;
pub mod user_reviewer;
pub mod user_worker;
pub mod version_pinning;

pub const CRATE_NAME: &str = "hepa-adapters";

pub fn crate_name() -> &'static str {
    CRATE_NAME
}

#[cfg(test)]
mod configured_adapter_fake_bins;
