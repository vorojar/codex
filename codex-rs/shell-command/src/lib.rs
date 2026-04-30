//! Command parsing and safety utilities shared across Codex crates.

mod shell_detect;

pub mod bash;
pub(crate) mod command_safety;
pub mod parse_command;
pub mod powershell;

pub use command_safety::is_dangerous_command;
pub use command_safety::is_safe_command;
pub use command_safety::windows_safe_commands::split_powershell_command_sequence;
