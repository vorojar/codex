/// The current Codex CLI version as embedded at compile time.
pub const CODEX_CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) use codex_utils_cli::version::CODEX_CLI_DISPLAY_VERSION;
