use codex_protocol::config_types::SERVICE_TIER_PRIORITY;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::model_service_tier;
use codex_protocol::openai_models::model_supports_service_tier;

use super::ChatWidget;
use crate::bottom_pane::slash_commands::ServiceTierCommand;
use crate::bottom_pane::slash_commands::service_tier_commands_for_model;

pub(super) fn model_preset(chat: &ChatWidget, model: &str) -> Option<ModelPreset> {
    chat.model_catalog
        .try_list_models()
        .ok()
        .and_then(|models| models.into_iter().find(|preset| preset.model == model))
}

pub(crate) fn available_service_tier_commands(chat: &ChatWidget) -> Vec<ServiceTierCommand> {
    model_preset(chat, chat.current_model())
        .map(|model| service_tier_commands_for_model(&model))
        .unwrap_or_default()
}

pub(crate) fn service_tier_display_name(
    chat: &ChatWidget,
    model: &str,
    service_tier: &ServiceTier,
) -> String {
    model_preset(chat, model)
        .and_then(|preset| model_service_tier(&preset, service_tier).map(|tier| tier.name.clone()))
        .unwrap_or_else(|| service_tier.to_string())
}

pub(crate) fn current_service_tier_name(chat: &ChatWidget) -> Option<String> {
    chat.current_service_tier()
        .map(|tier| service_tier_display_name(chat, chat.current_model(), &tier))
}

pub(crate) fn current_service_tier_status_label(chat: &ChatWidget) -> String {
    current_service_tier_name(chat)
        .map(|name| format!("Tier {name}"))
        .unwrap_or_else(|| "Tier default".to_string())
}

pub(crate) fn model_supports_fast_mode(chat: &ChatWidget, model: &str) -> bool {
    model_preset(chat, model)
        .map(|preset| model_supports_service_tier(&preset, &SERVICE_TIER_PRIORITY.into()))
        .unwrap_or(false)
}
