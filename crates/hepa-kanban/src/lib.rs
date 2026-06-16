pub mod card_mapping;
pub mod doctor;
pub mod spec_import;
pub mod sync;
pub mod transition;

pub const CRATE_NAME: &str = "hepa-kanban";

pub fn crate_name() -> &'static str {
    CRATE_NAME
}
