/// Git revision embedded into the CLI binary at build time.
pub const CODEX_CLI_BUILD_GIT_SHA: &str = env!("CODEX_BUILD_GIT_SHA");

/// Version text for user-facing CLI version displays.
///
/// This matches `codex --version`, which reports the build Git SHA for current
/// in-repo builds instead of the workspace package version.
pub const CODEX_CLI_DISPLAY_VERSION: &str = CODEX_CLI_BUILD_GIT_SHA;
