pub mod card_mapping;
pub mod doctor;
pub mod sync;
pub mod transition;

pub const CRATE_NAME: &str = "hepa-kanban";

pub fn crate_name() -> &'static str {
    CRATE_NAME
}
