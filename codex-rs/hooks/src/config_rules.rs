use std::collections::HashSet;

use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStack;
use codex_config::ConfigLayerStackOrdering;
use codex_config::HooksToml;

/// Build hook enablement rules from config layers that are allowed to override
/// user preferences.
///
/// This intentionally reads only user and session flag layers, including
/// disabled layers, to match the skills config behavior. Project, managed, and
/// plugin layers can discover hooks, but they do not get to write user
/// enablement state.
pub(crate) fn disabled_hook_keys_from_stack(
    config_layer_stack: Option<&ConfigLayerStack>,
) -> HashSet<String> {
    let Some(config_layer_stack) = config_layer_stack else {
        return HashSet::new();
    };

    let mut disabled_keys = HashSet::new();
    for layer in config_layer_stack.get_layers(
        ConfigLayerStackOrdering::LowestPrecedenceFirst,
        /*include_disabled*/ true,
    ) {
        if !matches!(
            layer.name,
            ConfigLayerSource::User { .. } | ConfigLayerSource::SessionFlags
        ) {
            continue;
        }

        let Some(hooks_value) = layer.config.get("hooks") else {
            continue;
        };
        let hooks: HooksToml = match hooks_value.clone().try_into() {
            Ok(hooks) => hooks,
            Err(_) => {
                continue;
            }
        };

        for (key, state) in hooks.state {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            // Later layers win. Hooks without an explicit enabled override can
            // still carry future per-hook state without changing enablement.
            match state.enabled {
                Some(false) => {
                    disabled_keys.insert(key.to_string());
                }
                Some(true) => {
                    disabled_keys.remove(key);
                }
                None => {}
            }
        }
    }

    disabled_keys
}

#[cfg(test)]
mod tests {
    use codex_config::ConfigLayerEntry;
    use codex_config::TomlValue;
    use codex_utils_absolute_path::test_support::PathBufExt;
    use codex_utils_absolute_path::test_support::test_path_buf;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn disabled_hook_keys_from_stack_respects_layer_precedence() {
        let key = "file:/tmp/hooks.json:pre_tool_use:0:0";
        let stack = ConfigLayerStack::new(
            vec![
                ConfigLayerEntry::new(
                    ConfigLayerSource::User {
                        file: test_path_buf("/tmp/config.toml").abs(),
                    },
                    config_with_hook_override(key, Some(false)),
                ),
                ConfigLayerEntry::new(
                    ConfigLayerSource::SessionFlags,
                    config_with_hook_override(key, Some(true)),
                ),
            ],
            Default::default(),
            Default::default(),
        )
        .expect("config layer stack");

        assert_eq!(disabled_hook_keys_from_stack(Some(&stack)), HashSet::new());
    }

    fn config_with_hook_override(key: &str, enabled: Option<bool>) -> TomlValue {
        let mut config = TomlValue::Table(Default::default());
        let TomlValue::Table(config_entries) = &mut config else {
            unreachable!("config root should be a table");
        };
        let mut hooks = TomlValue::Table(Default::default());
        let TomlValue::Table(hook_entries) = &mut hooks else {
            unreachable!("hooks should be a table");
        };
        let mut state_entries = TomlValue::Table(Default::default());
        let TomlValue::Table(state_map) = &mut state_entries else {
            unreachable!("state should be a table");
        };
        let mut hook_state = TomlValue::Table(Default::default());
        let TomlValue::Table(hook_state_entries) = &mut hook_state else {
            unreachable!("hook state should be a table");
        };
        if let Some(enabled) = enabled {
            hook_state_entries.insert("enabled".to_string(), TomlValue::Boolean(enabled));
        }
        state_map.insert(key.to_string(), hook_state);
        hook_entries.insert("state".to_string(), state_entries);
        config_entries.insert("hooks".to_string(), hooks);
        config
    }
}
