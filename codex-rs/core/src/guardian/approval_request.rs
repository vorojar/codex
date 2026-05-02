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
use codex_protocol::request_user_input::RequestUserInputAnswer;
use codex_protocol::request_user_input::RequestUserInputQuestion;
use codex_protocol::request_user_input::RequestUserInputQuestionOption;
use codex_protocol::request_user_input::RequestUserInputResponse;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Serialize;
use serde_json::Value;

use super::GUARDIAN_MAX_ACTION_STRING_TOKENS;
use super::prompt::guardian_truncate_text;
use crate::mcp_tool_approval_templates::RenderedMcpToolApprovalParam;
use crate::mcp_tool_approval_templates::render_mcp_tool_approval_template;
use crate::tools::hook_names::HookToolName;
use crate::tools::sandboxing::PermissionRequestPayload;

/// Canonical description of an approval-worthy action in core.
///
/// This type should describe the action being reviewed exactly once, with
/// guardian review, approval hooks, and user-prompt transports deriving their
/// own projections from it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ApprovalRequest {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct McpToolApprovalPromptOptions {
    pub(crate) allow_session_remember: bool,
    pub(crate) allow_persistent_approval: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct McpToolApprovalPrompt {
    pub(crate) question: RequestUserInputQuestion,
    pub(crate) message_override: Option<String>,
    pub(crate) tool_params: Option<Value>,
    pub(crate) tool_params_display: Option<Vec<RenderedMcpToolApprovalParam>>,
}

pub(crate) const MCP_TOOL_APPROVAL_ACCEPT: &str = "Allow";
pub(crate) const MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION: &str = "Allow for this session";
pub(crate) const MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER: &str = "Allow and don't ask me again";
pub(crate) const MCP_TOOL_APPROVAL_CANCEL: &str = "Cancel";
pub(crate) const MCP_TOOL_APPROVAL_DECLINE_SYNTHETIC: &str = "__codex_mcp_decline__";
pub(crate) const MCP_TOOL_APPROVAL_QUESTION_ID_PREFIX: &str = "mcp_tool_call_approval";
pub(crate) const MCP_TOOL_APPROVAL_KIND_KEY: &str = "codex_approval_kind";
pub(crate) const MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL: &str = "mcp_tool_call";
pub(crate) const MCP_TOOL_APPROVAL_PERSIST_KEY: &str = "persist";
pub(crate) const MCP_TOOL_APPROVAL_PERSIST_SESSION: &str = "session";
pub(crate) const MCP_TOOL_APPROVAL_PERSIST_ALWAYS: &str = "always";
pub(crate) const MCP_TOOL_APPROVAL_SOURCE_KEY: &str = "source";
pub(crate) const MCP_TOOL_APPROVAL_SOURCE_CONNECTOR: &str = "connector";
pub(crate) const MCP_TOOL_APPROVAL_CONNECTOR_ID_KEY: &str = "connector_id";
pub(crate) const MCP_TOOL_APPROVAL_CONNECTOR_NAME_KEY: &str = "connector_name";
pub(crate) const MCP_TOOL_APPROVAL_CONNECTOR_DESCRIPTION_KEY: &str = "connector_description";
pub(crate) const MCP_TOOL_APPROVAL_TOOL_TITLE_KEY: &str = "tool_title";
pub(crate) const MCP_TOOL_APPROVAL_TOOL_DESCRIPTION_KEY: &str = "tool_description";
pub(crate) const MCP_TOOL_APPROVAL_TOOL_PARAMS_KEY: &str = "tool_params";
pub(crate) const MCP_TOOL_APPROVAL_TOOL_PARAMS_DISPLAY_KEY: &str = "tool_params_display";

impl ApprovalRequest {
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

    pub(crate) fn mcp_tool_approval_prompt(
        &self,
        question_id: String,
        prompt_options: McpToolApprovalPromptOptions,
        monitor_reason: Option<&str>,
    ) -> Option<McpToolApprovalPrompt> {
        let Self::McpToolCall {
            server,
            tool_name,
            arguments,
            connector_id,
            connector_name,
            tool_title,
            ..
        } = self
        else {
            return None;
        };

        let rendered_template = render_mcp_tool_approval_template(
            server,
            connector_id.as_deref(),
            connector_name.as_deref(),
            tool_title.as_deref(),
            arguments.as_ref(),
        );
        let tool_params_display = rendered_template
            .as_ref()
            .map(|rendered_template| rendered_template.tool_params_display.clone())
            .or_else(|| build_mcp_tool_approval_display_params(arguments.as_ref()));
        let question_override = rendered_template
            .as_ref()
            .map(|rendered_template| rendered_template.question.as_str());
        let mut question = build_mcp_tool_approval_question(
            question_id,
            server,
            tool_name,
            connector_name.as_deref(),
            prompt_options,
            question_override,
        );
        question.question = mcp_tool_approval_question_text(question.question, monitor_reason);

        Some(McpToolApprovalPrompt {
            question,
            message_override: rendered_template.as_ref().and_then(|rendered_template| {
                monitor_reason
                    .is_none()
                    .then_some(rendered_template.elicitation_message.clone())
            }),
            tool_params: rendered_template
                .as_ref()
                .and_then(|rendered_template| rendered_template.tool_params.clone())
                .or_else(|| arguments.clone()),
            tool_params_display,
        })
    }

    pub(crate) fn mcp_tool_approval_elicitation_meta(
        &self,
        tool_params: Option<&Value>,
        tool_params_display: Option<&[RenderedMcpToolApprovalParam]>,
        prompt_options: McpToolApprovalPromptOptions,
    ) -> Option<Value> {
        let Self::McpToolCall {
            server,
            connector_id,
            connector_name,
            connector_description,
            tool_title,
            tool_description,
            ..
        } = self
        else {
            return None;
        };

        let mut meta = serde_json::Map::new();
        meta.insert(
            MCP_TOOL_APPROVAL_KIND_KEY.to_string(),
            serde_json::Value::String(MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL.to_string()),
        );
        match (
            prompt_options.allow_session_remember,
            prompt_options.allow_persistent_approval,
        ) {
            (true, true) => {
                meta.insert(
                    MCP_TOOL_APPROVAL_PERSIST_KEY.to_string(),
                    serde_json::json!([
                        MCP_TOOL_APPROVAL_PERSIST_SESSION,
                        MCP_TOOL_APPROVAL_PERSIST_ALWAYS,
                    ]),
                );
            }
            (true, false) => {
                meta.insert(
                    MCP_TOOL_APPROVAL_PERSIST_KEY.to_string(),
                    serde_json::Value::String(MCP_TOOL_APPROVAL_PERSIST_SESSION.to_string()),
                );
            }
            (false, true) => {
                meta.insert(
                    MCP_TOOL_APPROVAL_PERSIST_KEY.to_string(),
                    serde_json::Value::String(MCP_TOOL_APPROVAL_PERSIST_ALWAYS.to_string()),
                );
            }
            (false, false) => {}
        }
        if let Some(tool_title) = tool_title.as_ref() {
            meta.insert(
                MCP_TOOL_APPROVAL_TOOL_TITLE_KEY.to_string(),
                serde_json::Value::String(tool_title.clone()),
            );
        }
        if let Some(tool_description) = tool_description.as_ref() {
            meta.insert(
                MCP_TOOL_APPROVAL_TOOL_DESCRIPTION_KEY.to_string(),
                serde_json::Value::String(tool_description.clone()),
            );
        }
        if server == "codex_apps"
            && (connector_id.is_some()
                || connector_name.is_some()
                || connector_description.is_some())
        {
            meta.insert(
                MCP_TOOL_APPROVAL_SOURCE_KEY.to_string(),
                serde_json::Value::String(MCP_TOOL_APPROVAL_SOURCE_CONNECTOR.to_string()),
            );
            if let Some(connector_id) = connector_id.as_deref() {
                meta.insert(
                    MCP_TOOL_APPROVAL_CONNECTOR_ID_KEY.to_string(),
                    serde_json::Value::String(connector_id.to_string()),
                );
            }
            if let Some(connector_name) = connector_name.as_ref() {
                meta.insert(
                    MCP_TOOL_APPROVAL_CONNECTOR_NAME_KEY.to_string(),
                    serde_json::Value::String(connector_name.clone()),
                );
            }
            if let Some(connector_description) = connector_description.as_ref() {
                meta.insert(
                    MCP_TOOL_APPROVAL_CONNECTOR_DESCRIPTION_KEY.to_string(),
                    serde_json::Value::String(connector_description.clone()),
                );
            }
        }
        if let Some(tool_params) = tool_params {
            meta.insert(
                MCP_TOOL_APPROVAL_TOOL_PARAMS_KEY.to_string(),
                tool_params.clone(),
            );
        }
        if let Some(tool_params_display) = tool_params_display
            && let Ok(tool_params_display) = serde_json::to_value(tool_params_display)
        {
            meta.insert(
                MCP_TOOL_APPROVAL_TOOL_PARAMS_DISPLAY_KEY.to_string(),
                tool_params_display,
            );
        }

        (!meta.is_empty()).then_some(serde_json::Value::Object(meta))
    }

    pub(crate) fn mcp_tool_approval_compat_response(
        &self,
        question: &RequestUserInputQuestion,
        decision: ReviewDecision,
    ) -> Option<RequestUserInputResponse> {
        let Self::McpToolCall { .. } = self else {
            return None;
        };

        let selected_label = match decision {
            ReviewDecision::ApprovedForSession => question
                .options
                .as_ref()
                .and_then(|options| {
                    options
                        .iter()
                        .find(|option| option.label == MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION)
                })
                .map(|option| option.label.clone())
                .unwrap_or_else(|| MCP_TOOL_APPROVAL_ACCEPT.to_string()),
            ReviewDecision::Approved
            | ReviewDecision::ApprovedExecpolicyAmendment { .. }
            | ReviewDecision::NetworkPolicyAmendment { .. } => MCP_TOOL_APPROVAL_ACCEPT.to_string(),
            ReviewDecision::Denied | ReviewDecision::TimedOut | ReviewDecision::Abort => {
                MCP_TOOL_APPROVAL_DECLINE_SYNTHETIC.to_string()
            }
        };

        Some(RequestUserInputResponse {
            answers: HashMap::from([(
                question.id.clone(),
                RequestUserInputAnswer {
                    answers: vec![selected_label],
                },
            )]),
        })
    }
}

pub(crate) fn mcp_tool_approval_question_id(call_id: &str) -> String {
    format!("{MCP_TOOL_APPROVAL_QUESTION_ID_PREFIX}_{call_id}")
}

pub(crate) fn is_mcp_tool_approval_question_id(question_id: &str) -> bool {
    question_id
        .strip_prefix(MCP_TOOL_APPROVAL_QUESTION_ID_PREFIX)
        .is_some_and(|suffix| suffix.starts_with('_'))
}

fn build_mcp_tool_approval_question(
    question_id: String,
    server: &str,
    tool_name: &str,
    connector_name: Option<&str>,
    prompt_options: McpToolApprovalPromptOptions,
    question_override: Option<&str>,
) -> RequestUserInputQuestion {
    let question = question_override
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            build_mcp_tool_approval_fallback_message(server, tool_name, connector_name)
        });
    let question = format!("{}?", question.trim_end_matches('?'));

    let mut options = vec![RequestUserInputQuestionOption {
        label: MCP_TOOL_APPROVAL_ACCEPT.to_string(),
        description: "Run the tool and continue.".to_string(),
    }];
    if prompt_options.allow_session_remember {
        options.push(RequestUserInputQuestionOption {
            label: MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION.to_string(),
            description: "Run the tool and remember this choice for this session.".to_string(),
        });
    }
    if prompt_options.allow_persistent_approval {
        options.push(RequestUserInputQuestionOption {
            label: MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER.to_string(),
            description: "Run the tool and remember this choice for future tool calls.".to_string(),
        });
    }
    options.push(RequestUserInputQuestionOption {
        label: MCP_TOOL_APPROVAL_CANCEL.to_string(),
        description: "Cancel this tool call.".to_string(),
    });

    RequestUserInputQuestion {
        id: question_id,
        header: "Approve app tool call?".to_string(),
        question,
        is_other: false,
        is_secret: false,
        options: Some(options),
    }
}

fn build_mcp_tool_approval_fallback_message(
    server: &str,
    tool_name: &str,
    connector_name: Option<&str>,
) -> String {
    let actor = connector_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            if server == "codex_apps" {
                "this app".to_string()
            } else {
                format!("the {server} MCP server")
            }
        });
    format!("Allow {actor} to run tool \"{tool_name}\"?")
}

fn mcp_tool_approval_question_text(question: String, monitor_reason: Option<&str>) -> String {
    match monitor_reason.map(str::trim) {
        Some(reason) if !reason.is_empty() => {
            format!("Tool call needs your approval. Reason: {reason}")
        }
        _ => question,
    }
}

fn build_mcp_tool_approval_display_params(
    tool_params: Option<&Value>,
) -> Option<Vec<RenderedMcpToolApprovalParam>> {
    let tool_params = tool_params?.as_object()?;
    let mut display_params = tool_params
        .iter()
        .map(|(name, value)| RenderedMcpToolApprovalParam {
            name: name.clone(),
            value: value.clone(),
            display_name: name.clone(),
        })
        .collect::<Vec<_>>();
    display_params.sort_by(|left, right| left.name.cmp(&right.name));
    Some(display_params)
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
    action: &ApprovalRequest,
) -> serde_json::Result<Value> {
    match action {
        ApprovalRequest::Shell {
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
        ApprovalRequest::ExecCommand {
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
        ApprovalRequest::Execve {
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
        ApprovalRequest::ApplyPatch {
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
        ApprovalRequest::NetworkAccess {
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
        ApprovalRequest::McpToolCall {
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
        ApprovalRequest::RequestPermissions {
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

pub(crate) fn guardian_assessment_action(action: &ApprovalRequest) -> GuardianAssessmentAction {
    match action {
        ApprovalRequest::Shell { command, cwd, .. } => {
            command_assessment_action(GuardianCommandSource::Shell, command, cwd)
        }
        ApprovalRequest::ExecCommand { command, cwd, .. } => {
            command_assessment_action(GuardianCommandSource::UnifiedExec, command, cwd)
        }
        #[cfg(unix)]
        ApprovalRequest::Execve {
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
        ApprovalRequest::ApplyPatch { cwd, files, .. } => GuardianAssessmentAction::ApplyPatch {
            cwd: cwd.clone(),
            files: files.clone(),
        },
        ApprovalRequest::NetworkAccess {
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
        ApprovalRequest::McpToolCall {
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
        ApprovalRequest::RequestPermissions {
            reason,
            permissions,
            ..
        } => GuardianAssessmentAction::RequestPermissions {
            reason: reason.clone(),
            permissions: permissions.clone(),
        },
    }
}

pub(crate) fn guardian_reviewed_action(request: &ApprovalRequest) -> GuardianReviewedAction {
    match request {
        ApprovalRequest::Shell {
            sandbox_permissions,
            additional_permissions,
            ..
        } => GuardianReviewedAction::Shell {
            sandbox_permissions: *sandbox_permissions,
            additional_permissions: additional_permissions.clone(),
        },
        ApprovalRequest::ExecCommand {
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
        ApprovalRequest::Execve {
            source,
            program,
            additional_permissions,
            ..
        } => GuardianReviewedAction::Execve {
            source: *source,
            program: program.clone(),
            additional_permissions: additional_permissions.clone(),
        },
        ApprovalRequest::ApplyPatch { .. } => GuardianReviewedAction::ApplyPatch {},
        ApprovalRequest::NetworkAccess { protocol, port, .. } => {
            GuardianReviewedAction::NetworkAccess {
                protocol: *protocol,
                port: *port,
            }
        }
        ApprovalRequest::McpToolCall {
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
        ApprovalRequest::RequestPermissions { .. } => GuardianReviewedAction::RequestPermissions {},
    }
}

pub(crate) fn guardian_request_target_item_id(request: &ApprovalRequest) -> Option<&str> {
    match request {
        ApprovalRequest::Shell { id, .. }
        | ApprovalRequest::ExecCommand { id, .. }
        | ApprovalRequest::ApplyPatch { id, .. }
        | ApprovalRequest::McpToolCall { id, .. }
        | ApprovalRequest::RequestPermissions { id, .. } => Some(id),
        ApprovalRequest::NetworkAccess { .. } => None,
        #[cfg(unix)]
        ApprovalRequest::Execve { id, .. } => Some(id),
    }
}

pub(crate) fn guardian_request_turn_id<'a>(
    request: &'a ApprovalRequest,
    default_turn_id: &'a str,
) -> &'a str {
    match request {
        ApprovalRequest::NetworkAccess { turn_id, .. }
        | ApprovalRequest::RequestPermissions { turn_id, .. } => turn_id,
        ApprovalRequest::Shell { .. }
        | ApprovalRequest::ExecCommand { .. }
        | ApprovalRequest::ApplyPatch { .. }
        | ApprovalRequest::McpToolCall { .. } => default_turn_id,
        #[cfg(unix)]
        ApprovalRequest::Execve { .. } => default_turn_id,
    }
}

pub(crate) fn format_guardian_action_pretty(
    action: &ApprovalRequest,
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
    use codex_protocol::protocol::ReviewDecision;
    use codex_utils_absolute_path::test_support::PathBufExt;
    use codex_utils_absolute_path::test_support::test_path_buf;
    use pretty_assertions::assert_eq;

    #[test]
    fn exec_approval_event_is_projected_from_shell_request() {
        let request = ApprovalRequest::Shell {
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
        let request = ApprovalRequest::ApplyPatch {
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
        let request = ApprovalRequest::RequestPermissions {
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
        let request = ApprovalRequest::NetworkAccess {
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

    #[test]
    fn mcp_tool_approval_compat_response_uses_session_label_when_present() {
        let request = ApprovalRequest::McpToolCall {
            id: "call-1".to_string(),
            server: "custom_server".to_string(),
            tool_name: "dangerous_tool".to_string(),
            hook_tool_name: "custom_server__dangerous_tool".to_string(),
            arguments: None,
            connector_id: None,
            connector_name: None,
            connector_description: None,
            tool_title: None,
            tool_description: None,
            annotations: None,
        };
        let question = RequestUserInputQuestion {
            id: "q-1".to_string(),
            header: "Approve app tool call?".to_string(),
            question: "Allow this app tool?".to_string(),
            is_other: false,
            is_secret: false,
            options: Some(vec![RequestUserInputQuestionOption {
                label: MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION.to_string(),
                description: "Remember until session ends".to_string(),
            }]),
        };

        let response = request
            .mcp_tool_approval_compat_response(&question, ReviewDecision::ApprovedForSession)
            .expect("compat response");

        assert_eq!(
            response.answers.get("q-1"),
            Some(&RequestUserInputAnswer {
                answers: vec![MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION.to_string()],
            })
        );
    }

    #[test]
    fn mcp_tool_approval_compat_response_uses_synthetic_decline_for_abort() {
        let request = ApprovalRequest::McpToolCall {
            id: "call-1".to_string(),
            server: "custom_server".to_string(),
            tool_name: "dangerous_tool".to_string(),
            hook_tool_name: "custom_server__dangerous_tool".to_string(),
            arguments: None,
            connector_id: None,
            connector_name: None,
            connector_description: None,
            tool_title: None,
            tool_description: None,
            annotations: None,
        };
        let question = RequestUserInputQuestion {
            id: "q-1".to_string(),
            header: "Approve app tool call?".to_string(),
            question: "Allow this app tool?".to_string(),
            is_other: false,
            is_secret: false,
            options: None,
        };

        let response = request
            .mcp_tool_approval_compat_response(&question, ReviewDecision::Abort)
            .expect("compat response");

        assert_eq!(
            response.answers.get("q-1"),
            Some(&RequestUserInputAnswer {
                answers: vec![MCP_TOOL_APPROVAL_DECLINE_SYNTHETIC.to_string()],
            })
        );
    }

    #[test]
    fn mcp_tool_approval_question_id_helpers_round_trip() {
        let question_id = mcp_tool_approval_question_id("call-1");

        assert_eq!(question_id, "mcp_tool_call_approval_call-1");
        assert!(is_mcp_tool_approval_question_id(&question_id));
        assert!(!is_mcp_tool_approval_question_id("other_question"));
    }
}
