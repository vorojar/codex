use std::path::PathBuf;

use codex_protocol::ThreadId;
use codex_protocol::protocol::HookCompletedEvent;
use codex_protocol::protocol::HookEventName;
use codex_protocol::protocol::HookOutputEntry;
use codex_protocol::protocol::HookOutputEntryKind;
use codex_protocol::protocol::HookRunStatus;
use codex_protocol::protocol::HookRunSummary;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::common;
use crate::engine::CommandShell;
use crate::engine::ConfiguredHandler;
use crate::engine::command_runner::CommandRunResult;
use crate::engine::dispatcher;
use crate::engine::output_parser;
use crate::schema::PostCompactCommandInput;
use crate::schema::PreCompactCommandInput;

#[derive(Debug, Clone)]
pub struct PreCompactRequest {
    pub session_id: ThreadId,
    pub turn_id: String,
    pub cwd: AbsolutePathBuf,
    pub transcript_path: Option<PathBuf>,
    pub model: String,
    pub permission_mode: String,
    pub trigger: String,
    pub custom_instructions: String,
}

#[derive(Debug, Clone)]
pub struct PostCompactRequest {
    pub session_id: ThreadId,
    pub turn_id: String,
    pub cwd: AbsolutePathBuf,
    pub transcript_path: Option<PathBuf>,
    pub model: String,
    pub permission_mode: String,
    pub trigger: String,
    pub compact_summary: String,
}

#[derive(Debug)]
pub struct StatelessHookOutcome {
    pub hook_events: Vec<HookCompletedEvent>,
}

#[derive(Debug)]
pub struct PreCompactOutcome {
    pub hook_events: Vec<HookCompletedEvent>,
    pub should_block: bool,
    pub block_reason: Option<String>,
}

pub(crate) fn preview_pre(
    handlers: &[ConfiguredHandler],
    request: &PreCompactRequest,
) -> Vec<HookRunSummary> {
    dispatcher::select_handlers(
        handlers,
        HookEventName::PreCompact,
        Some(request.trigger.as_str()),
    )
    .into_iter()
    .map(|handler| dispatcher::running_summary(&handler))
    .collect()
}

pub(crate) async fn run_pre(
    handlers: &[ConfiguredHandler],
    shell: &CommandShell,
    request: PreCompactRequest,
) -> PreCompactOutcome {
    let matched = dispatcher::select_handlers(
        handlers,
        HookEventName::PreCompact,
        Some(request.trigger.as_str()),
    );
    if matched.is_empty() {
        return PreCompactOutcome {
            hook_events: Vec::new(),
            should_block: false,
            block_reason: None,
        };
    }

    let input_json = match pre_command_input_json(&request) {
        Ok(input_json) => input_json,
        Err(error) => {
            return PreCompactOutcome {
                hook_events: common::serialization_failure_hook_events(
                    matched,
                    Some(request.turn_id),
                    format!("failed to serialize pre compact hook input: {error}"),
                ),
                should_block: false,
                block_reason: None,
            };
        }
    };

    let results = dispatcher::execute_handlers(
        shell,
        matched,
        input_json,
        request.cwd.as_path(),
        Some(request.turn_id),
        parse_pre_completed,
    )
    .await;
    let should_block = results.iter().any(|result| result.data.should_block);
    let block_reason = results
        .iter()
        .find_map(|result| result.data.block_reason.clone());
    PreCompactOutcome {
        hook_events: results.into_iter().map(|result| result.completed).collect(),
        should_block,
        block_reason,
    }
}

fn pre_command_input_json(request: &PreCompactRequest) -> Result<String, serde_json::Error> {
    serde_json::to_string(&PreCompactCommandInput {
        session_id: request.session_id.to_string(),
        turn_id: request.turn_id.clone(),
        transcript_path: crate::schema::NullableString::from_path(request.transcript_path.clone()),
        cwd: request.cwd.display().to_string(),
        hook_event_name: "PreCompact".to_string(),
        model: request.model.clone(),
        permission_mode: request.permission_mode.clone(),
        trigger: request.trigger.clone(),
        custom_instructions: request.custom_instructions.clone(),
    })
}

pub(crate) fn preview_post(
    handlers: &[ConfiguredHandler],
    request: &PostCompactRequest,
) -> Vec<HookRunSummary> {
    dispatcher::select_handlers(
        handlers,
        HookEventName::PostCompact,
        Some(request.trigger.as_str()),
    )
    .into_iter()
    .map(|handler| dispatcher::running_summary(&handler))
    .collect()
}

pub(crate) async fn run_post(
    handlers: &[ConfiguredHandler],
    shell: &CommandShell,
    request: PostCompactRequest,
) -> StatelessHookOutcome {
    let matched = dispatcher::select_handlers(
        handlers,
        HookEventName::PostCompact,
        Some(request.trigger.as_str()),
    );
    if matched.is_empty() {
        return StatelessHookOutcome {
            hook_events: Vec::new(),
        };
    }

    let input_json = match post_command_input_json(&request) {
        Ok(input_json) => input_json,
        Err(error) => {
            return StatelessHookOutcome {
                hook_events: common::serialization_failure_hook_events(
                    matched,
                    Some(request.turn_id),
                    format!("failed to serialize post compact hook input: {error}"),
                ),
            };
        }
    };

    let results = dispatcher::execute_handlers(
        shell,
        matched,
        input_json,
        request.cwd.as_path(),
        Some(request.turn_id),
        parse_post_completed,
    )
    .await;
    StatelessHookOutcome {
        hook_events: results.into_iter().map(|result| result.completed).collect(),
    }
}

fn post_command_input_json(request: &PostCompactRequest) -> Result<String, serde_json::Error> {
    serde_json::to_string(&PostCompactCommandInput {
        session_id: request.session_id.to_string(),
        turn_id: request.turn_id.clone(),
        transcript_path: crate::schema::NullableString::from_path(request.transcript_path.clone()),
        cwd: request.cwd.display().to_string(),
        hook_event_name: "PostCompact".to_string(),
        model: request.model.clone(),
        permission_mode: request.permission_mode.clone(),
        trigger: request.trigger.clone(),
        compact_summary: request.compact_summary.clone(),
    })
}

#[derive(Default)]
struct CompactHandlerData {
    should_block: bool,
    block_reason: Option<String>,
}

fn parse_pre_completed(
    handler: &ConfiguredHandler,
    run_result: CommandRunResult,
    turn_id: Option<String>,
) -> dispatcher::ParsedHandler<CompactHandlerData> {
    let mut entries = Vec::new();
    let mut status = HookRunStatus::Completed;
    let mut should_block = false;
    let mut block_reason = None;

    match run_result.error.as_deref() {
        Some(error) => {
            status = HookRunStatus::Failed;
            entries.push(HookOutputEntry {
                kind: HookOutputEntryKind::Error,
                text: error.to_string(),
            });
        }
        None => match run_result.exit_code {
            Some(0) => {
                let trimmed_stdout = run_result.stdout.trim();
                if trimmed_stdout.is_empty() {
                } else if let Some(parsed) = output_parser::parse_pre_compact(&run_result.stdout) {
                    if let Some(system_message) = parsed.universal.system_message {
                        entries.push(HookOutputEntry {
                            kind: HookOutputEntryKind::Warning,
                            text: system_message,
                        });
                    }
                    if let Some(invalid_reason) = parsed.invalid_reason {
                        status = HookRunStatus::Failed;
                        entries.push(HookOutputEntry {
                            kind: HookOutputEntryKind::Error,
                            text: invalid_reason,
                        });
                    } else if let Some(invalid_block_reason) = parsed.invalid_block_reason {
                        status = HookRunStatus::Failed;
                        entries.push(HookOutputEntry {
                            kind: HookOutputEntryKind::Error,
                            text: invalid_block_reason,
                        });
                    } else if parsed.should_block {
                        status = HookRunStatus::Blocked;
                        should_block = true;
                        if let Some(reason) = parsed.reason {
                            block_reason = Some(reason.clone());
                            entries.push(HookOutputEntry {
                                kind: HookOutputEntryKind::Feedback,
                                text: reason,
                            });
                        }
                    }
                } else {
                    status = HookRunStatus::Failed;
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Error,
                        text: "hook returned invalid PreCompact hook JSON output".to_string(),
                    });
                }
            }
            Some(2) => {
                if let Some(reason) = common::trimmed_non_empty(&run_result.stderr) {
                    status = HookRunStatus::Blocked;
                    should_block = true;
                    block_reason = Some(reason.clone());
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Feedback,
                        text: reason,
                    });
                } else {
                    status = HookRunStatus::Failed;
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Error,
                        text: "PreCompact hook exited with code 2 but did not write a blocking reason to stderr".to_string(),
                    });
                }
            }
            Some(code) => {
                status = HookRunStatus::Failed;
                entries.push(HookOutputEntry {
                    kind: HookOutputEntryKind::Error,
                    text: common::trimmed_non_empty(&run_result.stderr)
                        .unwrap_or_else(|| format!("hook exited with code {code}")),
                });
            }
            None => {
                status = HookRunStatus::Failed;
                entries.push(HookOutputEntry {
                    kind: HookOutputEntryKind::Error,
                    text: "hook process terminated without an exit code".to_string(),
                });
            }
        },
    }

    dispatcher::ParsedHandler {
        completed: HookCompletedEvent {
            turn_id,
            run: dispatcher::completed_summary(handler, &run_result, status, entries),
        },
        data: CompactHandlerData {
            should_block,
            block_reason,
        },
    }
}

fn parse_post_completed(
    handler: &ConfiguredHandler,
    run_result: CommandRunResult,
    turn_id: Option<String>,
) -> dispatcher::ParsedHandler<CompactHandlerData> {
    parse_completed(
        handler,
        run_result,
        turn_id,
        "PostCompact",
        output_parser::parse_post_compact,
    )
}

fn parse_completed(
    handler: &ConfiguredHandler,
    run_result: CommandRunResult,
    turn_id: Option<String>,
    event_label: &'static str,
    parse_output: fn(&str) -> Option<output_parser::StatelessHookOutput>,
) -> dispatcher::ParsedHandler<CompactHandlerData> {
    let mut entries = Vec::new();
    let mut status = HookRunStatus::Completed;

    match run_result.error.as_deref() {
        Some(error) => {
            status = HookRunStatus::Failed;
            entries.push(HookOutputEntry {
                kind: HookOutputEntryKind::Error,
                text: error.to_string(),
            });
        }
        None => match run_result.exit_code {
            Some(0) => {
                let trimmed_stdout = run_result.stdout.trim();
                if trimmed_stdout.is_empty() {
                } else if let Some(parsed) = parse_output(&run_result.stdout) {
                    if let Some(system_message) = parsed.universal.system_message {
                        entries.push(HookOutputEntry {
                            kind: HookOutputEntryKind::Warning,
                            text: system_message,
                        });
                    }
                    if let Some(invalid_reason) = parsed.invalid_reason {
                        status = HookRunStatus::Failed;
                        entries.push(HookOutputEntry {
                            kind: HookOutputEntryKind::Error,
                            text: invalid_reason,
                        });
                    }
                } else {
                    status = HookRunStatus::Failed;
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Error,
                        text: format!("hook returned invalid {event_label} hook JSON output"),
                    });
                }
            }
            Some(code) => {
                status = HookRunStatus::Failed;
                entries.push(HookOutputEntry {
                    kind: HookOutputEntryKind::Error,
                    text: common::trimmed_non_empty(&run_result.stderr)
                        .unwrap_or_else(|| format!("hook exited with code {code}")),
                });
            }
            None => {
                status = HookRunStatus::Failed;
                entries.push(HookOutputEntry {
                    kind: HookOutputEntryKind::Error,
                    text: "hook process terminated without an exit code".to_string(),
                });
            }
        },
    }

    dispatcher::ParsedHandler {
        completed: HookCompletedEvent {
            turn_id,
            run: dispatcher::completed_summary(handler, &run_result, status, entries),
        },
        data: CompactHandlerData::default(),
    }
}

#[cfg(test)]
mod tests {
    use codex_protocol::ThreadId;
    use codex_protocol::protocol::HookEventName;
    use codex_protocol::protocol::HookOutputEntry;
    use codex_protocol::protocol::HookOutputEntryKind;
    use codex_protocol::protocol::HookRunStatus;
    use codex_utils_absolute_path::test_support::PathBufExt;
    use codex_utils_absolute_path::test_support::test_path_buf;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::parse_pre_completed;
    use super::post_command_input_json;
    use super::pre_command_input_json;
    use crate::engine::ConfiguredHandler;
    use crate::engine::command_runner::CommandRunResult;

    #[test]
    fn pre_compact_input_includes_lifecycle_metadata() {
        let input_json = pre_command_input_json(&pre_request()).expect("serialize command input");
        let input: serde_json::Value =
            serde_json::from_str(&input_json).expect("parse command input");

        assert_eq!(
            input,
            json!({
                "session_id": pre_request().session_id.to_string(),
                "turn_id": "turn-1",
                "transcript_path": null,
                "cwd": test_path_buf("/tmp").display().to_string(),
                "hook_event_name": "PreCompact",
                "model": "gpt-test",
                "permission_mode": "default",
                "trigger": "manual",
                "custom_instructions": "",
            })
        );
    }

    #[test]
    fn post_compact_input_includes_summary() {
        let input_json = post_command_input_json(&post_request()).expect("serialize command input");
        let input: serde_json::Value =
            serde_json::from_str(&input_json).expect("parse command input");

        assert_eq!(
            input,
            json!({
                "session_id": post_request().session_id.to_string(),
                "turn_id": "turn-1",
                "transcript_path": null,
                "cwd": test_path_buf("/tmp").display().to_string(),
                "hook_event_name": "PostCompact",
                "model": "gpt-test",
                "permission_mode": "default",
                "trigger": "manual",
                "compact_summary": "summary request complete",
            })
        );
    }

    #[test]
    fn block_decision_stops_compaction() {
        let parsed = parse_pre_completed(
            &handler(HookEventName::PreCompact),
            run_result(
                Some(0),
                r#"{"decision":"block","reason":"policy blocked compaction"}"#,
                "",
            ),
            Some("turn-1".to_string()),
        );

        assert_eq!(parsed.completed.run.status, HookRunStatus::Blocked);
        assert_eq!(parsed.data.should_block, true);
        assert_eq!(
            parsed.data.block_reason,
            Some("policy blocked compaction".to_string())
        );
        assert_eq!(
            parsed.completed.run.entries,
            vec![HookOutputEntry {
                kind: HookOutputEntryKind::Feedback,
                text: "policy blocked compaction".to_string(),
            }]
        );
    }

    #[test]
    fn continue_false_is_not_a_compaction_block_decision() {
        let parsed = parse_pre_completed(
            &handler(HookEventName::PreCompact),
            run_result(Some(0), r#"{"continue":false,"stopReason":"nope"}"#, ""),
            Some("turn-1".to_string()),
        );

        assert_eq!(parsed.completed.run.status, HookRunStatus::Failed);
        assert_eq!(
            parsed.completed.run.entries,
            vec![HookOutputEntry {
                kind: HookOutputEntryKind::Error,
                text: "PreCompact hook returned unsupported continue:false".to_string(),
            }]
        );
    }

    fn pre_request() -> super::PreCompactRequest {
        super::PreCompactRequest {
            session_id: ThreadId::from_string("00000000-0000-4000-8000-000000000001")
                .expect("valid thread id"),
            turn_id: "turn-1".to_string(),
            cwd: test_path_buf("/tmp").abs(),
            transcript_path: None,
            model: "gpt-test".to_string(),
            permission_mode: "default".to_string(),
            trigger: "manual".to_string(),
            custom_instructions: String::new(),
        }
    }

    fn post_request() -> super::PostCompactRequest {
        super::PostCompactRequest {
            session_id: ThreadId::from_string("00000000-0000-4000-8000-000000000002")
                .expect("valid thread id"),
            turn_id: "turn-1".to_string(),
            cwd: test_path_buf("/tmp").abs(),
            transcript_path: None,
            model: "gpt-test".to_string(),
            permission_mode: "default".to_string(),
            trigger: "manual".to_string(),
            compact_summary: "summary request complete".to_string(),
        }
    }

    fn handler(event_name: HookEventName) -> ConfiguredHandler {
        ConfiguredHandler {
            event_name,
            is_managed: false,
            matcher: None,
            command: "python3 compact_hook.py".to_string(),
            timeout_sec: 5,
            status_message: Some("running compact hook".to_string()),
            source_path: test_path_buf("/tmp/hooks.json").abs(),
            source: codex_protocol::protocol::HookSource::User,
            display_order: 0,
        }
    }

    fn run_result(exit_code: Option<i32>, stdout: &str, stderr: &str) -> CommandRunResult {
        CommandRunResult {
            started_at: 1_700_000_000,
            completed_at: 1_700_000_001,
            duration_ms: 12,
            exit_code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            error: None,
        }
    }
}
