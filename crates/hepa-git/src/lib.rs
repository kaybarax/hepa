pub mod branches;
pub mod worktree;

pub const CRATE_NAME: &str = "hepa-git";

pub fn crate_name() -> &'static str {
    CRATE_NAME
}
