use std::sync::Arc;

use anyhow::Context;
use codex_config::CONFIG_TOML_FILE;
use codex_hooks::HookListEntry;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::HookAutoReviewCompletedEvent;
use codex_protocol::protocol::HookAutoReviewDangerousHook;
use codex_protocol::protocol::HookAutoReviewStartedEvent;
use codex_protocol::protocol::HookTrustStatus;
use codex_protocol::protocol::WarningEvent;
use tokio::fs;
use toml_edit::value;
use tracing::warn;

use crate::config::edit::ConfigEdit;
use crate::config::edit::ConfigEditsBuilder;
use crate::guardian;
use crate::guardian::HookSecurityReviewRequest;
use crate::guardian::HookSecurityVerdict;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

const MAX_SOURCE_EXCERPT_CHARS: usize = 32_000;

#[derive(Debug, Default)]
struct HookAutoReviewStats {
    reviewed_count: u32,
    trusted_count: u32,
    dangerous_count: u32,
    skipped_count: u32,
    failed_count: u32,
    dangerous_hooks: Vec<HookAutoReviewDangerousHook>,
}

pub(crate) async fn maybe_run_hook_auto_review(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
) {
    if turn_context.config.approvals_reviewer != ApprovalsReviewer::AutoReview {
        return;
    }

    let hooks = sess.hooks();
    let candidates = hooks
        .hook_entries()
        .iter()
        .filter(|hook| {
            matches!(
                hook.trust_status,
                HookTrustStatus::Untrusted | HookTrustStatus::Modified
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return;
    }

    sess.send_event(
        turn_context,
        EventMsg::HookAutoReviewStarted(HookAutoReviewStartedEvent {
            turn_id: turn_context.sub_id.clone(),
            hook_count: u32_count(candidates.len()),
        }),
    )
    .await;

    let mut stats = HookAutoReviewStats::default();
    let mut edits = Vec::new();
    for hook in candidates {
        let review_request = HookSecurityReviewRequest {
            key: hook.key.clone(),
            event_name: hook.event_name,
            matcher: hook.matcher.clone(),
            command: hook.command.clone(),
            timeout_sec: hook.timeout_sec,
            source_path: hook.source_path.clone(),
            source: hook.source,
            current_hash: hook.current_hash.clone(),
            source_excerpt: read_source_excerpt(&hook).await,
        };
        let review = guardian::review_hook_for_security(
            Arc::clone(sess),
            Arc::clone(turn_context),
            review_request,
        )
        .await;
        let review: guardian::HookSecurityReview = match review {
            Ok(review) => review,
            Err(err) => {
                warn!("hook auto-review failed for {}: {err:#}", hook.key);
                stats.failed_count = stats.failed_count.saturating_add(1);
                continue;
            }
        };

        stats.reviewed_count = stats.reviewed_count.saturating_add(1);
        match review.verdict {
            HookSecurityVerdict::Safe => {
                stats.trusted_count = stats.trusted_count.saturating_add(1);
                edits.extend(trust_hook_edits(&hook));
            }
            HookSecurityVerdict::Dangerous => {
                stats.dangerous_count = stats.dangerous_count.saturating_add(1);
                let reason = normalized_reason(review.reason);
                edits.extend(dangerous_hook_edits(&hook, &reason));
                stats.dangerous_hooks.push(HookAutoReviewDangerousHook {
                    key: hook.key.clone(),
                    source_path: hook.source_path.clone(),
                    reason,
                });
            }
            HookSecurityVerdict::Unsure => {
                stats.skipped_count = stats.skipped_count.saturating_add(1);
            }
        }
    }

    if !edits.is_empty() {
        let apply_result = ConfigEditsBuilder::new(turn_context.config.codex_home.as_path())
            .with_profile(turn_context.config.active_profile.as_deref())
            .with_edits(edits)
            .apply()
            .await
            .with_context(|| {
                format!(
                    "failed to persist hook auto-review verdicts to {}",
                    turn_context
                        .config
                        .codex_home
                        .join(CONFIG_TOML_FILE)
                        .display()
                )
            });
        if let Err(err) = apply_result {
            let persisted_count = stats.trusted_count.saturating_add(stats.dangerous_count);
            stats.failed_count = stats.failed_count.saturating_add(persisted_count);
            stats.trusted_count = 0;
            stats.dangerous_count = 0;
            stats.dangerous_hooks.clear();
            sess.send_event(
                turn_context,
                EventMsg::Warning(WarningEvent {
                    message: format!("{err:#}"),
                }),
            )
            .await;
        } else {
            sess.reload_user_config_layer().await;
        }
    }

    sess.send_event(
        turn_context,
        EventMsg::HookAutoReviewCompleted(HookAutoReviewCompletedEvent {
            turn_id: turn_context.sub_id.clone(),
            reviewed_count: stats.reviewed_count,
            trusted_count: stats.trusted_count,
            dangerous_count: stats.dangerous_count,
            skipped_count: stats.skipped_count,
            failed_count: stats.failed_count,
            dangerous_hooks: stats.dangerous_hooks,
        }),
    )
    .await;
}

async fn read_source_excerpt(hook: &HookListEntry) -> String {
    match fs::read_to_string(hook.source_path.as_path()).await {
        Ok(contents) => truncate_source_excerpt(&contents),
        Err(err) => format!("Unable to read hook source file: {err}"),
    }
}

fn truncate_source_excerpt(contents: &str) -> String {
    let mut chars = contents.chars();
    let excerpt = chars
        .by_ref()
        .take(MAX_SOURCE_EXCERPT_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{excerpt}\n\n[truncated after {MAX_SOURCE_EXCERPT_CHARS} characters]")
    } else {
        excerpt
    }
}

fn trust_hook_edits(hook: &HookListEntry) -> Vec<ConfigEdit> {
    vec![
        set_hook_state(&hook.key, "trusted_hash", value(hook.current_hash.clone())),
        set_hook_state(
            &hook.key,
            "reviewed_by",
            value(guardian::hook_reviewer_name()),
        ),
        clear_hook_state(&hook.key, "dangerous_hash"),
        clear_hook_state(&hook.key, "dangerous_reason"),
    ]
}

fn dangerous_hook_edits(hook: &HookListEntry, reason: &str) -> Vec<ConfigEdit> {
    vec![
        set_hook_state(&hook.key, "enabled", value(false)),
        set_hook_state(
            &hook.key,
            "dangerous_hash",
            value(hook.current_hash.clone()),
        ),
        set_hook_state(&hook.key, "dangerous_reason", value(reason)),
        set_hook_state(
            &hook.key,
            "reviewed_by",
            value(guardian::hook_reviewer_name()),
        ),
        clear_hook_state(&hook.key, "trusted_hash"),
    ]
}

fn set_hook_state(key: &str, field: &str, value: toml_edit::Item) -> ConfigEdit {
    ConfigEdit::SetPath {
        segments: hook_state_segments(key, field),
        value,
    }
}

fn clear_hook_state(key: &str, field: &str) -> ConfigEdit {
    ConfigEdit::ClearPath {
        segments: hook_state_segments(key, field),
    }
}

fn hook_state_segments(key: &str, field: &str) -> Vec<String> {
    vec![
        "hooks".to_string(),
        "state".to_string(),
        key.to_string(),
        field.to_string(),
    ]
}

fn normalized_reason(reason: String) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        "Auto-review marked this hook dangerous.".to_string()
    } else {
        reason.to_string()
    }
}

fn u32_count(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn truncate_source_excerpt_marks_long_content() {
        let contents = "a".repeat(MAX_SOURCE_EXCERPT_CHARS + 1);

        let excerpt = truncate_source_excerpt(&contents);

        assert_eq!(
            excerpt,
            format!(
                "{}\n\n[truncated after {MAX_SOURCE_EXCERPT_CHARS} characters]",
                "a".repeat(MAX_SOURCE_EXCERPT_CHARS)
            )
        );
    }
}
