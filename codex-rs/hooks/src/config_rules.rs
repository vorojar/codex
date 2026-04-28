use std::collections::HashSet;

use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStack;
use codex_config::ConfigLayerStackOrdering;
use codex_config::HookConfig;
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

        for entry in hooks.config {
            let Some(key) = hook_config_key(&entry) else {
                continue;
            };
            // Later layers win: an enabled entry removes a disabled override
            // for the same key, while a disabled entry inserts it.
            if entry.enabled {
                disabled_keys.remove(&key);
            } else {
                disabled_keys.insert(key);
            }
        }
    }

    disabled_keys
}

fn hook_config_key(entry: &HookConfig) -> Option<String> {
    let key = entry.key.as_deref().map(str::trim).unwrap_or_default();
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
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
                    config_with_hook_override(key, false),
                ),
                ConfigLayerEntry::new(
                    ConfigLayerSource::SessionFlags,
                    config_with_hook_override(key, true),
                ),
            ],
            Default::default(),
            Default::default(),
        )
        .expect("config layer stack");

        assert_eq!(disabled_hook_keys_from_stack(Some(&stack)), HashSet::new());
    }

    fn config_with_hook_override(key: &str, enabled: bool) -> TomlValue {
        let mut config = TomlValue::Table(Default::default());
        let TomlValue::Table(config_entries) = &mut config else {
            unreachable!("config root should be a table");
        };
        let mut hooks = TomlValue::Table(Default::default());
        let TomlValue::Table(hook_entries) = &mut hooks else {
            unreachable!("hooks should be a table");
        };
        let mut hook_override = TomlValue::Table(Default::default());
        let TomlValue::Table(hook_override_entries) = &mut hook_override else {
            unreachable!("hook override should be a table");
        };
        hook_override_entries.insert("key".to_string(), TomlValue::String(key.to_string()));
        hook_override_entries.insert("enabled".to_string(), TomlValue::Boolean(enabled));
        hook_entries.insert("config".to_string(), TomlValue::Array(vec![hook_override]));
        config_entries.insert("hooks".to_string(), hooks);
        config
    }
}
