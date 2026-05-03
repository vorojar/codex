//! Shared helpers for filtering and matching slash commands.
//!
//! The same sandbox- and feature-gating rules are used by both the composer
//! and the command popup. Centralizing them here keeps those call sites small
//! and ensures they stay in sync.
use std::collections::HashSet;
use std::str::FromStr;

use codex_protocol::config_types::ServiceTier;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelServiceTier;
use codex_utils_fuzzy_match::fuzzy_match;

use crate::slash_command::SlashCommand;
use crate::slash_command::built_in_slash_commands;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BuiltinCommandFlags {
    pub(crate) collaboration_modes_enabled: bool,
    pub(crate) connectors_enabled: bool,
    pub(crate) plugins_command_enabled: bool,
    pub(crate) fast_command_enabled: bool,
    pub(crate) goal_command_enabled: bool,
    pub(crate) personality_command_enabled: bool,
    pub(crate) realtime_conversation_enabled: bool,
    pub(crate) audio_device_selection_enabled: bool,
    pub(crate) allow_elevate_sandbox: bool,
    pub(crate) side_conversation_active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServiceTierCommand {
    pub(crate) service_tier: ServiceTier,
    pub(crate) command: String,
    pub(crate) name: String,
    pub(crate) description: String,
}

fn service_tier_command_from_model_tier(
    service_tier: &ModelServiceTier,
    command: String,
) -> ServiceTierCommand {
    ServiceTierCommand {
        service_tier: service_tier.id.clone(),
        command,
        name: service_tier.name.clone(),
        description: service_tier.description.clone(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SlashCommandAction {
    Builtin(SlashCommand),
    ServiceTier(ServiceTierCommand),
}

impl From<SlashCommand> for SlashCommandAction {
    fn from(command: SlashCommand) -> Self {
        Self::Builtin(command)
    }
}

pub(crate) fn command_name(action: &SlashCommandAction) -> &str {
    match action {
        SlashCommandAction::Builtin(command) => command.command(),
        SlashCommandAction::ServiceTier(command) => command.command.as_str(),
    }
}

pub(crate) fn command_description(action: &SlashCommandAction) -> &str {
    match action {
        SlashCommandAction::Builtin(command) => command.description(),
        SlashCommandAction::ServiceTier(command) => command.description.as_str(),
    }
}

pub(crate) fn command_supports_inline_args(action: &SlashCommandAction) -> bool {
    match action {
        SlashCommandAction::Builtin(command) => command.supports_inline_args(),
        SlashCommandAction::ServiceTier(_) => true,
    }
}

pub(crate) fn command_available_in_side_conversation(action: &SlashCommandAction) -> bool {
    match action {
        SlashCommandAction::Builtin(command) => command.available_in_side_conversation(),
        SlashCommandAction::ServiceTier(_) => false,
    }
}

pub(crate) fn command_available_during_task(action: &SlashCommandAction) -> bool {
    match action {
        SlashCommandAction::Builtin(command) => command.available_during_task(),
        SlashCommandAction::ServiceTier(_) => false,
    }
}

pub(crate) fn command_as_builtin(action: &SlashCommandAction) -> Option<SlashCommand> {
    match action {
        SlashCommandAction::Builtin(command) => Some(*command),
        SlashCommandAction::ServiceTier(_) => None,
    }
}

/// Return the built-ins that should be visible/usable for the current input.
pub(crate) fn builtins_for_input(flags: BuiltinCommandFlags) -> Vec<(&'static str, SlashCommand)> {
    built_in_slash_commands()
        .into_iter()
        .filter(|(_, cmd)| flags.allow_elevate_sandbox || *cmd != SlashCommand::ElevateSandbox)
        .filter(|(_, cmd)| {
            flags.collaboration_modes_enabled
                || !matches!(*cmd, SlashCommand::Collab | SlashCommand::Plan)
        })
        .filter(|(_, cmd)| flags.connectors_enabled || *cmd != SlashCommand::Apps)
        .filter(|(_, cmd)| flags.plugins_command_enabled || *cmd != SlashCommand::Plugins)
        .filter(|(_, cmd)| flags.goal_command_enabled || *cmd != SlashCommand::Goal)
        .filter(|(_, cmd)| flags.personality_command_enabled || *cmd != SlashCommand::Personality)
        .filter(|(_, cmd)| flags.realtime_conversation_enabled || *cmd != SlashCommand::Realtime)
        .filter(|(_, cmd)| flags.audio_device_selection_enabled || *cmd != SlashCommand::Settings)
        .filter(|(_, cmd)| !flags.side_conversation_active || cmd.available_in_side_conversation())
        .collect()
}

fn visible_service_tier_commands(
    flags: BuiltinCommandFlags,
    service_tier_commands: &[ServiceTierCommand],
) -> Vec<ServiceTierCommand> {
    if !flags.fast_command_enabled || flags.side_conversation_active {
        return Vec::new();
    }
    service_tier_commands.to_vec()
}

pub(crate) fn commands_for_input(
    flags: BuiltinCommandFlags,
    service_tier_commands: &[ServiceTierCommand],
) -> Vec<SlashCommandAction> {
    let visible_service_tier_commands = visible_service_tier_commands(flags, service_tier_commands);
    let mut commands = Vec::new();
    for (_, command) in builtins_for_input(flags) {
        commands.push(SlashCommandAction::Builtin(command));
        if command == SlashCommand::Model {
            commands.extend(
                visible_service_tier_commands
                    .iter()
                    .cloned()
                    .map(SlashCommandAction::ServiceTier),
            );
        }
    }
    commands
}

/// Find a single slash command by exact name, after applying feature gating.
///
/// Side-conversation gating is intentionally enforced by dispatch rather than exact lookup so a
/// typed command can produce a side-specific unavailable message while the popup still hides it.
pub(crate) fn find_command(
    name: &str,
    flags: BuiltinCommandFlags,
    service_tier_commands: &[ServiceTierCommand],
) -> Option<SlashCommandAction> {
    let lookup_flags = BuiltinCommandFlags {
        side_conversation_active: false,
        ..flags
    };
    if let Some(command) = SlashCommand::from_str(name).ok().filter(|command| {
        builtins_for_input(lookup_flags)
            .into_iter()
            .any(|(_, visible_command)| visible_command == *command)
    }) {
        return Some(SlashCommandAction::Builtin(command));
    }
    visible_service_tier_commands(lookup_flags, service_tier_commands)
        .into_iter()
        .find(|command| command.command == name)
        .map(SlashCommandAction::ServiceTier)
}

/// Whether any visible slash command fuzzily matches the provided prefix.
pub(crate) fn has_command_prefix(
    name: &str,
    flags: BuiltinCommandFlags,
    service_tier_commands: &[ServiceTierCommand],
) -> bool {
    commands_for_input(flags, service_tier_commands)
        .into_iter()
        .any(|command| fuzzy_match(command_name(&command), name).is_some())
}

pub(crate) fn service_tier_commands_for_model(model: &ModelPreset) -> Vec<ServiceTierCommand> {
    let mut reserved_names = HashSet::new();
    let mut commands = Vec::new();
    for service_tier in &model.service_tiers {
        let Some(command) = command_token_for_service_tier(service_tier, &reserved_names) else {
            continue;
        };
        reserved_names.insert(command.clone());
        commands.push(service_tier_command_from_model_tier(service_tier, command));
    }
    commands
}

fn command_token_for_service_tier(
    service_tier: &ModelServiceTier,
    reserved_names: &HashSet<String>,
) -> Option<String> {
    [
        normalize_service_tier_command_token(service_tier.name.as_str()),
        Some(service_tier.id.to_string()),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| !command_token_conflicts(candidate, reserved_names))
}

fn command_token_conflicts(candidate: &str, reserved_names: &HashSet<String>) -> bool {
    candidate.is_empty()
        || reserved_names.contains(candidate)
        || SlashCommand::from_str(candidate).is_ok()
}

fn normalize_service_tier_command_token(name: &str) -> Option<String> {
    let mut normalized = String::new();
    let mut pending_dash = false;

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !normalized.is_empty() {
                normalized.push('-');
            }
            normalized.push(ch.to_ascii_lowercase());
            pending_dash = false;
        } else if !normalized.is_empty() {
            pending_dash = true;
        }
    }

    (!normalized.is_empty()).then_some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::config_types::SERVICE_TIER_PRIORITY;
    use codex_protocol::openai_models::ModelServiceTier;
    use pretty_assertions::assert_eq;

    fn all_enabled_flags() -> BuiltinCommandFlags {
        BuiltinCommandFlags {
            collaboration_modes_enabled: true,
            connectors_enabled: true,
            plugins_command_enabled: true,
            fast_command_enabled: true,
            goal_command_enabled: true,
            personality_command_enabled: true,
            realtime_conversation_enabled: true,
            audio_device_selection_enabled: true,
            allow_elevate_sandbox: true,
            side_conversation_active: false,
        }
    }

    #[test]
    fn debug_command_still_resolves_for_dispatch() {
        let cmd = find_command("debug-config", all_enabled_flags(), &[]);
        assert_eq!(
            cmd,
            Some(SlashCommandAction::Builtin(SlashCommand::DebugConfig))
        );
    }

    #[test]
    fn clear_command_resolves_for_dispatch() {
        assert_eq!(
            find_command("clear", all_enabled_flags(), &[]),
            Some(SlashCommandAction::Builtin(SlashCommand::Clear))
        );
    }

    #[test]
    fn stop_command_resolves_for_dispatch() {
        assert_eq!(
            find_command("stop", all_enabled_flags(), &[]),
            Some(SlashCommandAction::Builtin(SlashCommand::Stop))
        );
    }

    #[test]
    fn clean_command_alias_resolves_for_dispatch() {
        assert_eq!(
            find_command("clean", all_enabled_flags(), &[]),
            Some(SlashCommandAction::Builtin(SlashCommand::Stop))
        );
    }

    #[test]
    fn service_tier_command_is_hidden_when_disabled() {
        let service_tier_commands = vec![ServiceTierCommand {
            service_tier: SERVICE_TIER_PRIORITY.into(),
            command: "fast".to_string(),
            name: "Fast".to_string(),
            description: "Fast tier".to_string(),
        }];
        let mut flags = all_enabled_flags();
        flags.fast_command_enabled = false;
        assert_eq!(find_command("fast", flags, &service_tier_commands), None);
    }

    #[test]
    fn goal_command_is_hidden_when_disabled() {
        let mut flags = all_enabled_flags();
        flags.goal_command_enabled = false;
        assert_eq!(find_command("goal", flags, &[]), None);
    }

    #[test]
    fn realtime_command_is_hidden_when_realtime_is_disabled() {
        let mut flags = all_enabled_flags();
        flags.realtime_conversation_enabled = false;
        assert_eq!(find_command("realtime", flags, &[]), None);
    }

    #[test]
    fn settings_command_is_hidden_when_realtime_is_disabled() {
        let mut flags = all_enabled_flags();
        flags.realtime_conversation_enabled = false;
        flags.audio_device_selection_enabled = false;
        assert_eq!(find_command("settings", flags, &[]), None);
    }

    #[test]
    fn settings_command_is_hidden_when_audio_device_selection_is_disabled() {
        let mut flags = all_enabled_flags();
        flags.audio_device_selection_enabled = false;
        assert_eq!(find_command("settings", flags, &[]), None);
    }

    #[test]
    fn side_conversation_hides_commands_without_side_flag() {
        let commands = commands_for_input(
            BuiltinCommandFlags {
                side_conversation_active: true,
                ..all_enabled_flags()
            },
            &[],
        )
        .into_iter()
        .map(|command| command_name(&command).to_string())
        .collect::<Vec<_>>();

        assert_eq!(
            commands,
            vec![
                "ide".to_string(),
                "copy".to_string(),
                "diff".to_string(),
                "mention".to_string(),
                "status".to_string(),
            ]
        );
    }

    #[test]
    fn side_conversation_exact_lookup_still_resolves_hidden_commands_for_dispatch_error() {
        assert_eq!(
            find_command(
                "review",
                BuiltinCommandFlags {
                    side_conversation_active: true,
                    ..all_enabled_flags()
                },
                &[],
            ),
            Some(SlashCommandAction::Builtin(SlashCommand::Review))
        );
    }

    #[test]
    fn service_tier_command_uses_name_then_id_on_collision() {
        let model = ModelPreset {
            id: "test".to_string(),
            model: "test".to_string(),
            display_name: "Test".to_string(),
            description: String::new(),
            default_reasoning_effort: codex_protocol::openai_models::ReasoningEffort::Medium,
            supported_reasoning_efforts: Vec::new(),
            supports_personality: false,
            service_tiers: vec![
                ModelServiceTier {
                    id: SERVICE_TIER_PRIORITY.into(),
                    name: "Fast".to_string(),
                    description: "Fast tier".to_string(),
                },
                ModelServiceTier {
                    id: ServiceTier::from("express"),
                    name: "Model".to_string(),
                    description: "Express tier".to_string(),
                },
            ],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            availability_nux: None,
            supported_in_api: true,
            input_modalities: Vec::new(),
        };

        let commands = service_tier_commands_for_model(&model);

        assert_eq!(
            commands,
            vec![
                ServiceTierCommand {
                    service_tier: SERVICE_TIER_PRIORITY.into(),
                    command: "fast".to_string(),
                    name: "Fast".to_string(),
                    description: "Fast tier".to_string(),
                },
                ServiceTierCommand {
                    service_tier: ServiceTier::from("express"),
                    command: "express".to_string(),
                    name: "Model".to_string(),
                    description: "Express tier".to_string(),
                },
            ]
        );
    }
}
