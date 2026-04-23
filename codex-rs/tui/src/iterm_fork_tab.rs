//! iTerm2 tab launcher for `codex fork`.
//!
//! The TUI uses this module to keep fork behavior non-mutating: it launches
//! `codex fork <thread-id>` in a new iTerm2 tab, then applies the configured
//! tab-focus behavior.
//! When iTerm2 exposes tab ordering, the forked tab is inserted directly after
//! the source tab; otherwise the helper falls back to creating a tab at the
//! default position.

use anyhow::Context;
use anyhow::Result;
use codex_protocol::ThreadId;
use serde::Deserialize;
use serde::Serialize;
use std::env;
use std::process::Stdio;
use tokio::process::Command;

/// Optional interpreter override for the iTerm2 Python helper launcher.
pub(crate) const ITERM_FORK_HELPER_PYTHON_ENV: &str = "CODEX_ITERM2_HELPER_PYTHON";
const ITERM_FORK_TAB_PYTHON_HELPER: &str = r#"
import asyncio
import json
import sys

def emit(status, **kwargs):
    payload = {"status": status}
    payload.update(kwargs)
    print(json.dumps(payload, separators=(",", ":")))

def normalized_session_ids(raw_value):
    if not isinstance(raw_value, str):
        return set()
    value = raw_value.strip()
    if not value:
        return set()
    candidates = {value}
    if ":" in value:
        candidates.add(value.split(":", 1)[1])
    return candidates

def find_source_session(app, source_session_id):
    targets = normalized_session_ids(source_session_id)
    windows = list(getattr(app, "terminal_windows", []) or [])
    for window in windows:
        tabs = list(getattr(window, "tabs", []) or [])
        for tab in tabs:
            sessions = list(getattr(tab, "sessions", []) or [])
            for session in sessions:
                session_id = getattr(session, "session_id", None)
                if targets.intersection(normalized_session_ids(session_id)):
                    return session, tab, window
    return None, None, None

def tab_index(window, target_tab):
    tabs = list(getattr(window, "tabs", []) or [])
    target_tab_id = getattr(target_tab, "tab_id", None)
    for index, candidate in enumerate(tabs):
        candidate_tab_id = getattr(candidate, "tab_id", None)
        if target_tab_id is not None and candidate_tab_id == target_tab_id:
            return index
        if target_tab_id is None and candidate is target_tab:
            return index
    return None

async def create_tab_after_source(window, source_tab, fork_command):
    index = tab_index(window, source_tab)
    if index is None:
        return await window.async_create_tab(command=fork_command)

    try:
        return await window.async_create_tab(command=fork_command, index=index + 1)
    except Exception as indexed_exc:
        try:
            return await window.async_create_tab(command=fork_command)
        except Exception as fallback_exc:
            raise Exception(
                "Failed to create iTerm2 tab after source tab "
                f"(indexed create failed: {indexed_exc}; "
                f"fallback create failed: {fallback_exc})"
            ) from fallback_exc

def session_exists(app, session_id):
    targets = normalized_session_ids(session_id)
    if not targets:
        return False
    windows = list(getattr(app, "terminal_windows", []) or [])
    for window in windows:
        tabs = list(getattr(window, "tabs", []) or [])
        for tab in tabs:
            sessions = list(getattr(tab, "sessions", []) or [])
            for session in sessions:
                candidate = getattr(session, "session_id", None)
                if targets.intersection(normalized_session_ids(candidate)):
                    return True
    return False

async def launch(iterm2_module, connection, payload):
    app = await iterm2_module.async_get_app(connection)
    if app is None:
        return {"status": "unsupported", "reason": "iTerm2 app is unavailable."}

    source_session, source_tab, source_window = find_source_session(
        app,
        payload.get("sourceItermSessionId"),
    )
    if source_session is None or source_tab is None or source_window is None:
        return {
            "status": "failed",
            "reason": "Source iTerm2 session was not found in this window set.",
        }

    fork_command = payload.get("forkCommand")
    if not isinstance(fork_command, str) or not fork_command.strip():
        return {"status": "failed", "reason": "Fork command is missing."}

    try:
        created_tab = await create_tab_after_source(source_window, source_tab, fork_command)
    except Exception as exc:
        return {"status": "failed", "reason": f"Failed to create iTerm2 tab: {exc}"}

    if created_tab is None:
        return {
            "status": "failed",
            "reason": "iTerm2 did not keep the created tab alive.",
        }

    current_session = None
    created_session_id = None
    try:
        current_session = created_tab.current_session
        created_session_id = getattr(current_session, "session_id", None)
    except Exception:
        created_session_id = None

    try:
        open_behavior = payload.get("openBehavior", "foreground")
        if open_behavior == "foreground":
            if current_session is None:
                return {
                    "status": "failed",
                    "reason": "Created tab but could not identify its current session.",
                }
            await current_session.async_activate(select_tab=True, order_window_front=False)
        elif open_behavior == "background":
            # Restore source focus as soon as the tab exists to minimize visible
            # foreground switching when iTerm2 auto-selects new tabs.
            await source_session.async_activate(select_tab=True, order_window_front=False)
        else:
            return {
                "status": "failed",
                "reason": f"Unsupported fork tab open behavior: {open_behavior}",
            }
    except TypeError:
        if open_behavior == "foreground":
            await current_session.async_activate(select_tab=True)
        else:
            await source_session.async_activate(select_tab=True)
    except Exception as exc:
        return {
            "status": "failed",
            "reason": f"Created tab but failed to apply open behavior: {exc}",
        }

    try:
        await asyncio.sleep(0.4)
        refreshed_app = await iterm2_module.async_get_app(connection)
        if refreshed_app is None:
            return {
                "status": "failed",
                "reason": "Created tab but could not refresh iTerm2 app state for diagnostics.",
            }
        if created_session_id is not None and not session_exists(refreshed_app, created_session_id):
            return {
                "status": "failed",
                "reason": (
                    "Created iTerm2 tab session "
                    + created_session_id
                    + " closed immediately after launch; command was: "
                    + fork_command
                ),
            }
    except Exception as exc:
        return {"status": "failed", "reason": f"Tab-launch diagnostics failed: {exc}"}

    return {"status": "launched", "created_session_id": created_session_id}

def main():
    if len(sys.argv) != 2:
        emit("failed", reason="Helper expected one JSON payload argument.")
        return

    try:
        payload = json.loads(sys.argv[1])
    except Exception as exc:
        emit("failed", reason=f"Failed to parse helper payload JSON: {exc}")
        return

    try:
        import iterm2  # type: ignore
    except Exception as exc:
        emit("unsupported", reason=f"python module 'iterm2' is unavailable: {exc}")
        return

    async def runner(connection):
        return await launch(iterm2, connection, payload)

    try:
        result = iterm2.run_until_complete(runner)
    except Exception as exc:
        emit("failed", reason=f"iTerm2 API call failed: {exc}")
        return

    if isinstance(result, dict) and result.get("status") in {"launched", "unsupported", "failed"}:
        print(json.dumps(result, separators=(",", ":")))
    else:
        emit("failed", reason=f"Unexpected helper result: {result!r}")

if __name__ == "__main__":
    main()
"#;

/// Request to open a fork in an iTerm2 tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ItermForkTabRequest {
    /// Source iTerm2 session id (`ITERM_SESSION_ID`) to return focus to.
    pub(crate) source_iterm_session_id: String,
    /// Thread id that should be forked.
    pub(crate) source_thread_id: ThreadId,
    /// Command tokens used to build the launched tab command.
    pub(crate) codex_invocation: Vec<String>,
    /// Exit behavior for the forked tab after Codex exits.
    pub(crate) exit_behavior: ItermForkTabExitBehavior,
    /// Focus behavior after the forked tab opens.
    pub(crate) open_behavior: ItermForkTabOpenBehavior,
}

/// Exit behavior for a forked iTerm2 tab after the Codex process exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ItermForkTabExitBehavior {
    /// Preserve the legacy behavior and let the tab close with the Codex process.
    CloseTab,
    /// Keep the tab open by replacing Codex with the user's login shell.
    ReturnToShell { shell: String },
}

/// Focus behavior for a forked iTerm2 tab after launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ItermForkTabOpenBehavior {
    /// Leave the forked tab selected after it opens.
    Foreground,
    /// Restore focus to the source tab after the forked tab opens.
    Background,
}

/// Result of trying to launch an iTerm2 fork tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ItermForkTabOutcome {
    /// The helper created a tab and started the requested command.
    Launched { created_session_id: Option<String> },
    /// The environment cannot support this integration right now.
    Unsupported(String),
    /// The integration path was attempted but failed.
    Failed(String),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PythonHelperRequest {
    source_iterm_session_id: String,
    source_thread_id: String,
    fork_command: String,
    open_behavior: ItermForkTabOpenBehavior,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum PythonHelperResponse {
    Launched { created_session_id: Option<String> },
    Unsupported { reason: String },
    Failed { reason: String },
}

/// Join command tokens into a shell-escaped command string for iTerm2's
/// `async_create_tab(command=...)` API.
fn codex_command_text(request: &ItermForkTabRequest) -> Result<String> {
    let codex_invocation = &request.codex_invocation;
    if codex_invocation.is_empty() {
        anyhow::bail!("codex invocation cannot be empty");
    }

    let codex_command = shlex::try_join(codex_invocation.iter().map(String::as_str))
        .context("failed to quote codex invocation for iTerm2")?;
    if codex_command.is_empty() {
        anyhow::bail!("codex invocation resolved to an empty command");
    }

    let command = match &request.exit_behavior {
        ItermForkTabExitBehavior::CloseTab => codex_command,
        ItermForkTabExitBehavior::ReturnToShell { shell } => {
            if shell.trim().is_empty() {
                anyhow::bail!("shell path for fork-tab return-to-shell behavior cannot be empty");
            }

            let login_shell_command = shlex::try_join([shell.as_str(), "-l"])
                .context("failed to quote shell path for iTerm2 return-to-shell behavior")?;
            let shell_script = format!("{codex_command}; exec {login_shell_command}");
            shlex::try_join([shell.as_str(), "-lc", shell_script.as_str()])
                .context("failed to quote shell wrapper for iTerm2 return-to-shell behavior")?
        }
    };

    Ok(command)
}

/// Resolve the Python executable used to run the iTerm2 helper snippet.
fn helper_python_executable() -> String {
    if let Some(configured) = env::var(ITERM_FORK_HELPER_PYTHON_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        configured
    } else {
        "python3".to_string()
    }
}

/// Parse the helper process result and preserve helper stderr as context for
/// empty/invalid stdout responses.
fn parse_helper_response(stdout: &[u8], stderr: &[u8]) -> Result<ItermForkTabOutcome> {
    let stderr_text = String::from_utf8_lossy(stderr).trim().to_string();
    let stderr_suffix = if stderr_text.is_empty() {
        String::new()
    } else {
        format!(" (stderr: {stderr_text})")
    };

    let stdout_text = String::from_utf8(stdout.to_vec())
        .context("iTerm2 fork-tab helper stdout is not valid UTF-8")?;
    let trimmed_stdout = stdout_text.trim();
    if trimmed_stdout.is_empty() {
        anyhow::bail!("iTerm2 fork-tab helper returned empty stdout{stderr_suffix}");
    }

    let response: PythonHelperResponse = serde_json::from_str(trimmed_stdout)
        .with_context(|| format!("failed to parse iTerm2 helper JSON: {trimmed_stdout}"))?;
    let outcome = match response {
        PythonHelperResponse::Launched { created_session_id } => {
            ItermForkTabOutcome::Launched { created_session_id }
        }
        PythonHelperResponse::Unsupported { reason } => ItermForkTabOutcome::Unsupported(reason),
        PythonHelperResponse::Failed { reason } => ItermForkTabOutcome::Failed(reason),
    };
    Ok(outcome)
}

/// Launch `codex fork <thread-id>` in a new iTerm2 tab.
///
/// When iTerm2 exposes tab insertion by index, the helper requests a tab
/// immediately after the source tab. If that index cannot be determined or the
/// indexed create call fails, the helper falls back to iTerm2's default tab
/// placement so the fork still opens somewhere in the window. After creation,
/// the helper applies the requested tab-focus behavior.
pub(crate) async fn launch_fork_tab(request: &ItermForkTabRequest) -> Result<ItermForkTabOutcome> {
    let helper_request = PythonHelperRequest {
        source_iterm_session_id: request.source_iterm_session_id.clone(),
        source_thread_id: request.source_thread_id.to_string(),
        fork_command: codex_command_text(request)?,
        open_behavior: request.open_behavior,
    };
    let payload = serde_json::to_string(&helper_request)
        .context("failed to serialize iTerm2 fork-tab helper payload")?;
    let helper_python = helper_python_executable();

    let output = Command::new(&helper_python)
        .arg("-c")
        .arg(ITERM_FORK_TAB_PYTHON_HELPER)
        .arg(payload)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| {
            format!(
                "failed to run iTerm2 fork-tab helper via {} (env {}={:?})",
                helper_python,
                ITERM_FORK_HELPER_PYTHON_ENV,
                env::var(ITERM_FORK_HELPER_PYTHON_ENV).ok()
            )
        })?;

    if !output.status.success() {
        let stderr_text = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let reason = if stderr_text.is_empty() {
            format!(
                "iTerm2 fork-tab helper exited with status {}",
                output.status
            )
        } else {
            format!(
                "iTerm2 fork-tab helper exited with status {}: {stderr_text}",
                output.status
            )
        };
        return Ok(ItermForkTabOutcome::Failed(reason));
    }

    parse_helper_response(&output.stdout, &output.stderr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::Value;
    use serde_json::json;
    use std::process::Command as ProcessCommand;

    fn run_python_helper_test(test_body: &str) -> Result<Value> {
        let script = format!(
            r#"
import asyncio
import json

HELPER = {ITERM_FORK_TAB_PYTHON_HELPER:?}
namespace = {{"__name__": "embedded_helper"}}
exec(HELPER, namespace)

async def immediate_sleep(_duration):
    return None

namespace["asyncio"].sleep = immediate_sleep

class FakeSession:
    def __init__(self, session_id):
        self.session_id = session_id
        self.activation_calls = []

    async def async_activate(self, select_tab=True, order_window_front=False):
        self.activation_calls.append(
            {{
                "select_tab": select_tab,
                "order_window_front": order_window_front,
            }}
        )

class FakeTab:
    def __init__(self, tab_id, sessions):
        self.tab_id = tab_id
        self.sessions = sessions
        self.current_session = sessions[0] if sessions else None

class FakeWindow:
    def __init__(self, tabs, fail_indexed=False, fail_fallback=False):
        self.tabs = list(tabs)
        self.fail_indexed = fail_indexed
        self.fail_fallback = fail_fallback
        self.created = []

    async def async_create_tab(self, command=None, index=None):
        self.created.append({{"command": command, "index": index}})
        if index is not None and self.fail_indexed:
            raise Exception("indexed create failed")
        if index is None and self.fail_fallback:
            raise Exception("fallback create failed")

        created_session = FakeSession("created-session")
        created_tab = FakeTab("created-tab", [created_session])
        if index is None or index > len(self.tabs):
            self.tabs.append(created_tab)
        else:
            self.tabs.insert(index, created_tab)
        return created_tab

class FakeApp:
    def __init__(self, windows):
        self.terminal_windows = windows

class FakeIterm2Module:
    def __init__(self, app):
        self.app = app

    async def async_get_app(self, _connection):
        return self.app

{test_body}
"#,
        );

        let output = ProcessCommand::new("python3")
            .arg("-c")
            .arg(script)
            .output()
            .context("failed to run python3 for iTerm2 helper test")?;
        let stdout = String::from_utf8(output.stdout)
            .context("python helper test stdout is not valid UTF-8")?;
        let stderr = String::from_utf8(output.stderr)
            .context("python helper test stderr is not valid UTF-8")?;
        assert!(
            output.status.success(),
            "python helper test failed with status {} stdout={stdout:?} stderr={stderr:?}",
            output.status
        );
        let trimmed_stdout = stdout.trim();
        let value = serde_json::from_str(trimmed_stdout).with_context(|| {
            format!("failed to parse python helper test JSON: {trimmed_stdout}")
        })?;
        Ok(value)
    }

    #[test]
    // Verifies: command assembly shell-quotes tokens that contain spaces.
    // Catches: malformed fork commands where thread ids with spaces break argv boundaries.
    fn codex_command_text_quotes_spaces() -> Result<()> {
        let command = codex_command_text(&ItermForkTabRequest {
            source_iterm_session_id: "session-1".to_string(),
            source_thread_id: ThreadId::new(),
            codex_invocation: vec![
                "codex".to_string(),
                "fork".to_string(),
                "thread with spaces".to_string(),
            ],
            exit_behavior: ItermForkTabExitBehavior::CloseTab,
            open_behavior: ItermForkTabOpenBehavior::Foreground,
        })?;
        assert_eq!(command, "codex fork 'thread with spaces'");
        Ok(())
    }

    #[test]
    // Verifies: env-style argv tokens with spaces are shell-quoted as one token.
    // Catches: malformed fork launches where snapshot-path assignments split at spaces.
    fn codex_command_text_quotes_env_assignment_with_spaces() -> Result<()> {
        let command = codex_command_text(&ItermForkTabRequest {
            source_iterm_session_id: "session-1".to_string(),
            source_thread_id: ThreadId::new(),
            codex_invocation: vec![
                "/usr/bin/env".to_string(),
                "CODEX_FORK_ENV_SNAPSHOT_PATH=/tmp/fork env snapshot".to_string(),
                "codex".to_string(),
                "fork".to_string(),
                "thread-1".to_string(),
            ],
            exit_behavior: ItermForkTabExitBehavior::CloseTab,
            open_behavior: ItermForkTabOpenBehavior::Foreground,
        })?;
        assert_eq!(
            command,
            "/usr/bin/env 'CODEX_FORK_ENV_SNAPSHOT_PATH=/tmp/fork env snapshot' codex fork thread-1"
        );
        Ok(())
    }

    #[test]
    // Verifies: keep-open mode runs the fork inside an explicit shell wrapper
    // so iTerm2 executes a real shell command rather than raw shell syntax.
    // Catches: regressions that pass `; exec ...` directly to iTerm2's custom
    // command field without an intervening shell process.
    fn codex_command_text_wraps_fork_in_explicit_shell_command() -> Result<()> {
        let command = codex_command_text(&ItermForkTabRequest {
            source_iterm_session_id: "session-1".to_string(),
            source_thread_id: ThreadId::new(),
            codex_invocation: vec![
                "/usr/bin/env".to_string(),
                "CODEX_ITERM2_HELPER_PYTHON=/opt/homebrew/bin/python3".to_string(),
                "CODEX_FORK_ENV_SNAPSHOT_PATH=/tmp/fork env snapshot".to_string(),
                "codex".to_string(),
                "fork".to_string(),
                "thread-1".to_string(),
            ],
            exit_behavior: ItermForkTabExitBehavior::ReturnToShell {
                shell: "/opt/homebrew/bin/fish".to_string(),
            },
            open_behavior: ItermForkTabOpenBehavior::Foreground,
        })?;
        assert_eq!(
            command,
            "/opt/homebrew/bin/fish -lc \"/usr/bin/env 'CODEX_ITERM2_HELPER_PYTHON=/opt/homebrew/bin/python3' 'CODEX_FORK_ENV_SNAPSHOT_PATH=/tmp/fork env snapshot' codex fork thread-1; exec /opt/homebrew/bin/fish -l\""
        );
        Ok(())
    }

    #[test]
    // Verifies: helper status "launched" maps to the launched outcome with session id passthrough.
    // Catches: regressions that drop created-session metadata on successful launches.
    fn parse_helper_response_maps_launched_status() -> Result<()> {
        let outcome = parse_helper_response(
            br#"{"status":"launched","created_session_id":"w0t1p0"}"#,
            b"",
        )?;
        assert_eq!(
            outcome,
            ItermForkTabOutcome::Launched {
                created_session_id: Some("w0t1p0".to_string())
            }
        );
        Ok(())
    }

    #[test]
    // Verifies: helper status "unsupported" maps to a user-facing unsupported outcome.
    // Catches: regressions that misclassify missing iTerm2 Python bindings as generic failures.
    fn parse_helper_response_maps_unsupported_status() -> Result<()> {
        let outcome = parse_helper_response(
            br#"{"status":"unsupported","reason":"python module 'iterm2' is unavailable"}"#,
            b"",
        )?;
        assert_eq!(
            outcome,
            ItermForkTabOutcome::Unsupported("python module 'iterm2' is unavailable".to_string())
        );
        Ok(())
    }

    #[test]
    // Verifies: helper status "failed" maps to a failed outcome with unchanged reason text.
    // Catches: regressions that hide actionable launch errors returned by the helper.
    fn parse_helper_response_maps_failed_status() -> Result<()> {
        let outcome = parse_helper_response(
            br#"{"status":"failed","reason":"Failed to create iTerm2 tab"}"#,
            b"",
        )?;
        assert_eq!(
            outcome,
            ItermForkTabOutcome::Failed("Failed to create iTerm2 tab".to_string())
        );
        Ok(())
    }

    #[test]
    // Verifies: the embedded helper requests the slot immediately after the
    // source tab when the tab index is available.
    // Catches: regressions that silently fall back to appending even when the
    // source tab position is known.
    fn python_helper_inserts_after_source_tab_when_index_is_available() -> Result<()> {
        let value = run_python_helper_test(
            r#"
source_session = FakeSession("session-1")
source_tab = FakeTab("tab-1", [source_session])
window = FakeWindow([source_tab, FakeTab("tab-2", [FakeSession("session-2")])])
app = FakeApp([window])
result = asyncio.run(
    namespace["launch"](
        FakeIterm2Module(app),
        None,
        {
            "sourceItermSessionId": "session-1",
            "sourceThreadId": "thread-1",
            "forkCommand": "codex fork thread-1",
        },
    )
)
created_session = window.tabs[1].current_session
print(
    json.dumps(
        {
            "result": result,
            "created": window.created,
            "source_activation_calls": source_session.activation_calls,
            "created_activation_calls": created_session.activation_calls,
        },
        separators=(",", ":"),
    )
)
"#,
        )?;
        assert_eq!(
            value,
            json!({
                "result": {
                    "status": "launched",
                    "created_session_id": "created-session",
                },
                "created": [
                    {
                        "command": "codex fork thread-1",
                        "index": 1,
                    }
                ],
                "source_activation_calls": [],
                "created_activation_calls": [
                    {
                        "select_tab": true,
                        "order_window_front": false,
                    }
                ],
            })
        );
        Ok(())
    }

    #[test]
    // Verifies: background open behavior restores focus to the source tab.
    // Catches: regressions that ignore the config-requested background launch mode.
    fn python_helper_restores_source_focus_for_background_open_behavior() -> Result<()> {
        let value = run_python_helper_test(
            r#"
source_session = FakeSession("session-1")
source_tab = FakeTab("tab-1", [source_session])
window = FakeWindow([source_tab])
app = FakeApp([window])
result = asyncio.run(
    namespace["launch"](
        FakeIterm2Module(app),
        None,
        {
            "sourceItermSessionId": "session-1",
            "sourceThreadId": "thread-1",
            "forkCommand": "codex fork thread-1",
            "openBehavior": "background",
        },
    )
)
created_session = window.tabs[1].current_session
print(
    json.dumps(
        {
            "result": result,
            "source_activation_calls": source_session.activation_calls,
            "created_activation_calls": created_session.activation_calls,
        },
        separators=(",", ":"),
    )
)
"#,
        )?;
        assert_eq!(
            value,
            json!({
                "result": {
                    "status": "launched",
                    "created_session_id": "created-session",
                },
                "source_activation_calls": [
                    {
                        "select_tab": true,
                        "order_window_front": false,
                    }
                ],
                "created_activation_calls": [],
            })
        );
        Ok(())
    }

    #[test]
    // Verifies: when the source tab position cannot be derived, the helper
    // still creates a tab using iTerm2's default placement.
    // Catches: regressions that fail the fork instead of degrading to a
    // best-effort launch when tab ordering metadata is unavailable.
    fn python_helper_falls_back_to_default_placement_when_index_is_unavailable() -> Result<()> {
        let value = run_python_helper_test(
            r#"
window = FakeWindow([FakeTab("tab-1", [FakeSession("session-1")])])
created_tab = asyncio.run(
    namespace["create_tab_after_source"](
        window,
        FakeTab("detached-tab", [FakeSession("detached-session")]),
        "codex fork thread-1",
    )
)
print(
    json.dumps(
        {
            "created": window.created,
            "created_session_id": created_tab.current_session.session_id,
        },
        separators=(",", ":"),
    )
)
"#,
        )?;
        assert_eq!(
            value,
            json!({
                "created": [
                    {
                        "command": "codex fork thread-1",
                        "index": null,
                    }
                ],
                "created_session_id": "created-session",
            })
        );
        Ok(())
    }

    #[test]
    // Verifies: an indexed tab-create failure retries without an index so the
    // fork still opens in the window.
    // Catches: regressions that surface index-related create failures to the
    // user even though a default-position retry would succeed.
    fn python_helper_retries_without_index_when_indexed_create_fails() -> Result<()> {
        let value = run_python_helper_test(
            r#"
source_session = FakeSession("session-1")
source_tab = FakeTab("tab-1", [source_session])
window = FakeWindow(
    [source_tab, FakeTab("tab-2", [FakeSession("session-2")])],
    fail_indexed=True,
)
app = FakeApp([window])
result = asyncio.run(
    namespace["launch"](
        FakeIterm2Module(app),
        None,
        {
            "sourceItermSessionId": "session-1",
            "sourceThreadId": "thread-1",
            "forkCommand": "codex fork thread-1",
        },
    )
)
print(json.dumps({"result": result, "created": window.created}, separators=(",", ":")))
"#,
        )?;
        assert_eq!(
            value,
            json!({
                "result": {
                    "status": "launched",
                    "created_session_id": "created-session",
                },
                "created": [
                    {
                        "command": "codex fork thread-1",
                        "index": 1,
                    },
                    {
                        "command": "codex fork thread-1",
                        "index": null,
                    },
                ],
            })
        );
        Ok(())
    }
}
