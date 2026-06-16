pub mod builtin;
pub mod engine;
pub mod fake;
pub mod registry;
pub mod routing;
pub mod spec;

pub const CRATE_NAME: &str = "hepa-adapters";

pub fn crate_name() -> &'static str {
    CRATE_NAME
}
