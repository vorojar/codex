use codex_protocol::ThreadId;
use codex_protocol::models::ShellCommandToolCallParams;
use codex_protocol::models::ShellToolCallParams;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::Value as JsonValue;
use std::sync::Arc;

use crate::exec::ExecCapturePolicy;
use crate::exec::ExecParams;
use crate::exec_env::create_env;
use crate::exec_policy::ExecApprovalRequest;
use crate::function_tool::FunctionCallError;
use crate::maybe_emit_implicit_skill_invocation;
use crate::sandboxing::ExecServerEnvConfig;
use crate::session::turn_context::TurnContext;
use crate::session::turn_context::TurnEnvironment;
use crate::shell::Shell;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::events::ToolEmitter;
use crate::tools::events::ToolEventCtx;
use crate::tools::handlers::apply_granted_turn_permissions;
use crate::tools::handlers::apply_patch::intercept_apply_patch;
use crate::tools::handlers::env_path::resolve_tool_environment;
use crate::tools::handlers::implicit_granted_permissions;
use crate::tools::handlers::normalize_and_validate_additional_permissions;
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::parse_arguments_with_base_path;
use crate::tools::handlers::reject_remote_process_when_sandbox_required;
use crate::tools::handlers::resolve_workdir_base_path;
use crate::tools::hook_names::HookToolName;
use crate::tools::orchestrator::ToolOrchestrator;
use crate::tools::registry::PostToolUsePayload;
use crate::tools::registry::PreToolUsePayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use crate::tools::runtimes::shell::ShellRequest;
use crate::tools::runtimes::shell::ShellRuntime;
use crate::tools::runtimes::shell::ShellRuntimeBackend;
use crate::tools::sandboxing::ToolCtx;
use codex_features::Feature;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::protocol::ExecCommandSource;
use codex_shell_command::is_safe_command::is_known_safe_command;
use codex_tools::ShellCommandBackendConfig;

pub struct ShellHandler;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellCommandBackend {
    Classic,
    ZshFork,
}

pub struct ShellCommandHandler {
    backend: ShellCommandBackend,
}

fn shell_payload_command(payload: &ToolPayload) -> Option<String> {
    match payload {
        ToolPayload::Function { arguments } => parse_arguments::<ShellToolCallParams>(arguments)
            .ok()
            .map(|params| codex_shell_command::parse_command::shlex_join(&params.command)),
        ToolPayload::LocalShell { params } => Some(codex_shell_command::parse_command::shlex_join(
            &params.command,
        )),
        _ => None,
    }
}

fn shell_command_payload_command(payload: &ToolPayload) -> Option<String> {
    let ToolPayload::Function { arguments } = payload else {
        return None;
    };

    parse_arguments::<ShellCommandToolCallParams>(arguments)
        .ok()
        .map(|params| params.command)
}

struct RunExecLikeArgs {
    tool_name: String,
    exec_params: ExecParams,
    hook_command: String,
    environment_id: String,
    environment: Arc<codex_exec_server::Environment>,
    additional_permissions: Option<AdditionalPermissionProfile>,
    prefix_rule: Option<Vec<String>>,
    session: Arc<crate::session::session::Session>,
    turn: Arc<TurnContext>,
    tracker: crate::tools::context::SharedTurnDiffTracker,
    call_id: String,
    freeform: bool,
    shell_runtime_backend: ShellRuntimeBackend,
}

#[derive(Debug, serde::Deserialize)]
struct EnvironmentTargetArgs {
    #[serde(default)]
    environment_id: Option<String>,
}

fn resolve_environment_from_arguments<'a>(
    turn: &'a TurnContext,
    arguments: &str,
    tool_name: &str,
) -> Result<&'a TurnEnvironment, FunctionCallError> {
    let target_args: EnvironmentTargetArgs = parse_arguments(arguments)?;
    resolve_tool_environment(turn, target_args.environment_id.as_deref(), tool_name)
}

impl ShellHandler {
    fn to_exec_params(
        params: &ShellToolCallParams,
        turn_context: &TurnContext,
        thread_id: ThreadId,
        cwd: &AbsolutePathBuf,
    ) -> ExecParams {
        ExecParams {
            command: params.command.clone(),
            cwd: cwd.clone(),
            expiration: params.timeout_ms.into(),
            capture_policy: ExecCapturePolicy::ShellTool,
            env: create_env(&turn_context.shell_environment_policy, Some(thread_id)),
            network: turn_context.network.clone(),
            sandbox_permissions: params.sandbox_permissions.unwrap_or_default(),
            windows_sandbox_level: turn_context.windows_sandbox_level,
            windows_sandbox_private_desktop: turn_context
                .config
                .permissions
                .windows_sandbox_private_desktop,
            justification: params.justification.clone(),
            arg0: None,
        }
    }
}

impl ShellCommandHandler {
    fn shell_runtime_backend(&self) -> ShellRuntimeBackend {
        match self.backend {
            ShellCommandBackend::Classic => ShellRuntimeBackend::ShellCommandClassic,
            ShellCommandBackend::ZshFork => ShellRuntimeBackend::ShellCommandZshFork,
        }
    }

    fn resolve_use_login_shell(
        login: Option<bool>,
        allow_login_shell: bool,
    ) -> Result<bool, FunctionCallError> {
        if !allow_login_shell && login == Some(true) {
            return Err(FunctionCallError::RespondToModel(
                "login shell is disabled by config; omit `login` or set it to false.".to_string(),
            ));
        }

        Ok(login.unwrap_or(allow_login_shell))
    }

    fn base_command(shell: &Shell, command: &str, use_login_shell: bool) -> Vec<String> {
        shell.derive_exec_args(command, use_login_shell)
    }

    fn to_exec_params(
        params: &ShellCommandToolCallParams,
        session: &crate::session::session::Session,
        turn_context: &TurnContext,
        thread_id: ThreadId,
        allow_login_shell: bool,
        cwd: &AbsolutePathBuf,
    ) -> Result<ExecParams, FunctionCallError> {
        let shell = session.user_shell();
        let use_login_shell = Self::resolve_use_login_shell(params.login, allow_login_shell)?;
        let command = Self::base_command(shell.as_ref(), &params.command, use_login_shell);

        Ok(ExecParams {
            command,
            cwd: cwd.clone(),
            expiration: params.timeout_ms.into(),
            capture_policy: ExecCapturePolicy::ShellTool,
            env: create_env(&turn_context.shell_environment_policy, Some(thread_id)),
            network: turn_context.network.clone(),
            sandbox_permissions: params.sandbox_permissions.unwrap_or_default(),
            windows_sandbox_level: turn_context.windows_sandbox_level,
            windows_sandbox_private_desktop: turn_context
                .config
                .permissions
                .windows_sandbox_private_desktop,
            justification: params.justification.clone(),
            arg0: None,
        })
    }
}

impl From<ShellCommandBackendConfig> for ShellCommandHandler {
    fn from(config: ShellCommandBackendConfig) -> Self {
        let backend = match config {
            ShellCommandBackendConfig::Classic => ShellCommandBackend::Classic,
            ShellCommandBackendConfig::ZshFork => ShellCommandBackend::ZshFork,
        };
        Self { backend }
    }
}

impl ToolHandler for ShellHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(
            payload,
            ToolPayload::Function { .. } | ToolPayload::LocalShell { .. }
        )
    }

    async fn is_mutating(&self, invocation: &ToolInvocation) -> bool {
        match &invocation.payload {
            ToolPayload::Function { arguments } => {
                serde_json::from_str::<ShellToolCallParams>(arguments)
                    .map(|params| !is_known_safe_command(&params.command))
                    .unwrap_or(true)
            }
            ToolPayload::LocalShell { params } => !is_known_safe_command(&params.command),
            _ => true, // unknown payloads => assume mutating
        }
    }

    fn pre_tool_use_payload(&self, invocation: &ToolInvocation) -> Option<PreToolUsePayload> {
        shell_payload_command(&invocation.payload).map(|command| PreToolUsePayload {
            tool_name: HookToolName::bash(),
            tool_input: serde_json::json!({ "command": command }),
        })
    }

    fn post_tool_use_payload(
        &self,
        invocation: &ToolInvocation,
        result: &Self::Output,
    ) -> Option<PostToolUsePayload> {
        let tool_response =
            result.post_tool_use_response(&invocation.call_id, &invocation.payload)?;
        let command = shell_payload_command(&invocation.payload)?;
        Some(PostToolUsePayload {
            tool_name: HookToolName::bash(),
            tool_use_id: invocation.call_id.clone(),
            tool_input: serde_json::json!({ "command": command }),
            tool_response,
        })
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            tracker,
            call_id,
            tool_name,
            payload,
            ..
        } = invocation;

        match payload {
            ToolPayload::Function { arguments } => {
                let turn_environment =
                    resolve_environment_from_arguments(turn.as_ref(), &arguments, "shell")?;
                let cwd = &turn_environment.cwd;
                let cwd = resolve_workdir_base_path(&arguments, cwd)?;
                let params: ShellToolCallParams = parse_arguments_with_base_path(&arguments, &cwd)?;
                let prefix_rule = params.prefix_rule.clone();
                let exec_params =
                    Self::to_exec_params(&params, turn.as_ref(), session.conversation_id, &cwd);
                Self::run_exec_like(RunExecLikeArgs {
                    tool_name: tool_name.display(),
                    exec_params,
                    hook_command: codex_shell_command::parse_command::shlex_join(&params.command),
                    environment_id: turn_environment.environment_id.clone(),
                    environment: Arc::clone(&turn_environment.environment),
                    additional_permissions: params.additional_permissions.clone(),
                    prefix_rule,
                    session,
                    turn,
                    tracker,
                    call_id,
                    freeform: false,
                    shell_runtime_backend: ShellRuntimeBackend::Generic,
                })
                .await
            }
            ToolPayload::LocalShell { params } => {
                let turn_environment = resolve_tool_environment(
                    turn.as_ref(),
                    /*environment_id*/ None,
                    "local_shell",
                )?;
                let cwd = params
                    .workdir
                    .as_deref()
                    .filter(|workdir| !workdir.is_empty())
                    .map_or_else(
                        || turn_environment.cwd.clone(),
                        |workdir| turn_environment.cwd.join(workdir),
                    );
                let exec_params =
                    Self::to_exec_params(&params, turn.as_ref(), session.conversation_id, &cwd);
                Self::run_exec_like(RunExecLikeArgs {
                    tool_name: tool_name.display(),
                    exec_params,
                    hook_command: codex_shell_command::parse_command::shlex_join(&params.command),
                    environment_id: turn_environment.environment_id.clone(),
                    environment: Arc::clone(&turn_environment.environment),
                    additional_permissions: None,
                    prefix_rule: None,
                    session,
                    turn,
                    tracker,
                    call_id,
                    freeform: false,
                    shell_runtime_backend: ShellRuntimeBackend::Generic,
                })
                .await
            }
            _ => Err(FunctionCallError::RespondToModel(format!(
                "unsupported payload for shell handler: {}",
                tool_name.display()
            ))),
        }
    }
}

impl ToolHandler for ShellCommandHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }

    async fn is_mutating(&self, invocation: &ToolInvocation) -> bool {
        let ToolPayload::Function { arguments } = &invocation.payload else {
            return true;
        };

        serde_json::from_str::<ShellCommandToolCallParams>(arguments)
            .map(|params| {
                let use_login_shell = match Self::resolve_use_login_shell(
                    params.login,
                    invocation.turn.tools_config.allow_login_shell,
                ) {
                    Ok(use_login_shell) => use_login_shell,
                    Err(_) => return true,
                };
                let shell = invocation.session.user_shell();
                let command = Self::base_command(shell.as_ref(), &params.command, use_login_shell);
                !is_known_safe_command(&command)
            })
            .unwrap_or(true)
    }

    fn pre_tool_use_payload(&self, invocation: &ToolInvocation) -> Option<PreToolUsePayload> {
        shell_command_payload_command(&invocation.payload).map(|command| PreToolUsePayload {
            tool_name: HookToolName::bash(),
            tool_input: serde_json::json!({ "command": command }),
        })
    }

    fn post_tool_use_payload(
        &self,
        invocation: &ToolInvocation,
        result: &Self::Output,
    ) -> Option<PostToolUsePayload> {
        let tool_response =
            result.post_tool_use_response(&invocation.call_id, &invocation.payload)?;
        let command = shell_command_payload_command(&invocation.payload)?;
        Some(PostToolUsePayload {
            tool_name: HookToolName::bash(),
            tool_use_id: invocation.call_id.clone(),
            tool_input: serde_json::json!({ "command": command }),
            tool_response,
        })
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            tracker,
            call_id,
            tool_name,
            payload,
            ..
        } = invocation;

        let ToolPayload::Function { arguments } = payload else {
            return Err(FunctionCallError::RespondToModel(format!(
                "unsupported payload for shell_command handler: {}",
                tool_name.display()
            )));
        };

        let turn_environment =
            resolve_environment_from_arguments(turn.as_ref(), &arguments, "shell_command")?;
        let cwd = &turn_environment.cwd;
        let cwd = resolve_workdir_base_path(&arguments, cwd)?;
        let params: ShellCommandToolCallParams = parse_arguments_with_base_path(&arguments, &cwd)?;
        maybe_emit_implicit_skill_invocation(
            session.as_ref(),
            turn.as_ref(),
            &params.command,
            &cwd,
        )
        .await;
        let prefix_rule = params.prefix_rule.clone();
        let exec_params = Self::to_exec_params(
            &params,
            session.as_ref(),
            turn.as_ref(),
            session.conversation_id,
            turn.tools_config.allow_login_shell,
            &cwd,
        )?;
        ShellHandler::run_exec_like(RunExecLikeArgs {
            tool_name: tool_name.display(),
            exec_params,
            hook_command: params.command,
            environment_id: turn_environment.environment_id.clone(),
            environment: Arc::clone(&turn_environment.environment),
            additional_permissions: params.additional_permissions.clone(),
            prefix_rule,
            session,
            turn,
            tracker,
            call_id,
            freeform: true,
            shell_runtime_backend: self.shell_runtime_backend(),
        })
        .await
    }
}

impl ShellHandler {
    async fn run_exec_like(args: RunExecLikeArgs) -> Result<FunctionToolOutput, FunctionCallError> {
        let RunExecLikeArgs {
            tool_name,
            exec_params,
            hook_command,
            environment_id,
            environment,
            additional_permissions,
            prefix_rule,
            session,
            turn,
            tracker,
            call_id,
            freeform,
            shell_runtime_backend,
        } = args;

        let mut exec_params = exec_params;
        let fs = environment.get_filesystem();
        if environment.is_remote()
            && matches!(
                shell_runtime_backend,
                ShellRuntimeBackend::ShellCommandZshFork
            )
        {
            return Err(FunctionCallError::RespondToModel(
                "shell_command zsh-fork is not supported for remote environments".to_string(),
            ));
        }

        let dependency_env = session.dependency_env().await;
        if !dependency_env.is_empty() {
            exec_params.env.extend(dependency_env.clone());
        }

        let mut explicit_env_overrides = turn.shell_environment_policy.r#set.clone();
        for key in dependency_env.keys() {
            if let Some(value) = exec_params.env.get(key) {
                explicit_env_overrides.insert(key.clone(), value.clone());
            }
        }

        let exec_permission_approvals_enabled =
            session.features().enabled(Feature::ExecPermissionApprovals);
        let requested_additional_permissions = additional_permissions.clone();
        let effective_additional_permissions = apply_granted_turn_permissions(
            session.as_ref(),
            turn.as_ref(),
            &environment_id,
            exec_params.cwd.as_path(),
            exec_params.sandbox_permissions,
            additional_permissions,
        )
        .await;
        let additional_permissions_allowed = exec_permission_approvals_enabled
            || (session.features().enabled(Feature::RequestPermissionsTool)
                && effective_additional_permissions.permissions_preapproved);
        let normalized_additional_permissions = implicit_granted_permissions(
            exec_params.sandbox_permissions,
            requested_additional_permissions.as_ref(),
            &effective_additional_permissions,
        )
        .map_or_else(
            || {
                normalize_and_validate_additional_permissions(
                    additional_permissions_allowed,
                    turn.approval_policy.value(),
                    effective_additional_permissions.sandbox_permissions,
                    effective_additional_permissions.additional_permissions,
                    effective_additional_permissions.permissions_preapproved,
                    &exec_params.cwd,
                )
            },
            |permissions| Ok(Some(permissions)),
        )
        .map_err(FunctionCallError::RespondToModel)?;

        // Approval policy guard for explicit escalation in non-OnRequest modes.
        // Sticky turn permissions have already been approved, so they should
        // continue through the normal exec approval flow for the command.
        if effective_additional_permissions
            .sandbox_permissions
            .requests_sandbox_override()
            && !effective_additional_permissions.permissions_preapproved
            && !matches!(
                turn.approval_policy.value(),
                codex_protocol::protocol::AskForApproval::OnRequest
            )
        {
            let approval_policy = turn.approval_policy.value();
            return Err(FunctionCallError::RespondToModel(format!(
                "approval policy is {approval_policy:?}; reject command — you should not ask for escalated permissions if the approval policy is {approval_policy:?}"
            )));
        }

        // Intercept apply_patch if present.
        if let Some(output) = intercept_apply_patch(
            &exec_params.command,
            &exec_params.cwd,
            fs.as_ref(),
            &environment_id,
            environment.is_remote(),
            session.clone(),
            turn.clone(),
            Some(&tracker),
            &call_id,
            tool_name.as_str(),
        )
        .await?
        {
            return Ok(output);
        }

        let source = ExecCommandSource::Agent;
        let emitter = ToolEmitter::shell(
            exec_params.command.clone(),
            exec_params.cwd.clone(),
            source,
            freeform,
        );
        let event_ctx = ToolEventCtx::new(
            session.as_ref(),
            turn.as_ref(),
            &call_id,
            /*turn_diff_tracker*/ None,
        );
        emitter.begin(event_ctx).await;

        let file_system_sandbox_policy = turn.file_system_sandbox_policy();
        let exec_approval_requirement = session
            .services
            .exec_policy
            .create_exec_approval_requirement_for_command(ExecApprovalRequest {
                command: &exec_params.command,
                approval_policy: turn.approval_policy.value(),
                permission_profile: turn.permission_profile(),
                file_system_sandbox_policy: &file_system_sandbox_policy,
                sandbox_cwd: exec_params.cwd.as_path(),
                sandbox_permissions: if effective_additional_permissions.permissions_preapproved {
                    codex_protocol::models::SandboxPermissions::UseDefault
                } else {
                    effective_additional_permissions.sandbox_permissions
                },
                prefix_rule,
            })
            .await;
        if environment.is_remote() {
            reject_remote_process_when_sandbox_required(
                turn.as_ref(),
                &environment_id,
                effective_additional_permissions.sandbox_permissions,
                &exec_approval_requirement,
                tool_name.as_str(),
            )
            .map_err(FunctionCallError::RespondToModel)?;
        }

        let req = ShellRequest {
            command: exec_params.command.clone(),
            hook_command,
            cwd: exec_params.cwd.clone(),
            timeout_ms: exec_params.expiration.timeout_ms(),
            environment_id,
            environment,
            env: exec_params.env.clone(),
            exec_server_env_config: Some(ExecServerEnvConfig::from_shell_environment_policy(
                &turn.shell_environment_policy,
                create_env(&turn.shell_environment_policy, /*thread_id*/ None),
            )),
            explicit_env_overrides,
            network: exec_params.network.clone(),
            sandbox_permissions: effective_additional_permissions.sandbox_permissions,
            additional_permissions: normalized_additional_permissions,
            #[cfg(unix)]
            additional_permissions_preapproved: effective_additional_permissions
                .permissions_preapproved,
            justification: exec_params.justification.clone(),
            exec_approval_requirement,
        };
        let mut orchestrator = ToolOrchestrator::new();
        let mut runtime = {
            use ShellRuntimeBackend::*;
            match shell_runtime_backend {
                Generic => ShellRuntime::new(),
                backend @ (ShellCommandClassic | ShellCommandZshFork) => {
                    ShellRuntime::for_shell_command(backend)
                }
            }
        };
        let tool_ctx = ToolCtx {
            session: session.clone(),
            turn: turn.clone(),
            call_id: call_id.clone(),
            tool_name,
        };
        let out = orchestrator
            .run(
                &mut runtime,
                &req,
                &tool_ctx,
                &turn,
                turn.approval_policy.value(),
            )
            .await
            .map(|result| result.output);
        let event_ctx = ToolEventCtx::new(
            session.as_ref(),
            turn.as_ref(),
            &call_id,
            /*turn_diff_tracker*/ None,
        );
        let post_tool_use_response = out
            .as_ref()
            .ok()
            .map(|output| crate::tools::format_exec_output_str(output, turn.truncation_policy))
            .map(JsonValue::String);
        let content = emitter.finish(event_ctx, out).await?;
        Ok(FunctionToolOutput {
            body: vec![
                codex_protocol::models::FunctionCallOutputContentItem::InputText { text: content },
            ],
            success: Some(true),
            post_tool_use_response,
        })
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
