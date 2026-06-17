pub mod arbitration;
pub mod fanout;
pub mod parser;
pub mod repair;

pub const CRATE_NAME: &str = "hepa-review";

pub fn crate_name() -> &'static str {
    CRATE_NAME
}
