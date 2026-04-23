//! iTerm fork-tab launch flow for the TUI app.

use super::*;
use crate::app_event::ForkTriggerSource;
use crate::iterm_fork_tab::ITERM_FORK_HELPER_PYTHON_ENV;
use crate::iterm_fork_tab::ItermForkTabExitBehavior;
use crate::iterm_fork_tab::ItermForkTabOpenBehavior;
use crate::iterm_fork_tab::ItermForkTabOutcome;
use crate::iterm_fork_tab::ItermForkTabRequest;
use crate::iterm_fork_tab::launch_fork_tab;
use codex_arg0::FORK_ENV_SNAPSHOT_PATH_ENV_VAR;
use codex_arg0::ForkEnvSnapshotFile;
use codex_config::types::ForkTabExitBehavior;
use codex_config::types::ForkTabOpenBehavior;
use codex_terminal_detection::TerminalName;
use codex_terminal_detection::terminal_info;
use std::env;

const ENV_BINARY: &str = "/usr/bin/env";

impl App {
    /// Launch the current persisted thread in a sibling iTerm2 tab.
    ///
    /// This path validates that the thread is persisted and that iTerm2 session identity is
    /// available before launching. If any precondition fails, the source tab receives an
    /// actionable error and no external terminal action is attempted.
    pub(super) async fn handle_fork_current_session_in_iterm_tab(
        &mut self,
        tui: &mut tui::Tui,
        source: ForkTriggerSource,
    ) -> Result<()> {
        self.session_telemetry.counter(
            "codex.thread.fork",
            /*inc*/ 1,
            &[("source", source.telemetry_source())],
        );

        if matches!(source, ForkTriggerSource::SlashCommand) {
            self.chat_widget
                .add_plain_history_lines(vec![source.trigger_label().magenta().into()]);
        }

        let missing_turn_message =
            "A thread must contain at least one turn before it can be forked.".to_string();
        let source_thread_id = self.chat_widget.thread_id();
        let rollout_path = self.chat_widget.rollout_path();
        let source_iterm_session_id = env::var("ITERM_SESSION_ID")
            .ok()
            .map(|session_id| session_id.trim().to_string())
            .filter(|session_id| !session_id.is_empty());

        let launch_precondition_error = if terminal_info().name != TerminalName::Iterm2 {
            Some("Fork tab launch requires iTerm2.".to_string())
        } else if source_iterm_session_id.is_none() {
            Some("ITERM_SESSION_ID is missing.".to_string())
        } else if source_thread_id.is_none() {
            Some(missing_turn_message.clone())
        } else if rollout_path.as_ref().is_none_or(|path| !path.exists()) {
            Some(missing_turn_message)
        } else {
            None
        };

        if let Some(message) = launch_precondition_error {
            self.chat_widget.add_error_message(message);
            tui.frame_requester().schedule_frame();
            return Ok(());
        }

        if let (Some(source_thread_id), Some(source_iterm_session_id)) =
            (source_thread_id, source_iterm_session_id)
        {
            let fork_env_snapshot = match ForkEnvSnapshotFile::create_for_current_process(
                self.config.codex_home.as_path(),
            ) {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    self.chat_widget.add_error_message(format!(
                        "Failed to snapshot the current Codex environment for fork launch: {err}"
                    ));
                    tui.frame_requester().schedule_frame();
                    return Ok(());
                }
            };
            let codex_executable = match env::current_exe() {
                Ok(path) => {
                    let path_text = path.to_string_lossy().trim().to_string();
                    if path_text.is_empty() {
                        "codex".to_string()
                    } else {
                        path_text
                    }
                }
                Err(_) => "codex".to_string(),
            };
            let helper_python = helper_python_for_fork_launch();
            let exit_behavior = match self.config.tui_fork_tab_exit_behavior {
                ForkTabExitBehavior::CloseTab => ItermForkTabExitBehavior::CloseTab,
                ForkTabExitBehavior::ReturnToShell => {
                    let shell = env::var("SHELL")
                        .ok()
                        .and_then(trim_non_empty)
                        .unwrap_or("/bin/sh".to_string());
                    ItermForkTabExitBehavior::ReturnToShell { shell }
                }
            };
            let open_behavior = match self.config.tui_fork_tab_open_behavior {
                ForkTabOpenBehavior::Foreground => ItermForkTabOpenBehavior::Foreground,
                ForkTabOpenBehavior::Background => ItermForkTabOpenBehavior::Background,
            };
            let fork_env_snapshot_path = match fork_env_snapshot.path() {
                Ok(path) => path.to_string_lossy().to_string(),
                Err(err) => {
                    self.chat_widget.add_error_message(format!(
                        "Failed to resolve the fork environment snapshot path: {err}"
                    ));
                    tui.frame_requester().schedule_frame();
                    return Ok(());
                }
            };
            let request = ItermForkTabRequest {
                source_iterm_session_id,
                source_thread_id,
                codex_invocation: codex_fork_invocation(
                    codex_executable,
                    &source_thread_id,
                    helper_python,
                    fork_env_snapshot_path,
                ),
                exit_behavior,
                open_behavior,
            };

            let launch_outcome = launch_fork_tab(&request).await;
            match launch_outcome {
                Ok(ItermForkTabOutcome::Launched { .. }) => {
                    if let Err(err) = fork_env_snapshot.persist_for_child() {
                        self.chat_widget.add_error_message(format!(
                            "Fork tab launched but failed to persist its environment snapshot: {err}"
                        ));
                    }
                }
                Ok(ItermForkTabOutcome::Unsupported(reason))
                | Ok(ItermForkTabOutcome::Failed(reason)) => {
                    self.chat_widget.add_error_message(reason);
                }
                Err(err) => {
                    self.chat_widget
                        .add_error_message(format!("Failed to launch iTerm2 fork tab: {err}"));
                }
            }
        }

        tui.frame_requester().schedule_frame();
        Ok(())
    }

    pub(super) fn maybe_dispatch_fork_hotkey(&mut self, key_event: KeyEvent) -> bool {
        if !matches!(
            key_event,
            KeyEvent {
                code: KeyCode::Char('o'),
                modifiers: KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            }
        ) {
            return false;
        }

        if !self.chat_widget.can_run_ctrl_o_fork_now() {
            return true;
        }

        self.app_event_tx
            .send(AppEvent::ForkCurrentSessionInItermTab(
                ForkTriggerSource::Hotkey,
            ));
        true
    }
}

fn trim_non_empty(value: String) -> Option<String> {
    let trimmed = value.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn find_python3_in_path(path_var: Option<std::ffi::OsString>) -> Option<String> {
    let path_var = path_var?;
    env::split_paths(&path_var)
        .map(|entry| entry.join("python3"))
        .find_map(|candidate| {
            if candidate.is_file() {
                trim_non_empty(candidate.to_string_lossy().to_string())
            } else {
                None
            }
        })
}

/// Select the Python interpreter used by iTerm2 fork-tab helper launches.
fn helper_python_for_fork_launch() -> String {
    if let Some(configured) = env::var(ITERM_FORK_HELPER_PYTHON_ENV)
        .ok()
        .and_then(trim_non_empty)
    {
        configured
    } else if let Some(path_python3) = find_python3_in_path(env::var_os("PATH")) {
        path_python3
    } else {
        "python3".to_string()
    }
}

/// Build the fork command tokens with helper-python pinning for descendant forks.
fn codex_fork_invocation(
    codex_executable: String,
    source_thread_id: &ThreadId,
    helper_python: String,
    fork_env_snapshot_path: String,
) -> Vec<String> {
    vec![
        ENV_BINARY.to_string(),
        format!("{ITERM_FORK_HELPER_PYTHON_ENV}={helper_python}"),
        format!("{FORK_ENV_SNAPSHOT_PATH_ENV_VAR}={fork_env_snapshot_path}"),
        codex_executable,
        "fork".to_string(),
        source_thread_id.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn codex_fork_invocation_includes_helper_and_snapshot_env_assignments() {
        let thread_id = ThreadId::new();

        let invocation = codex_fork_invocation(
            "/tmp/codex".to_string(),
            &thread_id,
            "/opt/homebrew/bin/python3".to_string(),
            "/tmp/fork env snapshot".to_string(),
        );

        assert_eq!(
            invocation,
            vec![
                "/usr/bin/env".to_string(),
                "CODEX_ITERM2_HELPER_PYTHON=/opt/homebrew/bin/python3".to_string(),
                "CODEX_FORK_ENV_SNAPSHOT_PATH=/tmp/fork env snapshot".to_string(),
                "/tmp/codex".to_string(),
                "fork".to_string(),
                thread_id.to_string(),
            ]
        );
    }
}
