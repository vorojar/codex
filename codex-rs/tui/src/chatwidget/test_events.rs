//! Test-only event fixtures for legacy-style ChatWidget coverage.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use crate::approval_events::ApplyPatchApprovalRequestEvent;
use crate::approval_events::ExecApprovalRequestEvent;
use crate::session_state::SessionNetworkProxyRuntime;
use crate::session_state::ThreadSessionState;
use crate::token_usage::TokenCountEvent;
use crate::tool_activity::ExecCommandBeginEvent;
use crate::tool_activity::ExecCommandEndEvent;
use crate::tool_activity::ExecCommandOutputDeltaEvent;
use crate::tool_activity::HookCompletedEvent;
use crate::tool_activity::HookStartedEvent;
use crate::tool_activity::ImageGenerationBeginEvent;
use crate::tool_activity::ImageGenerationEndEvent;
use crate::tool_activity::McpToolCallBeginEvent;
use crate::tool_activity::McpToolCallEndEvent;
use crate::tool_activity::PatchApplyBeginEvent;
use crate::tool_activity::PatchApplyEndEvent;
use crate::tool_activity::TerminalInteractionEvent;
use crate::tool_activity::ViewImageToolCallEvent;
use crate::tool_activity::WebSearchBeginEvent;
use crate::tool_activity::WebSearchEndEvent;
use crate::turn_state::TurnAbortReason;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::CodexErrorInfo;
use codex_app_server_protocol::McpAuthStatus;
use codex_app_server_protocol::ModelVerification;
use codex_protocol::ThreadId;
use codex_protocol::approvals::ElicitationRequestEvent;
use codex_protocol::approvals::GuardianAssessmentEvent;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::items::TurnItem;
use codex_protocol::mcp::Resource as McpResource;
use codex_protocol::mcp::ResourceTemplate as McpResourceTemplate;
use codex_protocol::mcp::Tool as McpTool;
use codex_protocol::memory_citation::MemoryCitation;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::plan_tool::UpdatePlanArgs;
use codex_protocol::request_permissions::RequestPermissionsEvent;
use codex_protocol::request_user_input::RequestUserInputEvent;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;

use super::user_messages::UserMessageEvent;

#[derive(Debug, Clone)]
pub(crate) struct Event {
    pub(crate) id: String,
    pub(crate) msg: EventMsg,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionConfiguredEvent {
    pub(crate) session_id: ThreadId,
    pub(crate) forked_from_id: Option<ThreadId>,
    pub(crate) thread_name: Option<String>,
    pub(crate) model: String,
    #[allow(dead_code)]
    pub(crate) model_provider_id: String,
    pub(crate) service_tier: Option<ServiceTier>,
    pub(crate) approval_policy: AskForApproval,
    pub(crate) approvals_reviewer: ApprovalsReviewer,
    pub(crate) permission_profile: PermissionProfile,
    pub(crate) cwd: AbsolutePathBuf,
    pub(crate) reasoning_effort: Option<ReasoningEffortConfig>,
    pub(crate) history_log_id: u64,
    pub(crate) history_entry_count: usize,
    pub(crate) initial_messages: Option<Vec<EventMsg>>,
    pub(crate) network_proxy: Option<SessionNetworkProxyRuntime>,
    pub(crate) rollout_path: Option<PathBuf>,
}

impl SessionConfiguredEvent {
    pub(crate) fn into_session(self) -> (ThreadSessionState, Option<Vec<EventMsg>>) {
        (
            ThreadSessionState {
                thread_id: self.session_id,
                forked_from_id: self.forked_from_id,
                fork_parent_title: None,
                thread_name: self.thread_name,
                model: self.model,
                model_provider_id: self.model_provider_id,
                service_tier: self.service_tier,
                approval_policy: self.approval_policy,
                approvals_reviewer: self.approvals_reviewer,
                permission_profile: self.permission_profile,
                cwd: self.cwd,
                instruction_source_paths: Vec::new(),
                reasoning_effort: self.reasoning_effort,
                history_log_id: self.history_log_id,
                history_entry_count: u64::try_from(self.history_entry_count).unwrap_or(u64::MAX),
                network_proxy: self.network_proxy,
                rollout_path: self.rollout_path,
            },
            self.initial_messages,
        )
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum EventMsg {
    Error(ErrorEvent),
    Warning(WarningEvent),
    GuardianWarning(WarningEvent),
    ModelVerification(ModelVerificationEvent),
    ThreadRolledBack(ThreadRolledBackEvent),
    TurnStarted(TurnStartedEvent),
    TurnComplete(TurnCompleteEvent),
    TokenCount(TokenCountEvent),
    AgentMessage(AgentMessageEvent),
    UserMessage(UserMessageEvent),
    AgentMessageDelta(AgentMessageDeltaEvent),
    AgentReasoning(AgentReasoningEvent),
    AgentReasoningDelta(AgentReasoningDeltaEvent),
    AgentReasoningRawContent(AgentReasoningRawContentEvent),
    AgentReasoningRawContentDelta(AgentReasoningRawContentDeltaEvent),
    SessionConfigured(SessionConfiguredEvent),
    ThreadNameUpdated(ThreadNameUpdatedEvent),
    McpToolCallBegin(McpToolCallBeginEvent),
    McpToolCallEnd(McpToolCallEndEvent),
    WebSearchBegin(WebSearchBeginEvent),
    WebSearchEnd(WebSearchEndEvent),
    ImageGenerationBegin(ImageGenerationBeginEvent),
    ImageGenerationEnd(ImageGenerationEndEvent),
    ExecCommandBegin(ExecCommandBeginEvent),
    ExecCommandOutputDelta(ExecCommandOutputDeltaEvent),
    TerminalInteraction(TerminalInteractionEvent),
    ExecCommandEnd(ExecCommandEndEvent),
    ViewImageToolCall(ViewImageToolCallEvent),
    ExecApprovalRequest(ExecApprovalRequestEvent),
    RequestPermissions(RequestPermissionsEvent),
    RequestUserInput(RequestUserInputEvent),
    ElicitationRequest(ElicitationRequestEvent),
    ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent),
    GuardianAssessment(GuardianAssessmentEvent),
    DeprecationNotice(DeprecationNoticeEvent),
    BackgroundEvent(BackgroundEventEvent),
    UndoStarted(UndoStartedEvent),
    UndoCompleted(UndoCompletedEvent),
    StreamError(StreamErrorEvent),
    PatchApplyBegin(PatchApplyBeginEvent),
    PatchApplyEnd(PatchApplyEndEvent),
    TurnDiff(TurnDiffEvent),
    McpListToolsResponse(McpListToolsResponseEvent),
    ListSkillsResponse(codex_app_server_protocol::SkillsListResponse),
    SkillsUpdateAvailable,
    PlanUpdate(UpdatePlanArgs),
    TurnAborted(TurnAbortedEvent),
    ShutdownComplete,
    EnteredReviewMode(String),
    ExitedReviewMode(ExitedReviewModeEvent),
    ItemCompleted(ItemCompletedEvent),
    HookStarted(HookStartedEvent),
    HookCompleted(HookCompletedEvent),
    PlanDelta(PlanDeltaEvent),
}

#[derive(Debug, Clone)]
pub(crate) struct ErrorEvent {
    pub(crate) message: String,
    pub(crate) codex_error_info: Option<CodexErrorInfo>,
}

#[derive(Debug, Clone)]
pub(crate) struct WarningEvent {
    pub(crate) message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelVerificationEvent {
    pub(crate) verifications: Vec<ModelVerification>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TurnCompleteEvent {
    pub(crate) turn_id: String,
    pub(crate) last_agent_message: Option<String>,
    pub(crate) completed_at: Option<i64>,
    pub(crate) duration_ms: Option<i64>,
    pub(crate) time_to_first_token_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct TurnStartedEvent {
    pub(crate) turn_id: String,
    pub(crate) started_at: Option<i64>,
    pub(crate) model_context_window: Option<i64>,
    pub(crate) collaboration_mode_kind: codex_protocol::config_types::ModeKind,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentMessageEvent {
    pub(crate) message: String,
    pub(crate) phase: Option<MessagePhase>,
    pub(crate) memory_citation: Option<MemoryCitation>,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentMessageDeltaEvent {
    pub(crate) delta: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentReasoningEvent {
    pub(crate) text: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentReasoningRawContentEvent {
    pub(crate) text: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentReasoningRawContentDeltaEvent {
    pub(crate) delta: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentReasoningDeltaEvent {
    pub(crate) delta: String,
}

#[derive(Debug, Clone)]
pub(crate) struct BackgroundEventEvent {
    pub(crate) message: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DeprecationNoticeEvent {
    pub(crate) summary: String,
    pub(crate) details: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct UndoStartedEvent {
    pub(crate) message: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct UndoCompletedEvent {
    pub(crate) success: bool,
    pub(crate) message: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ThreadRolledBackEvent {
    pub(crate) num_turns: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct StreamErrorEvent {
    pub(crate) message: String,
    pub(crate) codex_error_info: Option<CodexErrorInfo>,
    pub(crate) additional_details: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct TurnDiffEvent {
    pub(crate) unified_diff: String,
}

#[derive(Debug, Clone)]
pub(crate) struct McpListToolsResponseEvent {
    pub(crate) tools: HashMap<String, McpTool>,
    pub(crate) resources: HashMap<String, Vec<McpResource>>,
    pub(crate) resource_templates: HashMap<String, Vec<McpResourceTemplate>>,
    pub(crate) auth_statuses: HashMap<String, McpAuthStatus>,
}

#[derive(Debug, Clone)]
pub(crate) struct ThreadNameUpdatedEvent {
    pub(crate) thread_id: ThreadId,
    pub(crate) thread_name: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct TurnAbortedEvent {
    pub(crate) turn_id: Option<String>,
    pub(crate) reason: TurnAbortReason,
    pub(crate) completed_at: Option<i64>,
    pub(crate) duration_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct ItemCompletedEvent {
    pub(crate) thread_id: ThreadId,
    pub(crate) turn_id: String,
    pub(crate) item: TurnItem,
}

#[derive(Debug, Clone)]
pub(crate) struct PlanDeltaEvent {
    pub(crate) thread_id: String,
    pub(crate) turn_id: String,
    pub(crate) item_id: String,
    pub(crate) delta: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ExitedReviewModeEvent;
