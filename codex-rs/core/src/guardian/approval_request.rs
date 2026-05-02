use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use codex_analytics::GuardianReviewedAction;
use codex_protocol::approvals::ExecApprovalRequestEvent;
use codex_protocol::approvals::GuardianAssessmentAction;
use codex_protocol::approvals::GuardianCommandSource;
use codex_protocol::approvals::NetworkApprovalContext;
use codex_protocol::approvals::NetworkApprovalProtocol;
use codex_protocol::approvals::NetworkPolicyAmendment;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::protocol::ApplyPatchApprovalRequestEvent;
use codex_protocol::protocol::ExecPolicyAmendment;
use codex_protocol::protocol::FileChange;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::request_permissions::RequestPermissionProfile;
use codex_protocol::request_permissions::RequestPermissionsEvent;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Serialize;
use serde_json::Value;

use super::GUARDIAN_MAX_ACTION_STRING_TOKENS;
use super::prompt::guardian_truncate_text;
use crate::tools::hook_names::HookToolName;
use crate::tools::sandboxing::PermissionRequestPayload;

/// Canonical description of an approval-worthy action in core.
///
/// This type should describe the action being reviewed exactly once, with
/// guardian review, approval hooks, and user-prompt transports deriving their
/// own projections from it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GuardianApprovalRequest {
    Shell {
        id: String,
        command: Vec<String>,
        hook_command: String,
        cwd: AbsolutePathBuf,
        sandbox_permissions: crate::sandboxing::SandboxPermissions,
        additional_permissions: Option<AdditionalPermissionProfile>,
        justification: Option<String>,
    },
    ExecCommand {
        id: String,
        command: Vec<String>,
        hook_command: String,
        cwd: AbsolutePathBuf,
        sandbox_permissions: crate::sandboxing::SandboxPermissions,
        additional_permissions: Option<AdditionalPermissionProfile>,
        justification: Option<String>,
        tty: bool,
    },
    #[cfg(unix)]
    Execve {
        id: String,
        source: GuardianCommandSource,
        program: String,
        argv: Vec<String>,
        cwd: AbsolutePathBuf,
        additional_permissions: Option<AdditionalPermissionProfile>,
    },
    ApplyPatch {
        id: String,
        cwd: AbsolutePathBuf,
        files: Vec<AbsolutePathBuf>,
        changes: HashMap<PathBuf, FileChange>,
        patch: String,
    },
    NetworkAccess {
        id: String,
        turn_id: String,
        target: String,
        hook_command: String,
        host: String,
        protocol: NetworkApprovalProtocol,
        port: u16,
        trigger: Option<GuardianNetworkAccessTrigger>,
    },
    McpToolCall {
        id: String,
        server: String,
        tool_name: String,
        hook_tool_name: String,
        arguments: Option<Value>,
        connector_id: Option<String>,
        connector_name: Option<String>,
        connector_description: Option<String>,
        tool_title: Option<String>,
        tool_description: Option<String>,
        annotations: Option<GuardianMcpAnnotations>,
    },
    RequestPermissions {
        id: String,
        turn_id: String,
        reason: Option<String>,
        permissions: RequestPermissionProfile,
        cwd: AbsolutePathBuf,
    },
}

impl GuardianApprovalRequest {
    pub(crate) fn permission_request_payload(&self) -> Option<PermissionRequestPayload> {
        match self {
            Self::Shell {
                hook_command,
                justification,
                ..
            }
            | Self::ExecCommand {
                hook_command,
                justification,
                ..
            } => Some(PermissionRequestPayload::bash(
                hook_command.clone(),
                justification.clone(),
            )),
            #[cfg(unix)]
            Self::Execve { program, argv, .. } => {
                let mut command = vec![program.clone()];
                if argv.len() > 1 {
                    command.extend_from_slice(&argv[1..]);
                }
                Some(PermissionRequestPayload::bash(
                    codex_shell_command::parse_command::shlex_join(&command),
                    /*description*/ None,
                ))
            }
            Self::ApplyPatch { patch, .. } => Some(PermissionRequestPayload {
                tool_name: HookToolName::apply_patch(),
                tool_input: serde_json::json!({ "command": patch }),
            }),
            Self::NetworkAccess {
                target,
                hook_command,
                ..
            } => Some(PermissionRequestPayload::bash(
                hook_command.clone(),
                Some(format!("network-access {target}")),
            )),
            Self::McpToolCall {
                hook_tool_name,
                arguments,
                ..
            } => Some(PermissionRequestPayload {
                tool_name: HookToolName::new(hook_tool_name.clone()),
                tool_input: arguments
                    .clone()
                    .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
            }),
            Self::RequestPermissions { .. } => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn exec_approval_event(
        &self,
        turn_id: String,
        approval_id: Option<String>,
        reason: Option<String>,
        network_approval_context: Option<NetworkApprovalContext>,
        proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
        proposed_network_policy_amendments: Option<Vec<NetworkPolicyAmendment>>,
        available_decisions: Option<Vec<ReviewDecision>>,
        fallback_cwd: Option<AbsolutePathBuf>,
    ) -> Option<ExecApprovalRequestEvent> {
        match self {
            Self::Shell {
                id,
                command,
                cwd,
                additional_permissions,
                ..
            }
            | Self::ExecCommand {
                id,
                command,
                cwd,
                additional_permissions,
                ..
            } => Some(ExecApprovalRequestEvent {
                call_id: id.clone(),
                approval_id,
                turn_id,
                command: command.clone(),
                cwd: cwd.clone(),
                reason,
                network_approval_context,
                proposed_execpolicy_amendment,
                proposed_network_policy_amendments,
                additional_permissions: additional_permissions.clone(),
                available_decisions,
                parsed_cmd: codex_shell_command::parse_command::parse_command(command),
            }),
            #[cfg(unix)]
            Self::Execve {
                id,
                argv,
                cwd,
                additional_permissions,
                ..
            } => Some(ExecApprovalRequestEvent {
                call_id: id.clone(),
                approval_id,
                turn_id,
                command: argv.clone(),
                cwd: cwd.clone(),
                reason,
                network_approval_context,
                proposed_execpolicy_amendment,
                proposed_network_policy_amendments,
                additional_permissions: additional_permissions.clone(),
                available_decisions,
                parsed_cmd: codex_shell_command::parse_command::parse_command(argv),
            }),
            Self::NetworkAccess {
                id,
                turn_id,
                target,
                host,
                protocol,
                ..
            } => {
                let command = vec!["network-access".to_string(), target.clone()];
                let cwd = fallback_cwd?;
                let network_approval_context = Some(NetworkApprovalContext {
                    host: host.clone(),
                    protocol: *protocol,
                });
                let proposed_network_policy_amendments = proposed_network_policy_amendments
                    .or_else(|| {
                        Some(vec![
                            NetworkPolicyAmendment {
                                host: host.clone(),
                                action: codex_protocol::approvals::NetworkPolicyRuleAction::Allow,
                            },
                            NetworkPolicyAmendment {
                                host: host.clone(),
                                action: codex_protocol::approvals::NetworkPolicyRuleAction::Deny,
                            },
                        ])
                    });
                Some(ExecApprovalRequestEvent {
                    call_id: id.clone(),
                    approval_id,
                    turn_id: turn_id.clone(),
                    command: command.clone(),
                    cwd,
                    reason,
                    network_approval_context,
                    proposed_execpolicy_amendment: None,
                    proposed_network_policy_amendments,
                    additional_permissions: None,
                    available_decisions,
                    parsed_cmd: codex_shell_command::parse_command::parse_command(&command),
                })
            }
            Self::ApplyPatch { .. }
            | Self::McpToolCall { .. }
            | Self::RequestPermissions { .. } => None,
        }
    }

    pub(crate) fn apply_patch_approval_event(
        &self,
        turn_id: String,
        reason: Option<String>,
        grant_root: Option<PathBuf>,
    ) -> Option<ApplyPatchApprovalRequestEvent> {
        match self {
            Self::ApplyPatch { id, changes, .. } => Some(ApplyPatchApprovalRequestEvent {
                call_id: id.clone(),
                turn_id,
                changes: changes.clone(),
                reason,
                grant_root,
            }),
            Self::Shell { .. }
            | Self::ExecCommand { .. }
            | Self::NetworkAccess { .. }
            | Self::McpToolCall { .. }
            | Self::RequestPermissions { .. } => None,
            #[cfg(unix)]
            Self::Execve { .. } => None,
        }
    }

    pub(crate) fn request_permissions_event(&self) -> Option<RequestPermissionsEvent> {
        match self {
            Self::RequestPermissions {
                id,
                turn_id,
                reason,
                permissions,
                cwd,
            } => Some(RequestPermissionsEvent {
                call_id: id.clone(),
                turn_id: turn_id.clone(),
                reason: reason.clone(),
                permissions: permissions.clone(),
                cwd: Some(cwd.clone()),
            }),
            Self::Shell { .. }
            | Self::ExecCommand { .. }
            | Self::ApplyPatch { .. }
            | Self::NetworkAccess { .. }
            | Self::McpToolCall { .. } => None,
            #[cfg(unix)]
            Self::Execve { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GuardianNetworkAccessTrigger {
    pub(crate) call_id: String,
    pub(crate) tool_name: String,
    pub(crate) command: Vec<String>,
    pub(crate) cwd: AbsolutePathBuf,
    pub(crate) sandbox_permissions: crate::sandboxing::SandboxPermissions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) additional_permissions: Option<AdditionalPermissionProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) justification: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tty: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GuardianMcpAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) destructive_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) open_world_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) read_only_hint: Option<bool>,
}

#[derive(Serialize)]
struct CommandApprovalAction<'a> {
    tool: &'a str,
    command: &'a [String],
    cwd: &'a Path,
    sandbox_permissions: crate::sandboxing::SandboxPermissions,
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_permissions: Option<&'a AdditionalPermissionProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    justification: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tty: Option<bool>,
}

#[cfg(unix)]
#[derive(Serialize)]
struct ExecveApprovalAction<'a> {
    tool: &'a str,
    program: &'a str,
    argv: &'a [String],
    cwd: &'a Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_permissions: Option<&'a AdditionalPermissionProfile>,
}

#[derive(Serialize)]
struct McpToolCallApprovalAction<'a> {
    tool: &'static str,
    server: &'a str,
    tool_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connector_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connector_name: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connector_description: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_title: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_description: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    annotations: Option<&'a GuardianMcpAnnotations>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkAccessApprovalAction<'a> {
    tool: &'static str,
    target: &'a str,
    host: &'a str,
    protocol: NetworkApprovalProtocol,
    port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger: Option<&'a GuardianNetworkAccessTrigger>,
}

#[derive(Serialize)]
struct RequestPermissionsApprovalAction<'a> {
    tool: &'static str,
    turn_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a String>,
    permissions: &'a RequestPermissionProfile,
}

fn serialize_guardian_action(value: impl Serialize) -> serde_json::Result<Value> {
    serde_json::to_value(value)
}

fn serialize_command_guardian_action(
    tool: &'static str,
    command: &[String],
    cwd: &Path,
    sandbox_permissions: crate::sandboxing::SandboxPermissions,
    additional_permissions: Option<&AdditionalPermissionProfile>,
    justification: Option<&String>,
    tty: Option<bool>,
) -> serde_json::Result<Value> {
    serialize_guardian_action(CommandApprovalAction {
        tool,
        command,
        cwd,
        sandbox_permissions,
        additional_permissions,
        justification,
        tty,
    })
}

fn command_assessment_action(
    source: GuardianCommandSource,
    command: &[String],
    cwd: &AbsolutePathBuf,
) -> GuardianAssessmentAction {
    GuardianAssessmentAction::Command {
        source,
        command: codex_shell_command::parse_command::shlex_join(command),
        cwd: cwd.clone(),
    }
}

#[cfg(unix)]
fn guardian_command_source_tool_name(source: GuardianCommandSource) -> &'static str {
    match source {
        GuardianCommandSource::Shell => "shell",
        GuardianCommandSource::UnifiedExec => "exec_command",
    }
}

fn truncate_guardian_action_value(value: Value) -> (Value, bool) {
    match value {
        Value::String(text) => {
            let (text, truncated) =
                guardian_truncate_text(&text, GUARDIAN_MAX_ACTION_STRING_TOKENS);
            (Value::String(text), truncated)
        }
        Value::Array(values) => {
            let mut truncated = false;
            let values = values
                .into_iter()
                .map(|value| {
                    let (value, value_truncated) = truncate_guardian_action_value(value);
                    truncated |= value_truncated;
                    value
                })
                .collect::<Vec<_>>();
            (Value::Array(values), truncated)
        }
        Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            let mut truncated = false;
            let values = entries
                .into_iter()
                .map(|(key, value)| {
                    let (value, value_truncated) = truncate_guardian_action_value(value);
                    truncated |= value_truncated;
                    (key, value)
                })
                .collect();
            (Value::Object(values), truncated)
        }
        other => (other, false),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FormattedGuardianAction {
    pub(crate) text: String,
    pub(crate) truncated: bool,
}

pub(crate) fn guardian_approval_request_to_json(
    action: &GuardianApprovalRequest,
) -> serde_json::Result<Value> {
    match action {
        GuardianApprovalRequest::Shell {
            id: _,
            command,
            cwd,
            sandbox_permissions,
            additional_permissions,
            justification,
            ..
        } => serialize_command_guardian_action(
            "shell",
            command,
            cwd,
            *sandbox_permissions,
            additional_permissions.as_ref(),
            justification.as_ref(),
            /*tty*/ None,
        ),
        GuardianApprovalRequest::ExecCommand {
            id: _,
            command,
            cwd,
            sandbox_permissions,
            additional_permissions,
            justification,
            tty,
            ..
        } => serialize_command_guardian_action(
            "exec_command",
            command,
            cwd,
            *sandbox_permissions,
            additional_permissions.as_ref(),
            justification.as_ref(),
            Some(*tty),
        ),
        #[cfg(unix)]
        GuardianApprovalRequest::Execve {
            id: _,
            source,
            program,
            argv,
            cwd,
            additional_permissions,
        } => serialize_guardian_action(ExecveApprovalAction {
            tool: guardian_command_source_tool_name(*source),
            program,
            argv,
            cwd,
            additional_permissions: additional_permissions.as_ref(),
        }),
        GuardianApprovalRequest::ApplyPatch {
            id: _,
            cwd,
            files,
            changes: _,
            patch,
        } => Ok(serde_json::json!({
            "tool": "apply_patch",
            "cwd": cwd,
            "files": files,
            "patch": patch,
        })),
        GuardianApprovalRequest::NetworkAccess {
            id: _,
            turn_id: _,
            target,
            host,
            protocol,
            port,
            trigger,
            ..
        } => serialize_guardian_action(NetworkAccessApprovalAction {
            tool: "network_access",
            target,
            host,
            protocol: *protocol,
            port: *port,
            trigger: trigger.as_ref(),
        }),
        GuardianApprovalRequest::McpToolCall {
            id: _,
            server,
            tool_name,
            arguments,
            connector_id,
            connector_name,
            connector_description,
            tool_title,
            tool_description,
            annotations,
            ..
        } => serialize_guardian_action(McpToolCallApprovalAction {
            tool: "mcp_tool_call",
            server,
            tool_name,
            arguments: arguments.as_ref(),
            connector_id: connector_id.as_ref(),
            connector_name: connector_name.as_ref(),
            connector_description: connector_description.as_ref(),
            tool_title: tool_title.as_ref(),
            tool_description: tool_description.as_ref(),
            annotations: annotations.as_ref(),
        }),
        GuardianApprovalRequest::RequestPermissions {
            id: _,
            turn_id,
            reason,
            permissions,
            ..
        } => serialize_guardian_action(RequestPermissionsApprovalAction {
            tool: "request_permissions",
            turn_id,
            reason: reason.as_ref(),
            permissions,
        }),
    }
}

pub(crate) fn guardian_assessment_action(
    action: &GuardianApprovalRequest,
) -> GuardianAssessmentAction {
    match action {
        GuardianApprovalRequest::Shell { command, cwd, .. } => {
            command_assessment_action(GuardianCommandSource::Shell, command, cwd)
        }
        GuardianApprovalRequest::ExecCommand { command, cwd, .. } => {
            command_assessment_action(GuardianCommandSource::UnifiedExec, command, cwd)
        }
        #[cfg(unix)]
        GuardianApprovalRequest::Execve {
            source,
            program,
            argv,
            cwd,
            ..
        } => GuardianAssessmentAction::Execve {
            source: *source,
            program: program.clone(),
            argv: argv.clone(),
            cwd: cwd.clone(),
        },
        GuardianApprovalRequest::ApplyPatch { cwd, files, .. } => {
            GuardianAssessmentAction::ApplyPatch {
                cwd: cwd.clone(),
                files: files.clone(),
            }
        }
        GuardianApprovalRequest::NetworkAccess {
            id: _id,
            turn_id: _turn_id,
            target,
            host,
            protocol,
            port,
            trigger: _trigger,
            ..
        } => GuardianAssessmentAction::NetworkAccess {
            target: target.clone(),
            host: host.clone(),
            protocol: *protocol,
            port: *port,
        },
        GuardianApprovalRequest::McpToolCall {
            server,
            tool_name,
            connector_id,
            connector_name,
            tool_title,
            ..
        } => GuardianAssessmentAction::McpToolCall {
            server: server.clone(),
            tool_name: tool_name.clone(),
            connector_id: connector_id.clone(),
            connector_name: connector_name.clone(),
            tool_title: tool_title.clone(),
        },
        GuardianApprovalRequest::RequestPermissions {
            reason,
            permissions,
            ..
        } => GuardianAssessmentAction::RequestPermissions {
            reason: reason.clone(),
            permissions: permissions.clone(),
        },
    }
}

pub(crate) fn guardian_reviewed_action(
    request: &GuardianApprovalRequest,
) -> GuardianReviewedAction {
    match request {
        GuardianApprovalRequest::Shell {
            sandbox_permissions,
            additional_permissions,
            ..
        } => GuardianReviewedAction::Shell {
            sandbox_permissions: *sandbox_permissions,
            additional_permissions: additional_permissions.clone(),
        },
        GuardianApprovalRequest::ExecCommand {
            sandbox_permissions,
            additional_permissions,
            tty,
            ..
        } => GuardianReviewedAction::UnifiedExec {
            sandbox_permissions: *sandbox_permissions,
            additional_permissions: additional_permissions.clone(),
            tty: *tty,
        },
        #[cfg(unix)]
        GuardianApprovalRequest::Execve {
            source,
            program,
            additional_permissions,
            ..
        } => GuardianReviewedAction::Execve {
            source: *source,
            program: program.clone(),
            additional_permissions: additional_permissions.clone(),
        },
        GuardianApprovalRequest::ApplyPatch { .. } => GuardianReviewedAction::ApplyPatch {},
        GuardianApprovalRequest::NetworkAccess { protocol, port, .. } => {
            GuardianReviewedAction::NetworkAccess {
                protocol: *protocol,
                port: *port,
            }
        }
        GuardianApprovalRequest::McpToolCall {
            server,
            tool_name,
            connector_id,
            connector_name,
            tool_title,
            ..
        } => GuardianReviewedAction::McpToolCall {
            server: server.clone(),
            tool_name: tool_name.clone(),
            connector_id: connector_id.clone(),
            connector_name: connector_name.clone(),
            tool_title: tool_title.clone(),
        },
        GuardianApprovalRequest::RequestPermissions { .. } => {
            GuardianReviewedAction::RequestPermissions {}
        }
    }
}

pub(crate) fn guardian_request_target_item_id(request: &GuardianApprovalRequest) -> Option<&str> {
    match request {
        GuardianApprovalRequest::Shell { id, .. }
        | GuardianApprovalRequest::ExecCommand { id, .. }
        | GuardianApprovalRequest::ApplyPatch { id, .. }
        | GuardianApprovalRequest::McpToolCall { id, .. }
        | GuardianApprovalRequest::RequestPermissions { id, .. } => Some(id),
        GuardianApprovalRequest::NetworkAccess { .. } => None,
        #[cfg(unix)]
        GuardianApprovalRequest::Execve { id, .. } => Some(id),
    }
}

pub(crate) fn guardian_request_turn_id<'a>(
    request: &'a GuardianApprovalRequest,
    default_turn_id: &'a str,
) -> &'a str {
    match request {
        GuardianApprovalRequest::NetworkAccess { turn_id, .. }
        | GuardianApprovalRequest::RequestPermissions { turn_id, .. } => turn_id,
        GuardianApprovalRequest::Shell { .. }
        | GuardianApprovalRequest::ExecCommand { .. }
        | GuardianApprovalRequest::ApplyPatch { .. }
        | GuardianApprovalRequest::McpToolCall { .. } => default_turn_id,
        #[cfg(unix)]
        GuardianApprovalRequest::Execve { .. } => default_turn_id,
    }
}

pub(crate) fn format_guardian_action_pretty(
    action: &GuardianApprovalRequest,
) -> serde_json::Result<FormattedGuardianAction> {
    let value = guardian_approval_request_to_json(action)?;
    let (value, truncated) = truncate_guardian_action_value(value);
    Ok(FormattedGuardianAction {
        text: serde_json::to_string_pretty(&value)?,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::FileChange;
    use codex_utils_absolute_path::test_support::PathBufExt;
    use codex_utils_absolute_path::test_support::test_path_buf;
    use pretty_assertions::assert_eq;

    #[test]
    fn exec_approval_event_is_projected_from_shell_request() {
        let request = GuardianApprovalRequest::Shell {
            id: "call-1".to_string(),
            command: vec!["echo".to_string(), "hi".to_string()],
            hook_command: "echo hi".to_string(),
            cwd: test_path_buf("/tmp").abs(),
            sandbox_permissions: crate::sandboxing::SandboxPermissions::UseDefault,
            additional_permissions: None,
            justification: Some("because".to_string()),
        };

        let event = request
            .exec_approval_event(
                "turn-1".to_string(),
                Some("approval-1".to_string()),
                Some("retry".to_string()),
                /*network_approval_context*/ None,
                /*proposed_execpolicy_amendment*/ None,
                /*proposed_network_policy_amendments*/ None,
                Some(vec![ReviewDecision::Approved, ReviewDecision::Abort]),
                /*fallback_cwd*/ None,
            )
            .expect("exec approval event");

        assert_eq!(event.call_id, "call-1");
        assert_eq!(event.approval_id.as_deref(), Some("approval-1"));
        assert_eq!(event.turn_id, "turn-1");
        assert_eq!(event.command, vec!["echo".to_string(), "hi".to_string()]);
        assert_eq!(event.reason.as_deref(), Some("retry"));
        assert_eq!(
            event.available_decisions,
            Some(vec![ReviewDecision::Approved, ReviewDecision::Abort])
        );
    }

    #[test]
    fn apply_patch_approval_event_is_projected_from_request() {
        let path = test_path_buf("/tmp/file.txt");
        let abs_path = path.abs();
        let request = GuardianApprovalRequest::ApplyPatch {
            id: "call-1".to_string(),
            cwd: test_path_buf("/tmp").abs(),
            files: vec![abs_path],
            changes: HashMap::from([(
                path.clone(),
                FileChange::Add {
                    content: "hello".to_string(),
                },
            )]),
            patch: "*** Begin Patch".to_string(),
        };

        let event = request
            .apply_patch_approval_event(
                "turn-1".to_string(),
                Some("needs write".to_string()),
                /*grant_root*/ None,
            )
            .expect("apply_patch approval event");

        assert_eq!(event.call_id, "call-1");
        assert_eq!(event.turn_id, "turn-1");
        assert_eq!(event.reason.as_deref(), Some("needs write"));
        assert_eq!(
            event.changes,
            HashMap::from([(
                path,
                FileChange::Add {
                    content: "hello".to_string(),
                },
            )])
        );
    }

    #[test]
    fn request_permissions_event_is_projected_from_request() {
        let request = GuardianApprovalRequest::RequestPermissions {
            id: "call-1".to_string(),
            turn_id: "turn-1".to_string(),
            reason: Some("need outbound network".to_string()),
            permissions: RequestPermissionProfile {
                network: Some(codex_protocol::models::NetworkPermissions {
                    enabled: Some(true),
                }),
                file_system: None,
            },
            cwd: test_path_buf("/tmp").abs(),
        };

        let event = request
            .request_permissions_event()
            .expect("request_permissions event");

        assert_eq!(event.call_id, "call-1");
        assert_eq!(event.turn_id, "turn-1");
        assert_eq!(event.reason.as_deref(), Some("need outbound network"));
        assert_eq!(
            event.permissions,
            RequestPermissionProfile {
                network: Some(codex_protocol::models::NetworkPermissions {
                    enabled: Some(true),
                }),
                file_system: None,
            }
        );
        assert_eq!(event.cwd, Some(test_path_buf("/tmp").abs()));
    }

    #[test]
    fn network_exec_approval_event_is_projected_from_request() {
        let request = GuardianApprovalRequest::NetworkAccess {
            id: "network-1".to_string(),
            turn_id: "turn-1".to_string(),
            target: "https://example.com:443".to_string(),
            hook_command: "curl https://example.com".to_string(),
            host: "example.com".to_string(),
            protocol: NetworkApprovalProtocol::Https,
            port: 443,
            trigger: None,
        };

        let event = request
            .exec_approval_event(
                "ignored-turn".to_string(),
                /*approval_id*/ None,
                Some("need network".to_string()),
                /*network_approval_context*/ None,
                /*proposed_execpolicy_amendment*/ None,
                /*proposed_network_policy_amendments*/ None,
                /*available_decisions*/ None,
                Some(test_path_buf("/tmp").abs()),
            )
            .expect("network exec approval event");

        assert_eq!(event.call_id, "network-1");
        assert_eq!(event.turn_id, "turn-1");
        assert_eq!(
            event.command,
            vec![
                "network-access".to_string(),
                "https://example.com:443".to_string()
            ]
        );
        assert_eq!(event.cwd, test_path_buf("/tmp").abs());
        assert_eq!(event.reason.as_deref(), Some("need network"));
        assert_eq!(
            event.network_approval_context,
            Some(NetworkApprovalContext {
                host: "example.com".to_string(),
                protocol: NetworkApprovalProtocol::Https,
            })
        );
        assert_eq!(
            event.proposed_network_policy_amendments,
            Some(vec![
                NetworkPolicyAmendment {
                    host: "example.com".to_string(),
                    action: codex_protocol::approvals::NetworkPolicyRuleAction::Allow,
                },
                NetworkPolicyAmendment {
                    host: "example.com".to_string(),
                    action: codex_protocol::approvals::NetworkPolicyRuleAction::Deny,
                },
            ])
        );
    }
}
