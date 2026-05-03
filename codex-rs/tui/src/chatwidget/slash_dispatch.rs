//! Slash-command dispatch and local-recall handoff for `ChatWidget`.
//!
//! `ChatComposer` parses slash input and stages recognized command text for local
//! Up-arrow recall before returning an input result. This module owns the app-level
//! dispatch step and records the staged entry once the command has been handled, so
//! slash-command recall follows the same submitted-input rule as ordinary text.

use super::service_tiers::available_service_tier_commands;
use super::*;
use crate::app_event::ThreadGoalSetMode;
use crate::bottom_pane::prompt_args::parse_slash_name;
use crate::bottom_pane::slash_commands;
use crate::bottom_pane::slash_commands::SlashCommandAction;
use crate::bottom_pane::slash_commands::command_as_builtin;
use crate::bottom_pane::slash_commands::command_available_during_task;
use crate::bottom_pane::slash_commands::command_available_in_side_conversation;
use crate::bottom_pane::slash_commands::command_name;
use crate::bottom_pane::slash_commands::command_supports_inline_args;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlashCommandDispatchSource {
    Live,
    Queued,
}

struct PreparedSlashCommandArgs {
    args: String,
    text_elements: Vec<TextElement>,
    local_images: Vec<LocalImageAttachment>,
    remote_image_urls: Vec<String>,
    mention_bindings: Vec<MentionBinding>,
    source: SlashCommandDispatchSource,
}

const SIDE_STARTING_CONTEXT_LABEL: &str = "Side starting...";
const SIDE_REVIEW_UNAVAILABLE_MESSAGE: &str =
    "'/side' is unavailable while code review is running.";
const SIDE_SLASH_COMMAND_UNAVAILABLE_HINT: &str = "Press Esc to return to the main thread first.";
const GOAL_USAGE: &str = "Usage: /goal <objective>";
const GOAL_USAGE_HINT: &str = "Example: /goal improve benchmark coverage";

impl ChatWidget {
    /// Dispatch a bare slash command and record its staged local-history entry.
    ///
    /// The composer stages history before returning `InputResult::Command`; this wrapper commits
    /// that staged entry after dispatch so slash-command recall follows the same "submitted input"
    /// rule as normal text.
    pub(super) fn handle_slash_command_dispatch(&mut self, cmd: SlashCommandAction) {
        let is_goal = command_as_builtin(&cmd) == Some(SlashCommand::Goal);
        self.dispatch_command(cmd);
        if is_goal {
            self.bottom_pane.drain_pending_submission_state();
        }
        self.bottom_pane.record_pending_slash_command_history();
    }

    /// Dispatch an inline slash command and record its staged local-history entry.
    ///
    /// Inline command arguments may later be prepared through the normal submission pipeline, but
    /// local command recall still tracks the original command invocation. Treating this wrapper as
    /// the only input-result entry point avoids double-recording commands with inline args.
    pub(super) fn handle_slash_command_with_args_dispatch(
        &mut self,
        cmd: SlashCommandAction,
        args: String,
        text_elements: Vec<TextElement>,
    ) {
        self.dispatch_command_with_args(cmd, args, text_elements);
        self.bottom_pane.record_pending_slash_command_history();
    }

    fn apply_plan_slash_command(&mut self) -> bool {
        if !self.collaboration_modes_enabled() {
            self.add_info_message(
                "Collaboration modes are disabled.".to_string(),
                Some("Enable collaboration modes to use /plan.".to_string()),
            );
            return false;
        }
        if let Some(mask) = collaboration_modes::plan_mask(self.model_catalog.as_ref()) {
            self.set_collaboration_mask(mask);
            true
        } else {
            self.add_info_message(
                "Plan mode unavailable right now.".to_string(),
                /*hint*/ None,
            );
            false
        }
    }

    fn request_side_conversation(
        &mut self,
        parent_thread_id: ThreadId,
        user_message: Option<UserMessage>,
    ) {
        self.set_side_conversation_context_label(Some(SIDE_STARTING_CONTEXT_LABEL.to_string()));
        self.request_redraw();
        self.app_event_tx.send(AppEvent::StartSide {
            parent_thread_id,
            user_message,
        });
    }

    fn request_empty_side_conversation(&mut self) {
        let Some(parent_thread_id) = self.thread_id else {
            self.add_error_message("'/side' is unavailable before the session starts.".to_string());
            return;
        };

        self.request_side_conversation(parent_thread_id, /*user_message*/ None);
    }

    pub(super) fn dispatch_command(&mut self, cmd: impl Into<SlashCommandAction>) {
        let cmd = cmd.into();
        if !self.ensure_slash_command_allowed_in_side_conversation(&cmd) {
            return;
        }
        if !self.ensure_side_command_allowed_outside_review(&cmd) {
            return;
        }
        if !command_available_during_task(&cmd) && self.bottom_pane.is_task_running() {
            let message = format!(
                "'/{}' is disabled while a task is in progress.",
                command_name(&cmd)
            );
            self.add_to_history(history_cell::new_error_event(message));
            self.bottom_pane.drain_pending_submission_state();
            self.request_redraw();
            return;
        }

        match cmd {
            SlashCommandAction::ServiceTier(command) => {
                let next_tier =
                    if self.current_service_tier().as_ref() == Some(&command.service_tier) {
                        None
                    } else {
                        Some(command.service_tier)
                    };
                self.set_service_tier_selection(next_tier);
            }
            SlashCommandAction::Builtin(SlashCommand::Feedback) => {
                if !self.config.feedback_enabled {
                    let params = crate::bottom_pane::feedback_disabled_params();
                    self.bottom_pane.show_selection_view(params);
                    self.request_redraw();
                    return;
                }
                let params =
                    crate::bottom_pane::feedback_selection_params(self.app_event_tx.clone());
                self.bottom_pane.show_selection_view(params);
                self.request_redraw();
            }
            SlashCommandAction::Builtin(SlashCommand::New) => {
                self.app_event_tx.send(AppEvent::NewSession);
            }
            SlashCommandAction::Builtin(SlashCommand::Clear) => {
                self.app_event_tx.send(AppEvent::ClearUi);
            }
            SlashCommandAction::Builtin(SlashCommand::Resume) => {
                self.app_event_tx.send(AppEvent::OpenResumePicker);
            }
            SlashCommandAction::Builtin(SlashCommand::Fork) => {
                self.app_event_tx.send(AppEvent::ForkCurrentSession);
            }
            SlashCommandAction::Builtin(SlashCommand::Init) => {
                let init_target = self.config.cwd.join(DEFAULT_AGENTS_MD_FILENAME);
                if init_target.exists() {
                    let message = format!(
                        "{DEFAULT_AGENTS_MD_FILENAME} already exists here. Skipping /init to avoid overwriting it."
                    );
                    self.add_info_message(message, /*hint*/ None);
                    return;
                }
                const INIT_PROMPT: &str = include_str!("../../prompt_for_init_command.md");
                self.submit_user_message(INIT_PROMPT.to_string().into());
            }
            SlashCommandAction::Builtin(SlashCommand::Compact) => {
                self.clear_token_usage();
                if !self.bottom_pane.is_task_running() {
                    self.bottom_pane.set_task_running(/*running*/ true);
                }
                self.app_event_tx.compact();
            }
            SlashCommandAction::Builtin(SlashCommand::Review) => {
                self.open_review_popup();
            }
            SlashCommandAction::Builtin(SlashCommand::Rename) => {
                self.session_telemetry
                    .counter("codex.thread.rename", /*inc*/ 1, &[]);
                self.show_rename_prompt();
            }
            SlashCommandAction::Builtin(SlashCommand::Model) => {
                self.open_model_popup();
            }
            SlashCommandAction::Builtin(SlashCommand::Realtime) => {
                if !self.realtime_conversation_enabled() {
                    return;
                }
                if self.realtime_conversation.is_live() {
                    self.stop_realtime_conversation_from_ui();
                } else {
                    self.start_realtime_conversation();
                }
            }
            SlashCommandAction::Builtin(SlashCommand::Settings) => {
                if !self.realtime_audio_device_selection_enabled() {
                    return;
                }
                self.open_realtime_audio_popup();
            }
            SlashCommandAction::Builtin(SlashCommand::Personality) => {
                self.open_personality_popup();
            }
            SlashCommandAction::Builtin(SlashCommand::Plan) => {
                self.apply_plan_slash_command();
            }
            SlashCommandAction::Builtin(SlashCommand::Goal) => {
                if !self.config.features.enabled(Feature::Goals) {
                    return;
                }
                if let Some(thread_id) = self.thread_id {
                    self.app_event_tx
                        .send(AppEvent::OpenThreadGoalMenu { thread_id });
                } else {
                    self.add_info_message(
                        GOAL_USAGE.to_string(),
                        Some(GOAL_USAGE_HINT.to_string()),
                    );
                }
            }
            SlashCommandAction::Builtin(SlashCommand::Collab) => {
                if !self.collaboration_modes_enabled() {
                    self.add_info_message(
                        "Collaboration modes are disabled.".to_string(),
                        Some("Enable collaboration modes to use /collab.".to_string()),
                    );
                    return;
                }
                self.open_collaboration_modes_popup();
            }
            SlashCommandAction::Builtin(SlashCommand::Side) => {
                self.request_empty_side_conversation();
            }
            SlashCommandAction::Builtin(SlashCommand::Agent)
            | SlashCommandAction::Builtin(SlashCommand::MultiAgents) => {
                self.app_event_tx.send(AppEvent::OpenAgentPicker);
            }
            SlashCommandAction::Builtin(SlashCommand::Approvals)
            | SlashCommandAction::Builtin(SlashCommand::Permissions) => {
                self.open_permissions_popup();
            }
            SlashCommandAction::Builtin(SlashCommand::Vim) => {
                self.toggle_vim_mode_and_notify();
            }
            SlashCommandAction::Builtin(SlashCommand::Keymap) => {
                self.open_keymap_picker();
            }
            SlashCommandAction::Builtin(SlashCommand::ElevateSandbox) => {
                #[cfg(target_os = "windows")]
                {
                    let windows_sandbox_level = WindowsSandboxLevel::from_config(&self.config);
                    let windows_degraded_sandbox_enabled =
                        matches!(windows_sandbox_level, WindowsSandboxLevel::RestrictedToken);
                    if !windows_degraded_sandbox_enabled
                        || !crate::legacy_core::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED
                    {
                        return;
                    }

                    let Some(preset) = builtin_approval_presets()
                        .into_iter()
                        .find(|preset| preset.id == "auto")
                    else {
                        self.add_error_message(
                            "Internal error: missing the 'auto' approval preset.".to_string(),
                        );
                        return;
                    };

                    if let Err(err) = self
                        .config
                        .permissions
                        .approval_policy
                        .can_set(&preset.approval)
                    {
                        self.add_error_message(err.to_string());
                        return;
                    }

                    self.session_telemetry.counter(
                        "codex.windows_sandbox.setup_elevated_sandbox_command",
                        /*inc*/ 1,
                        &[],
                    );
                    self.app_event_tx
                        .send(AppEvent::BeginWindowsSandboxElevatedSetup { preset });
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = &self.session_telemetry;
                }
            }
            SlashCommandAction::Builtin(SlashCommand::SandboxReadRoot) => {
                self.add_error_message(
                    "Usage: /sandbox-add-read-dir <absolute-directory-path>".to_string(),
                );
            }
            SlashCommandAction::Builtin(SlashCommand::Experimental) => {
                self.open_experimental_popup();
            }
            SlashCommandAction::Builtin(SlashCommand::AutoReview) => {
                self.open_auto_review_denials_popup();
            }
            SlashCommandAction::Builtin(SlashCommand::Memories) => {
                self.open_memories_popup();
            }
            SlashCommandAction::Builtin(SlashCommand::Quit)
            | SlashCommandAction::Builtin(SlashCommand::Exit) => {
                self.request_quit_without_confirmation();
            }
            SlashCommandAction::Builtin(SlashCommand::Logout) => {
                self.app_event_tx.send(AppEvent::Logout);
            }
            SlashCommandAction::Builtin(SlashCommand::Copy) => {
                self.copy_last_agent_markdown();
            }
            SlashCommandAction::Builtin(SlashCommand::Diff) => {
                self.add_diff_in_progress();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let text = match get_git_diff().await {
                        Ok((is_git_repo, diff_text)) => {
                            if is_git_repo {
                                diff_text
                            } else {
                                "`/diff` — _not inside a git repository_".to_string()
                            }
                        }
                        Err(e) => format!("Failed to compute diff: {e}"),
                    };
                    tx.send(AppEvent::DiffResult(text));
                });
            }
            SlashCommandAction::Builtin(SlashCommand::Mention) => {
                self.insert_str("@");
            }
            SlashCommandAction::Builtin(SlashCommand::Skills) => {
                self.open_skills_menu();
            }
            SlashCommandAction::Builtin(SlashCommand::Hooks) => {
                self.add_hooks_output();
            }
            SlashCommandAction::Builtin(SlashCommand::Status) => {
                if self.should_prefetch_rate_limits() {
                    let request_id = self.next_status_refresh_request_id;
                    self.next_status_refresh_request_id =
                        self.next_status_refresh_request_id.wrapping_add(1);
                    self.add_status_output(/*refreshing_rate_limits*/ true, Some(request_id));
                    self.app_event_tx.send(AppEvent::RefreshRateLimits {
                        origin: RateLimitRefreshOrigin::StatusCommand { request_id },
                    });
                } else {
                    self.add_status_output(
                        /*refreshing_rate_limits*/ false, /*request_id*/ None,
                    );
                }
            }
            SlashCommandAction::Builtin(SlashCommand::Ide) => {
                self.handle_ide_command();
            }
            SlashCommandAction::Builtin(SlashCommand::DebugConfig) => {
                self.add_debug_config_output();
            }
            SlashCommandAction::Builtin(SlashCommand::Title) => {
                self.open_terminal_title_setup();
            }
            SlashCommandAction::Builtin(SlashCommand::Statusline) => {
                self.open_status_line_setup();
            }
            SlashCommandAction::Builtin(SlashCommand::Theme) => {
                self.open_theme_picker();
            }
            SlashCommandAction::Builtin(SlashCommand::Ps) => {
                self.add_ps_output();
            }
            SlashCommandAction::Builtin(SlashCommand::Stop) => {
                self.clean_background_terminals();
            }
            SlashCommandAction::Builtin(SlashCommand::MemoryDrop)
            | SlashCommandAction::Builtin(SlashCommand::MemoryUpdate) => {
                self.add_app_server_stub_message("Memory maintenance");
            }
            SlashCommandAction::Builtin(SlashCommand::Mcp) => {
                self.add_mcp_output(McpServerStatusDetail::ToolsAndAuthOnly);
            }
            SlashCommandAction::Builtin(SlashCommand::Apps) => {
                self.add_connectors_output();
            }
            SlashCommandAction::Builtin(SlashCommand::Plugins) => {
                self.add_plugins_output();
            }
            SlashCommandAction::Builtin(SlashCommand::Rollout) => {
                if let Some(path) = self.rollout_path() {
                    self.add_info_message(
                        format!("Current rollout path: {}", path.display()),
                        /*hint*/ None,
                    );
                } else {
                    self.add_info_message(
                        "Rollout path is not available yet.".to_string(),
                        /*hint*/ None,
                    );
                }
            }
            SlashCommandAction::Builtin(SlashCommand::TestApproval) => {
                use std::collections::HashMap;

                use crate::approval_events::ApplyPatchApprovalRequestEvent;
                use crate::diff_model::FileChange;

                self.on_apply_patch_approval_request(
                    "1".to_string(),
                    ApplyPatchApprovalRequestEvent {
                        call_id: "1".to_string(),
                        turn_id: "turn-1".to_string(),
                        changes: HashMap::from([
                            (
                                PathBuf::from("/tmp/test.txt"),
                                FileChange::Add {
                                    content: "test".to_string(),
                                },
                            ),
                            (
                                PathBuf::from("/tmp/test2.txt"),
                                FileChange::Update {
                                    unified_diff: "+test\n-test2".to_string(),
                                    move_path: None,
                                },
                            ),
                        ]),
                        reason: None,
                        grant_root: Some(PathBuf::from("/tmp")),
                    },
                );
            }
        }
    }

    /// Run an inline slash command.
    ///
    /// Branches that prepare arguments should pass `record_history: false` to the composer because
    /// the staged slash-command entry is the recall record; using the normal submission-history
    /// path as well would make a single command appear twice during Up-arrow navigation.
    pub(super) fn dispatch_command_with_args(
        &mut self,
        cmd: impl Into<SlashCommandAction>,
        args: String,
        text_elements: Vec<TextElement>,
    ) {
        let cmd = cmd.into();
        if !self.ensure_slash_command_allowed_in_side_conversation(&cmd) {
            return;
        }
        if !self.ensure_side_command_allowed_outside_review(&cmd) {
            return;
        }
        if !command_supports_inline_args(&cmd) {
            self.dispatch_command(cmd);
            return;
        }
        if !command_available_during_task(&cmd) && self.bottom_pane.is_task_running() {
            let message = format!(
                "'/{}' is disabled while a task is in progress.",
                command_name(&cmd)
            );
            self.add_to_history(history_cell::new_error_event(message));
            self.request_redraw();
            return;
        }

        let trimmed = args.trim();
        if trimmed.is_empty() {
            self.dispatch_command(cmd);
            return;
        }

        let Some((prepared_args, prepared_elements)) =
            self.prepare_live_inline_args(args, text_elements)
        else {
            return;
        };
        self.dispatch_prepared_command_with_args(
            cmd,
            PreparedSlashCommandArgs {
                args: prepared_args,
                text_elements: prepared_elements,
                local_images: Vec::new(),
                remote_image_urls: Vec::new(),
                mention_bindings: Vec::new(),
                source: SlashCommandDispatchSource::Live,
            },
        );
    }

    fn prepare_live_inline_args(
        &mut self,
        args: String,
        text_elements: Vec<TextElement>,
    ) -> Option<(String, Vec<TextElement>)> {
        if self.bottom_pane.composer_text().is_empty() {
            Some((args, text_elements))
        } else {
            self.bottom_pane
                .prepare_inline_args_submission(/*record_history*/ false)
        }
    }

    fn prepared_inline_user_message(
        &mut self,
        args: String,
        text_elements: Vec<TextElement>,
        mut local_images: Vec<LocalImageAttachment>,
        mut remote_image_urls: Vec<String>,
        mut mention_bindings: Vec<MentionBinding>,
        source: SlashCommandDispatchSource,
    ) -> UserMessage {
        if source == SlashCommandDispatchSource::Live {
            local_images = self
                .bottom_pane
                .take_recent_submission_images_with_placeholders();
            remote_image_urls = self.take_remote_image_urls();
            mention_bindings = self.bottom_pane.take_recent_submission_mention_bindings();
        }
        UserMessage {
            text: args,
            local_images,
            remote_image_urls,
            text_elements,
            mention_bindings,
        }
    }

    fn dispatch_prepared_command_with_args(
        &mut self,
        cmd: SlashCommandAction,
        prepared: PreparedSlashCommandArgs,
    ) {
        let PreparedSlashCommandArgs {
            args,
            text_elements,
            local_images,
            remote_image_urls,
            mention_bindings,
            source,
        } = prepared;
        let trimmed = args.trim();
        let is_goal_command = command_as_builtin(&cmd) == Some(SlashCommand::Goal);
        match cmd {
            SlashCommandAction::ServiceTier(command) => {
                match trimmed.to_ascii_lowercase().as_str() {
                    "on" => self.set_service_tier_selection(Some(command.service_tier.clone())),
                    "off" => self.set_service_tier_selection(/*service_tier*/ None),
                    "status" => {
                        let status = if self.current_service_tier().as_ref()
                            == Some(&command.service_tier)
                        {
                            "on"
                        } else {
                            "off"
                        };
                        self.add_info_message(
                            format!("{} service tier is {status}.", command.name),
                            /*hint*/ None,
                        );
                    }
                    _ => {
                        self.add_error_message(format!(
                            "Usage: /{} [on|off|status]",
                            command.command
                        ));
                    }
                }
            }
            SlashCommandAction::Builtin(SlashCommand::Ide) => {
                self.handle_ide_command_args(trimmed);
            }
            SlashCommandAction::Builtin(SlashCommand::Mcp) => {
                match trimmed.to_ascii_lowercase().as_str() {
                    "verbose" => self.add_mcp_output(McpServerStatusDetail::Full),
                    _ => self.add_error_message("Usage: /mcp [verbose]".to_string()),
                }
            }
            SlashCommandAction::Builtin(SlashCommand::Rename) if !trimmed.is_empty() => {
                if !self.ensure_thread_rename_allowed() {
                    return;
                }
                self.session_telemetry
                    .counter("codex.thread.rename", /*inc*/ 1, &[]);
                let Some(name) = crate::legacy_core::util::normalize_thread_name(&args) else {
                    self.add_error_message("Thread name cannot be empty.".to_string());
                    return;
                };
                self.app_event_tx.set_thread_name(name);
            }
            SlashCommandAction::Builtin(SlashCommand::Plan) if !trimmed.is_empty() => {
                if !self.apply_plan_slash_command() {
                    return;
                }
                let user_message = self.prepared_inline_user_message(
                    args,
                    text_elements,
                    local_images,
                    remote_image_urls,
                    mention_bindings,
                    source,
                );
                if self.is_session_configured() {
                    self.reasoning_buffer.clear();
                    self.full_reasoning_buffer.clear();
                    self.set_status_header(String::from("Working"));
                    self.submit_user_message(user_message);
                } else {
                    self.queue_user_message(user_message);
                }
            }
            SlashCommandAction::Builtin(SlashCommand::Goal) if !trimmed.is_empty() => {
                if !self.config.features.enabled(Feature::Goals) {
                    return;
                }
                enum GoalControlCommand {
                    Clear,
                    SetStatus(AppThreadGoalStatus),
                }
                let control_command = match trimmed.to_ascii_lowercase().as_str() {
                    "clear" => Some(GoalControlCommand::Clear),
                    "pause" => Some(GoalControlCommand::SetStatus(AppThreadGoalStatus::Paused)),
                    "resume" => Some(GoalControlCommand::SetStatus(AppThreadGoalStatus::Active)),
                    _ => None,
                };
                if let Some(command) = control_command {
                    let Some(thread_id) = self.thread_id else {
                        self.add_info_message(
                            GOAL_USAGE.to_string(),
                            Some(
                                "The session must start before you can change a goal.".to_string(),
                            ),
                        );
                        return;
                    };
                    match command {
                        GoalControlCommand::Clear => {
                            self.app_event_tx
                                .send(AppEvent::ClearThreadGoal { thread_id });
                        }
                        GoalControlCommand::SetStatus(status) => {
                            self.app_event_tx
                                .send(AppEvent::SetThreadGoalStatus { thread_id, status });
                        }
                    }
                    if source == SlashCommandDispatchSource::Live {
                        self.bottom_pane.drain_pending_submission_state();
                    }
                    return;
                }
                let objective = args.trim();
                if objective.is_empty() {
                    self.add_error_message("Goal objective must not be empty.".to_string());
                    self.add_info_message(
                        GOAL_USAGE.to_string(),
                        Some(GOAL_USAGE_HINT.to_string()),
                    );
                    if source == SlashCommandDispatchSource::Live {
                        self.bottom_pane.drain_pending_submission_state();
                    }
                    return;
                }
                let Some(thread_id) = self.thread_id else {
                    if source == SlashCommandDispatchSource::Live {
                        self.queue_user_message_with_options(
                            UserMessage {
                                text: format!("/goal {args}"),
                                local_images: Vec::new(),
                                remote_image_urls: Vec::new(),
                                text_elements: Vec::new(),
                                mention_bindings: Vec::new(),
                            },
                            QueuedInputAction::ParseSlash,
                        );
                        self.bottom_pane.drain_pending_submission_state();
                    } else {
                        self.add_info_message(
                            GOAL_USAGE.to_string(),
                            Some("The session must start before you can set a goal.".to_string()),
                        );
                    }
                    return;
                };
                self.app_event_tx.send(AppEvent::SetThreadGoalObjective {
                    thread_id,
                    objective: objective.to_string(),
                    mode: ThreadGoalSetMode::ConfirmIfExists,
                });
                if source == SlashCommandDispatchSource::Live {
                    self.bottom_pane.drain_pending_submission_state();
                }
            }
            SlashCommandAction::Builtin(SlashCommand::Side) if !trimmed.is_empty() => {
                let Some(parent_thread_id) = self.thread_id else {
                    self.add_error_message(
                        "'/side' is unavailable before the session starts.".to_string(),
                    );
                    return;
                };
                let user_message = self.prepared_inline_user_message(
                    args,
                    text_elements,
                    local_images,
                    remote_image_urls,
                    mention_bindings,
                    source,
                );
                self.request_side_conversation(parent_thread_id, Some(user_message));
            }
            SlashCommandAction::Builtin(SlashCommand::Review) if !trimmed.is_empty() => {
                self.submit_op(AppCommand::review(ReviewRequest {
                    target: ReviewTarget::Custom { instructions: args },
                    user_facing_hint: None,
                }));
            }
            SlashCommandAction::Builtin(SlashCommand::Resume) if !trimmed.is_empty() => {
                self.app_event_tx
                    .send(AppEvent::ResumeSessionByIdOrName(args));
            }
            SlashCommandAction::Builtin(SlashCommand::SandboxReadRoot) if !trimmed.is_empty() => {
                self.app_event_tx
                    .send(AppEvent::BeginWindowsSandboxGrantReadRoot { path: args });
            }
            _ => self.dispatch_command(cmd),
        }
        if source == SlashCommandDispatchSource::Live && !is_goal_command {
            self.bottom_pane.drain_pending_submission_state();
        }
    }

    pub(super) fn submit_queued_slash_prompt(&mut self, user_message: UserMessage) -> QueueDrain {
        let UserMessage {
            text,
            local_images,
            remote_image_urls,
            text_elements,
            mention_bindings,
        } = user_message;
        let Some((name, rest, rest_offset)) = parse_slash_name(&text) else {
            self.submit_user_message(UserMessage {
                text,
                local_images,
                remote_image_urls,
                text_elements,
                mention_bindings,
            });
            return QueueDrain::Stop;
        };

        if name.contains('/') {
            self.submit_user_message(UserMessage {
                text,
                local_images,
                remote_image_urls,
                text_elements,
                mention_bindings,
            });
            return QueueDrain::Stop;
        }

        let Some(cmd) = slash_commands::find_command(
            name,
            self.builtin_command_flags(),
            &available_service_tier_commands(self),
        ) else {
            self.add_info_message(
                format!(
                    r#"Unrecognized command '/{name}'. Type "/" for a list of supported commands."#
                ),
                /*hint*/ None,
            );
            return QueueDrain::Continue;
        };

        if rest.is_empty() {
            self.dispatch_command(cmd.clone());
            return self.queued_command_drain_result(cmd);
        }

        if !command_supports_inline_args(&cmd) {
            self.submit_user_message(UserMessage {
                text,
                local_images,
                remote_image_urls,
                text_elements,
                mention_bindings,
            });
            return QueueDrain::Stop;
        }

        let trimmed_start = rest.trim_start();
        let leading_trimmed = rest.len().saturating_sub(trimmed_start.len());
        let trimmed_rest = trimmed_start.trim_end();
        let args_elements = Self::slash_command_args_elements(
            trimmed_rest,
            rest_offset + leading_trimmed,
            &text_elements,
        );
        self.dispatch_prepared_command_with_args(
            cmd.clone(),
            PreparedSlashCommandArgs {
                args: trimmed_rest.to_string(),
                text_elements: args_elements,
                local_images,
                remote_image_urls,
                mention_bindings,
                source: SlashCommandDispatchSource::Queued,
            },
        );
        self.queued_command_drain_result(cmd)
    }

    fn builtin_command_flags(&self) -> slash_commands::BuiltinCommandFlags {
        #[cfg(target_os = "windows")]
        let allow_elevate_sandbox = {
            let windows_sandbox_level = WindowsSandboxLevel::from_config(&self.config);
            matches!(windows_sandbox_level, WindowsSandboxLevel::RestrictedToken)
        };
        #[cfg(not(target_os = "windows"))]
        let allow_elevate_sandbox = false;

        slash_commands::BuiltinCommandFlags {
            collaboration_modes_enabled: self.collaboration_modes_enabled(),
            connectors_enabled: self.connectors_enabled(),
            plugins_command_enabled: self.config.features.enabled(Feature::Plugins),
            goal_command_enabled: self.config.features.enabled(Feature::Goals),
            fast_command_enabled: self.fast_mode_enabled(),
            personality_command_enabled: self.config.features.enabled(Feature::Personality),
            realtime_conversation_enabled: self.realtime_conversation_enabled(),
            audio_device_selection_enabled: self.realtime_audio_device_selection_enabled(),
            allow_elevate_sandbox,
            side_conversation_active: self.active_side_conversation,
        }
    }

    fn queued_command_drain_result(&self, cmd: SlashCommandAction) -> QueueDrain {
        if self.is_user_turn_pending_or_running() || !self.bottom_pane.no_modal_or_popup_active() {
            return QueueDrain::Stop;
        }
        match cmd {
            SlashCommandAction::ServiceTier(_)
            | SlashCommandAction::Builtin(SlashCommand::Ide)
            | SlashCommandAction::Builtin(SlashCommand::Status)
            | SlashCommandAction::Builtin(SlashCommand::DebugConfig)
            | SlashCommandAction::Builtin(SlashCommand::Ps)
            | SlashCommandAction::Builtin(SlashCommand::Stop)
            | SlashCommandAction::Builtin(SlashCommand::MemoryDrop)
            | SlashCommandAction::Builtin(SlashCommand::MemoryUpdate)
            | SlashCommandAction::Builtin(SlashCommand::Mcp)
            | SlashCommandAction::Builtin(SlashCommand::Apps)
            | SlashCommandAction::Builtin(SlashCommand::Plugins)
            | SlashCommandAction::Builtin(SlashCommand::Rollout)
            | SlashCommandAction::Builtin(SlashCommand::Copy)
            | SlashCommandAction::Builtin(SlashCommand::Vim)
            | SlashCommandAction::Builtin(SlashCommand::Diff)
            | SlashCommandAction::Builtin(SlashCommand::Rename)
            | SlashCommandAction::Builtin(SlashCommand::TestApproval) => QueueDrain::Continue,
            SlashCommandAction::Builtin(SlashCommand::Feedback)
            | SlashCommandAction::Builtin(SlashCommand::New)
            | SlashCommandAction::Builtin(SlashCommand::Clear)
            | SlashCommandAction::Builtin(SlashCommand::Resume)
            | SlashCommandAction::Builtin(SlashCommand::Fork)
            | SlashCommandAction::Builtin(SlashCommand::Init)
            | SlashCommandAction::Builtin(SlashCommand::Compact)
            | SlashCommandAction::Builtin(SlashCommand::Review)
            | SlashCommandAction::Builtin(SlashCommand::Model)
            | SlashCommandAction::Builtin(SlashCommand::Realtime)
            | SlashCommandAction::Builtin(SlashCommand::Settings)
            | SlashCommandAction::Builtin(SlashCommand::Personality)
            | SlashCommandAction::Builtin(SlashCommand::Plan)
            | SlashCommandAction::Builtin(SlashCommand::Goal)
            | SlashCommandAction::Builtin(SlashCommand::Collab)
            | SlashCommandAction::Builtin(SlashCommand::Side)
            | SlashCommandAction::Builtin(SlashCommand::Keymap)
            | SlashCommandAction::Builtin(SlashCommand::Agent)
            | SlashCommandAction::Builtin(SlashCommand::MultiAgents)
            | SlashCommandAction::Builtin(SlashCommand::Approvals)
            | SlashCommandAction::Builtin(SlashCommand::Permissions)
            | SlashCommandAction::Builtin(SlashCommand::ElevateSandbox)
            | SlashCommandAction::Builtin(SlashCommand::SandboxReadRoot)
            | SlashCommandAction::Builtin(SlashCommand::Experimental)
            | SlashCommandAction::Builtin(SlashCommand::AutoReview)
            | SlashCommandAction::Builtin(SlashCommand::Memories)
            | SlashCommandAction::Builtin(SlashCommand::Quit)
            | SlashCommandAction::Builtin(SlashCommand::Exit)
            | SlashCommandAction::Builtin(SlashCommand::Logout)
            | SlashCommandAction::Builtin(SlashCommand::Mention)
            | SlashCommandAction::Builtin(SlashCommand::Skills)
            | SlashCommandAction::Builtin(SlashCommand::Hooks)
            | SlashCommandAction::Builtin(SlashCommand::Title)
            | SlashCommandAction::Builtin(SlashCommand::Statusline)
            | SlashCommandAction::Builtin(SlashCommand::Theme) => QueueDrain::Stop,
        }
    }

    fn slash_command_args_elements(
        rest: &str,
        rest_offset: usize,
        text_elements: &[TextElement],
    ) -> Vec<TextElement> {
        if rest.is_empty() || text_elements.is_empty() {
            return Vec::new();
        }
        text_elements
            .iter()
            .filter_map(|elem| {
                if elem.byte_range.end <= rest_offset {
                    return None;
                }
                let start = elem.byte_range.start.saturating_sub(rest_offset);
                let mut end = elem.byte_range.end.saturating_sub(rest_offset);
                if start >= rest.len() {
                    return None;
                }
                end = end.min(rest.len());
                (start < end).then_some(elem.map_range(|_| ByteRange { start, end }))
            })
            .collect()
    }

    fn ensure_slash_command_allowed_in_side_conversation(
        &mut self,
        cmd: &SlashCommandAction,
    ) -> bool {
        if !self.active_side_conversation || command_available_in_side_conversation(cmd) {
            return true;
        }
        self.add_error_message(format!(
            "'/{}' is unavailable in side conversations. {SIDE_SLASH_COMMAND_UNAVAILABLE_HINT}",
            command_name(cmd)
        ));
        self.bottom_pane.drain_pending_submission_state();
        false
    }

    fn ensure_side_command_allowed_outside_review(&mut self, cmd: &SlashCommandAction) -> bool {
        if command_as_builtin(cmd) != Some(SlashCommand::Side) || !self.is_review_mode {
            return true;
        }

        self.add_error_message(SIDE_REVIEW_UNAVAILABLE_MESSAGE.to_string());
        self.bottom_pane.drain_pending_submission_state();
        false
    }
}
