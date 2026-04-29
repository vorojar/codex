use crate::context::CollaborationModeInstructions;
use crate::context::ContextualUserFragment;
use crate::context::EnvironmentContext;
use crate::context::ModelSwitchInstructions;
use crate::context::PermissionsInstructions;
use crate::context::PersonalitySpecInstructions;
use crate::context::RealtimeEndInstructions;
use crate::context::RealtimeStartInstructions;
use crate::context::RealtimeStartWithInstructions;
use crate::session::PreviousTurnSettings;
use crate::session::turn_context::TurnContext;
use crate::shell::Shell;
use codex_execpolicy::Policy;
use codex_features::Feature;
use codex_journal::Journal;
use codex_journal::JournalEntry;
use codex_journal::KeyFilter;
use codex_journal::MetadataEntryBuilder;
use codex_journal::PromptMessage;
use codex_journal::PromptRenderer;
use codex_protocol::config_types::Personality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::TurnContextItem;

const PROMPT_BUNDLE_KEY_PREFIX: &str = "prompt";
pub(crate) const DEVELOPER_BUNDLE: &str = "developer";
pub(crate) const USAGE_HINT_BUNDLE: &str = "usage_hint";
pub(crate) const CONTEXTUAL_USER_BUNDLE: &str = "contextual_user";
pub(crate) const GUARDIAN_BUNDLE: &str = "guardian";

pub(crate) fn context_prompt_renderer() -> PromptRenderer {
    PromptRenderer::new()
        .group(KeyFilter::prefix([
            PROMPT_BUNDLE_KEY_PREFIX,
            DEVELOPER_BUNDLE,
        ]))
        .group(KeyFilter::prefix([
            PROMPT_BUNDLE_KEY_PREFIX,
            USAGE_HINT_BUNDLE,
        ]))
        .group(KeyFilter::prefix([
            PROMPT_BUNDLE_KEY_PREFIX,
            CONTEXTUAL_USER_BUNDLE,
        ]))
}

fn build_environment_update_item(
    previous: Option<&TurnContextItem>,
    next: &TurnContext,
    shell: &Shell,
) -> Option<String> {
    if !next.config.include_environment_context {
        return None;
    }

    let prev = previous?;
    let prev_context = EnvironmentContext::from_turn_context_item(prev, shell.name().to_string());
    let next_context = EnvironmentContext::from_turn_context(next, shell);
    if prev_context.equals_except_shell(&next_context) {
        return None;
    }

    Some(EnvironmentContext::diff_from_turn_context_item(prev, &next_context).render())
}

fn build_permissions_update_item(
    previous: Option<&TurnContextItem>,
    next: &TurnContext,
    exec_policy: &Policy,
) -> Option<String> {
    if !next.config.include_permissions_instructions {
        return None;
    }

    let prev = previous?;
    if prev.permission_profile() == next.permission_profile()
        && prev.approval_policy == next.approval_policy.value()
    {
        return None;
    }

    Some(
        PermissionsInstructions::from_permission_profile(
            &next.permission_profile,
            next.approval_policy.value(),
            next.config.approvals_reviewer,
            exec_policy,
            &next.cwd,
            next.features.enabled(Feature::ExecPermissionApprovals),
            next.features.enabled(Feature::RequestPermissionsTool),
        )
        .render(),
    )
}

fn build_collaboration_mode_update_item(
    previous: Option<&TurnContextItem>,
    next: &TurnContext,
) -> Option<String> {
    let prev = previous?;
    if prev.collaboration_mode.as_ref() != Some(&next.collaboration_mode) {
        // If the next mode has empty developer instructions, this returns None and we emit no
        // update, so prior collaboration instructions remain in the prompt history.
        Some(
            CollaborationModeInstructions::from_collaboration_mode(&next.collaboration_mode)?
                .render(),
        )
    } else {
        None
    }
}

pub(crate) fn build_realtime_update_item(
    previous: Option<&TurnContextItem>,
    previous_turn_settings: Option<&PreviousTurnSettings>,
    next: &TurnContext,
) -> Option<String> {
    match (
        previous.and_then(|item| item.realtime_active),
        next.realtime_active,
    ) {
        (Some(true), false) => Some(RealtimeEndInstructions::new("inactive").render()),
        (Some(false), true) | (None, true) => Some(
            if let Some(instructions) = next
                .config
                .experimental_realtime_start_instructions
                .as_deref()
            {
                RealtimeStartWithInstructions::new(instructions).render()
            } else {
                RealtimeStartInstructions.render()
            },
        ),
        (Some(true), true) | (Some(false), false) => None,
        (None, false) => previous_turn_settings
            .and_then(|settings| settings.realtime_active)
            .filter(|realtime_active| *realtime_active)
            .map(|_| RealtimeEndInstructions::new("inactive").render()),
    }
}

pub(crate) fn build_initial_realtime_item(
    previous: Option<&TurnContextItem>,
    previous_turn_settings: Option<&PreviousTurnSettings>,
    next: &TurnContext,
) -> Option<String> {
    build_realtime_update_item(previous, previous_turn_settings, next)
}

fn build_personality_update_item(
    previous: Option<&TurnContextItem>,
    next: &TurnContext,
    personality_feature_enabled: bool,
) -> Option<String> {
    if !personality_feature_enabled {
        return None;
    }
    let previous = previous?;
    if next.model_info.slug != previous.model {
        return None;
    }

    if let Some(personality) = next.personality
        && next.personality != previous.personality
    {
        let model_info = &next.model_info;
        let personality_message = personality_message_for(model_info, personality);
        personality_message.map(|message| PersonalitySpecInstructions::new(message).render())
    } else {
        None
    }
}

pub(crate) fn personality_message_for(
    model_info: &ModelInfo,
    personality: Personality,
) -> Option<String> {
    model_info
        .model_messages
        .as_ref()
        .and_then(|spec| spec.get_personality_message(Some(personality)))
        .filter(|message| !message.is_empty())
}

pub(crate) fn build_model_instructions_update_item(
    previous_turn_settings: Option<&PreviousTurnSettings>,
    next: &TurnContext,
) -> Option<String> {
    let previous_turn_settings = previous_turn_settings?;
    if previous_turn_settings.model == next.model_info.slug {
        return None;
    }

    let model_instructions = next.model_info.get_model_instructions(next.personality);
    if model_instructions.is_empty() {
        return None;
    }

    Some(ModelSwitchInstructions::new(model_instructions).render())
}

pub(crate) fn context_entry(
    bundle: &str,
    name: &str,
    prompt_order: i64,
    message: PromptMessage,
) -> Option<JournalEntry> {
    context_entry_builder(bundle, name, message)
        .prompt_order(prompt_order)
        .build()
}

fn context_entry_builder(bundle: &str, name: &str, message: PromptMessage) -> MetadataEntryBuilder {
    Journal::metadata_entry_builder([PROMPT_BUNDLE_KEY_PREFIX, bundle, name], message)
}

pub(crate) fn build_settings_update_entries(
    previous: Option<&TurnContextItem>,
    previous_turn_settings: Option<&PreviousTurnSettings>,
    next: &TurnContext,
    shell: &Shell,
    exec_policy: &Policy,
    personality_feature_enabled: bool,
) -> Vec<JournalEntry> {
    // TODO(ccunningham): build_settings_update_items still does not cover every
    // model-visible item emitted by build_initial_context. Persist the remaining
    // inputs or add explicit replay events so fork/resume can diff everything
    // deterministically.
    let mut entries = Vec::with_capacity(6);

    if let Some(item) =
        build_model_instructions_update_item(previous_turn_settings, next).and_then(|text| {
            // Keep model-switch instructions first so model-specific guidance is read before
            // any other context diffs on this turn.
            context_entry(
                DEVELOPER_BUNDLE,
                "model_switch",
                10,
                PromptMessage::developer_text(text),
            )
        })
    {
        entries.push(item);
    }
    if let Some(item) =
        build_permissions_update_item(previous, next, exec_policy).and_then(|text| {
            context_entry(
                DEVELOPER_BUNDLE,
                "permissions",
                20,
                PromptMessage::developer_text(text),
            )
        })
    {
        entries.push(item);
    }
    if let Some(item) = build_collaboration_mode_update_item(previous, next).and_then(|text| {
        context_entry(
            DEVELOPER_BUNDLE,
            "collaboration_mode",
            30,
            PromptMessage::developer_text(text),
        )
    }) {
        entries.push(item);
    }
    if let Some(item) =
        build_realtime_update_item(previous, previous_turn_settings, next).and_then(|text| {
            context_entry(
                DEVELOPER_BUNDLE,
                "realtime",
                40,
                PromptMessage::developer_text(text),
            )
        })
    {
        entries.push(item);
    }
    if let Some(item) = build_personality_update_item(previous, next, personality_feature_enabled)
        .and_then(|text| {
            context_entry(
                DEVELOPER_BUNDLE,
                "personality",
                50,
                PromptMessage::developer_text(text),
            )
        })
    {
        entries.push(item);
    }
    if let Some(item) = build_environment_update_item(previous, next, shell).and_then(|text| {
        context_entry(
            CONTEXTUAL_USER_BUNDLE,
            "environment",
            60,
            PromptMessage::user_text(text),
        )
    }) {
        entries.push(item);
    }

    entries
}
