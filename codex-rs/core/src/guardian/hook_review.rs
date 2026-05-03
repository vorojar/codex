use std::sync::Arc;

use anyhow::Context;
use anyhow::anyhow;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::HookEventName;
use codex_protocol::protocol::HookSource;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::codex_delegate::run_codex_thread_one_shot;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

use super::GUARDIAN_PREFERRED_MODEL;
use super::GUARDIAN_REVIEW_TIMEOUT;
use super::GUARDIAN_REVIEWER_NAME;
use super::review_session::build_guardian_review_session_config;

const HOOK_REVIEWER_NAME: &str = "hook_auto_review";

const HOOK_REVIEW_POLICY: &str = r#"You are the Codex hook auto-reviewer.

Your job is to review a newly discovered or modified user hook before Codex runs
it. Decide whether this exact hook source appears to contain clear malicious or
dangerous behavior.

Mark a hook dangerous only for clear security risk, such as credential theft,
secret exfiltration, remote code execution/persistence, tampering with Codex or
shell startup files, destructive file operations, or attempts to conceal those
actions. Benign automation, formatting, notifications, logging, and local
developer workflow helpers should not be marked dangerous.

Review the hook source file and the configured command. If the command invokes a
script or nearby source file, inspect that file too using read-only tools. Do not
execute the hook command or any referenced script.

Return only JSON that matches the requested schema. Use `dangerous` for clear
malicious or security-vulnerable behavior, `safe` for benign behavior, and
`unsure` when the evidence is insufficient."#;

#[derive(Debug, Clone)]
pub(crate) struct HookSecurityReviewRequest {
    pub(crate) key: String,
    pub(crate) event_name: HookEventName,
    pub(crate) matcher: Option<String>,
    pub(crate) command: Option<String>,
    pub(crate) timeout_sec: u64,
    pub(crate) source_path: AbsolutePathBuf,
    pub(crate) source: HookSource,
    pub(crate) current_hash: String,
    pub(crate) source_excerpt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookSecurityVerdict {
    Safe,
    Dangerous,
    Unsure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HookSecurityReview {
    pub(crate) verdict: HookSecurityVerdict,
    pub(crate) reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
struct HookSecurityReviewResponse {
    verdict: HookSecurityReviewResponseVerdict,
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum HookSecurityReviewResponseVerdict {
    Safe,
    Dangerous,
    Unsure,
}

pub(crate) async fn review_hook_for_security(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    request: HookSecurityReviewRequest,
) -> anyhow::Result<HookSecurityReview> {
    let live_network_config = match session.services.network_proxy.as_ref() {
        Some(network_proxy) => Some(
            network_proxy
                .proxy()
                .current_cfg()
                .await
                .context("failed to read live network proxy config for hook review")?,
        ),
        None => None,
    };
    let available_models = session
        .services
        .models_manager
        .list_models(RefreshStrategy::Offline)
        .await;
    let preferred_model = available_models
        .iter()
        .find(|preset| preset.model == GUARDIAN_PREFERRED_MODEL);
    let (review_model, review_reasoning_effort) = if let Some(preset) = preferred_model {
        let effort = preferred_reasoning_effort(
            preset
                .supported_reasoning_efforts
                .iter()
                .any(|effort| effort.effort == codex_protocol::openai_models::ReasoningEffort::Low),
            Some(preset.default_reasoning_effort),
        );
        (GUARDIAN_PREFERRED_MODEL.to_string(), effort)
    } else {
        let effort = preferred_reasoning_effort(
            turn.model_info
                .supported_reasoning_levels
                .iter()
                .any(|preset| preset.effort == codex_protocol::openai_models::ReasoningEffort::Low),
            turn.reasoning_effort
                .or(turn.model_info.default_reasoning_level),
        );
        (turn.model_info.slug.clone(), effort)
    };

    let mut review_config = build_guardian_review_session_config(
        turn.config.as_ref(),
        live_network_config,
        review_model.as_str(),
        review_reasoning_effort,
    )
    .context("failed to build hook review session config")?;
    review_config.base_instructions = Some(HOOK_REVIEW_POLICY.to_string());

    let cancel_token = CancellationToken::new();
    let codex = run_codex_thread_one_shot(
        review_config,
        Arc::clone(&session.services.auth_manager),
        Arc::clone(&session.services.models_manager),
        vec![UserInput::Text {
            text: hook_review_prompt(&request),
            text_elements: Vec::new(),
        }],
        session,
        turn,
        cancel_token.clone(),
        SubAgentSource::Other(GUARDIAN_REVIEWER_NAME.to_string()),
        Some(hook_review_schema()),
        Some(InitialHistory::New),
    )
    .await
    .context("failed to start hook review session")?;

    let raw_response = match tokio::time::timeout(GUARDIAN_REVIEW_TIMEOUT, async {
        await_hook_review_response(codex).await
    })
    .await
    {
        Ok(response) => response?,
        Err(_) => {
            cancel_token.cancel();
            return Err(anyhow!("hook review timed out"));
        }
    };
    parse_hook_review_response(&raw_response)
}

fn hook_review_prompt(request: &HookSecurityReviewRequest) -> String {
    format!(
        r#"Review this Codex hook.

Hook identity:
- key: {key}
- event: {event_name:?}
- matcher: {matcher}
- source: {source:?}
- source_path: {source_path}
- timeout_sec: {timeout_sec}
- current_hash: {current_hash}

Configured command:
```sh
{command}
```

Source file excerpt:
```
{source_excerpt}
```

Inspect referenced source files when needed. Do not execute the hook command.
"#,
        key = request.key,
        event_name = request.event_name,
        matcher = request.matcher.as_deref().unwrap_or("<none>"),
        source = request.source,
        source_path = request.source_path.display(),
        timeout_sec = request.timeout_sec,
        current_hash = request.current_hash,
        command = request.command.as_deref().unwrap_or("<none>"),
        source_excerpt = request.source_excerpt,
    )
}

fn hook_review_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict", "reason"],
        "properties": {
            "verdict": {
                "type": "string",
                "enum": ["safe", "dangerous", "unsure"]
            },
            "reason": {
                "type": "string"
            }
        }
    })
}

fn preferred_reasoning_effort(
    supports_low: bool,
    fallback: Option<codex_protocol::openai_models::ReasoningEffort>,
) -> Option<codex_protocol::openai_models::ReasoningEffort> {
    if supports_low {
        Some(codex_protocol::openai_models::ReasoningEffort::Low)
    } else {
        fallback
    }
}

async fn await_hook_review_response(codex: crate::session::Codex) -> anyhow::Result<String> {
    let mut last_error_message: Option<String> = None;
    loop {
        let event = codex
            .next_event()
            .await
            .map_err(|err| anyhow!("hook review session failed: {err}"))?;
        match event.msg {
            EventMsg::TurnComplete(turn_complete) => {
                if let Some(last_agent_message) = turn_complete.last_agent_message {
                    return Ok(last_agent_message);
                }
                if let Some(last_error_message) = last_error_message {
                    return Err(anyhow!(last_error_message));
                }
                return Err(anyhow!("hook review completed without a response"));
            }
            EventMsg::Error(error) => {
                last_error_message = Some(error.message);
            }
            EventMsg::TurnAborted(_) => {
                return Err(anyhow!("hook review was aborted"));
            }
            _ => {}
        }
    }
}

fn parse_hook_review_response(raw_response: &str) -> anyhow::Result<HookSecurityReview> {
    let response: HookSecurityReviewResponse = serde_json::from_str(raw_response)
        .with_context(|| format!("failed to parse hook review response: {raw_response}"))?;
    let verdict = match response.verdict {
        HookSecurityReviewResponseVerdict::Safe => HookSecurityVerdict::Safe,
        HookSecurityReviewResponseVerdict::Dangerous => HookSecurityVerdict::Dangerous,
        HookSecurityReviewResponseVerdict::Unsure => HookSecurityVerdict::Unsure,
    };
    Ok(HookSecurityReview {
        verdict,
        reason: response.reason,
    })
}

pub(crate) fn hook_reviewer_name() -> &'static str {
    HOOK_REVIEWER_NAME
}
