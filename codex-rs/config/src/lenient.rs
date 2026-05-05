use crate::config_toml::RealtimeTransport;
use crate::config_toml::RealtimeWsMode;
use crate::config_toml::RealtimeWsVersion;
use crate::permissions_toml::NetworkDomainPermissionToml;
use crate::permissions_toml::NetworkUnixSocketPermissionToml;
use crate::types::AppToolApproval;
use crate::types::AuthCredentialsStoreMode;
use crate::types::NotificationCondition;
use crate::types::NotificationMethod;
use crate::types::OAuthCredentialsStoreMode;
use crate::types::OtelExporterKind;
use crate::types::OtelHttpProtocol;
use crate::types::UriBasedFileOpener;
use crate::types::WindowsSandboxModeToml;
use codex_protocol::config_types::AltScreenMode;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::config_types::SandboxMode;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::ShellEnvironmentPolicyInherit;
use codex_protocol::config_types::TrustLevel;
use codex_protocol::config_types::Verbosity;
use codex_protocol::config_types::WebSearchContextSize;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::config_types::WebSearchUserLocationType;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AskForApproval;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use toml::Value as TomlValue;
use toml::map::Map as TomlMap;

#[derive(Deserialize)]
#[serde(untagged)]
enum Lenient<T> {
    Success(T),
    Failure,
}

pub(crate) fn sanitize_config_toml_enums(value: &mut TomlValue) -> Vec<String> {
    let mut warnings = Vec::new();
    let Some(table) = value.as_table_mut() else {
        return warnings;
    };

    sanitize_config_table(table, &mut warnings);
    warnings
}

fn sanitize_config_table(table: &mut TomlMap<String, TomlValue>, warnings: &mut Vec<String>) {
    remove_invalid_enum::<AskForApproval>(table, "approval_policy", "approval_policy", warnings);
    remove_invalid_enum::<ApprovalsReviewer>(
        table,
        "approvals_reviewer",
        "approvals_reviewer",
        warnings,
    );
    remove_invalid_enum::<SandboxMode>(table, "sandbox_mode", "sandbox_mode", warnings);
    remove_invalid_enum::<ReasoningEffort>(
        table,
        "model_reasoning_effort",
        "model_reasoning_effort",
        warnings,
    );
    remove_invalid_enum::<ReasoningEffort>(
        table,
        "plan_mode_reasoning_effort",
        "plan_mode_reasoning_effort",
        warnings,
    );
    remove_invalid_enum::<ReasoningSummary>(
        table,
        "model_reasoning_summary",
        "model_reasoning_summary",
        warnings,
    );
    remove_invalid_enum::<Verbosity>(table, "model_verbosity", "model_verbosity", warnings);
    remove_invalid_enum::<Personality>(table, "personality", "personality", warnings);
    remove_invalid_enum::<ServiceTier>(table, "service_tier", "service_tier", warnings);
    remove_invalid_enum::<ForcedLoginMethod>(
        table,
        "forced_login_method",
        "forced_login_method",
        warnings,
    );
    remove_invalid_enum::<AuthCredentialsStoreMode>(
        table,
        "cli_auth_credentials_store",
        "cli_auth_credentials_store",
        warnings,
    );
    remove_invalid_enum::<OAuthCredentialsStoreMode>(
        table,
        "mcp_oauth_credentials_store",
        "mcp_oauth_credentials_store",
        warnings,
    );
    remove_invalid_enum::<UriBasedFileOpener>(table, "file_opener", "file_opener", warnings);
    remove_invalid_enum::<ThreadStoreProbe>(
        table,
        "experimental_thread_store",
        "experimental_thread_store",
        warnings,
    );
    remove_invalid_enum::<WebSearchMode>(table, "web_search", "web_search", warnings);
    if let Some(tui) = table.get_mut("tui").and_then(TomlValue::as_table_mut) {
        remove_invalid_enum::<NotificationMethod>(
            tui,
            "notification_method",
            "tui.notification_method",
            warnings,
        );
        remove_invalid_enum::<NotificationCondition>(
            tui,
            "notification_condition",
            "tui.notification_condition",
            warnings,
        );
        remove_invalid_enum::<AltScreenMode>(
            tui,
            "alternate_screen",
            "tui.alternate_screen",
            warnings,
        );
    }

    if let Some(shell) = table
        .get_mut("shell_environment_policy")
        .and_then(TomlValue::as_table_mut)
    {
        remove_invalid_enum::<ShellEnvironmentPolicyInherit>(
            shell,
            "inherit",
            "shell_environment_policy.inherit",
            warnings,
        );
    }

    if let Some(windows) = table.get_mut("windows").and_then(TomlValue::as_table_mut) {
        remove_invalid_enum::<WindowsSandboxModeToml>(
            windows,
            "sandbox",
            "windows.sandbox",
            warnings,
        );
    }

    if let Some(realtime) = table.get_mut("realtime").and_then(TomlValue::as_table_mut) {
        remove_invalid_enum::<RealtimeWsVersion>(realtime, "version", "realtime.version", warnings);
        remove_invalid_enum::<RealtimeWsMode>(realtime, "type", "realtime.type", warnings);
        remove_invalid_enum::<RealtimeTransport>(
            realtime,
            "transport",
            "realtime.transport",
            warnings,
        );
    }

    if let Some(web_search_tool) = table
        .get_mut("tools")
        .and_then(TomlValue::as_table_mut)
        .and_then(|tools| tools.get_mut("web_search"))
        .and_then(TomlValue::as_table_mut)
    {
        remove_invalid_enum::<WebSearchContextSize>(
            web_search_tool,
            "search_context_size",
            "tools.web_search.search_context_size",
            warnings,
        );
        if let Some(user_location) = web_search_tool
            .get_mut("user_location")
            .and_then(TomlValue::as_table_mut)
        {
            remove_invalid_enum::<WebSearchUserLocationType>(
                user_location,
                "type",
                "tools.web_search.user_location.type",
                warnings,
            );
        }
    }

    if let Some(projects) = table.get_mut("projects").and_then(TomlValue::as_table_mut) {
        for (name, project) in projects {
            if let Some(project) = project.as_table_mut() {
                remove_invalid_enum::<TrustLevel>(
                    project,
                    "trust_level",
                    &format!("projects.{name}.trust_level"),
                    warnings,
                );
            }
        }
    }

    sanitize_profiles(table, warnings);
    sanitize_permissions(table, warnings);
    sanitize_mcp_servers(table, warnings);
    sanitize_plugins(table, warnings);
    sanitize_otel(table, warnings);
}

fn sanitize_profiles(table: &mut TomlMap<String, TomlValue>, warnings: &mut Vec<String>) {
    let Some(profiles) = table.get_mut("profiles").and_then(TomlValue::as_table_mut) else {
        return;
    };
    for (name, profile) in profiles {
        let Some(profile) = profile.as_table_mut() else {
            continue;
        };
        remove_invalid_enum::<ServiceTier>(
            profile,
            "service_tier",
            &format!("profiles.{name}.service_tier"),
            warnings,
        );
        remove_invalid_enum::<AskForApproval>(
            profile,
            "approval_policy",
            &format!("profiles.{name}.approval_policy"),
            warnings,
        );
        remove_invalid_enum::<ApprovalsReviewer>(
            profile,
            "approvals_reviewer",
            &format!("profiles.{name}.approvals_reviewer"),
            warnings,
        );
        remove_invalid_enum::<SandboxMode>(
            profile,
            "sandbox_mode",
            &format!("profiles.{name}.sandbox_mode"),
            warnings,
        );
        remove_invalid_enum::<ReasoningEffort>(
            profile,
            "model_reasoning_effort",
            &format!("profiles.{name}.model_reasoning_effort"),
            warnings,
        );
        remove_invalid_enum::<ReasoningEffort>(
            profile,
            "plan_mode_reasoning_effort",
            &format!("profiles.{name}.plan_mode_reasoning_effort"),
            warnings,
        );
        remove_invalid_enum::<ReasoningSummary>(
            profile,
            "model_reasoning_summary",
            &format!("profiles.{name}.model_reasoning_summary"),
            warnings,
        );
        remove_invalid_enum::<Verbosity>(
            profile,
            "model_verbosity",
            &format!("profiles.{name}.model_verbosity"),
            warnings,
        );
        remove_invalid_enum::<Personality>(
            profile,
            "personality",
            &format!("profiles.{name}.personality"),
            warnings,
        );
        remove_invalid_enum::<WebSearchMode>(
            profile,
            "web_search",
            &format!("profiles.{name}.web_search"),
            warnings,
        );
        if let Some(windows) = profile.get_mut("windows").and_then(TomlValue::as_table_mut) {
            remove_invalid_enum::<WindowsSandboxModeToml>(
                windows,
                "sandbox",
                &format!("profiles.{name}.windows.sandbox"),
                warnings,
            );
        }
    }
}

fn sanitize_permissions(table: &mut TomlMap<String, TomlValue>, warnings: &mut Vec<String>) {
    let Some(permissions) = table
        .get_mut("permissions")
        .and_then(TomlValue::as_table_mut)
    else {
        return;
    };
    for (profile_name, profile) in permissions {
        let Some(network) = profile
            .as_table_mut()
            .and_then(|profile| profile.get_mut("network"))
            .and_then(TomlValue::as_table_mut)
        else {
            continue;
        };
        if let Some(domains) = network.get_mut("domains").and_then(TomlValue::as_table_mut) {
            remove_invalid_map_enums::<NetworkDomainPermissionToml>(
                domains,
                &format!("permissions.{profile_name}.network.domains"),
                warnings,
            );
        }
        if let Some(unix_sockets) = network
            .get_mut("unix_sockets")
            .and_then(TomlValue::as_table_mut)
        {
            remove_invalid_map_enums::<NetworkUnixSocketPermissionToml>(
                unix_sockets,
                &format!("permissions.{profile_name}.network.unix_sockets"),
                warnings,
            );
        }
    }
}

fn sanitize_mcp_servers(table: &mut TomlMap<String, TomlValue>, warnings: &mut Vec<String>) {
    let Some(mcp_servers) = table
        .get_mut("mcp_servers")
        .and_then(TomlValue::as_table_mut)
    else {
        return;
    };
    for (server_name, server) in mcp_servers {
        let Some(server) = server.as_table_mut() else {
            continue;
        };
        remove_invalid_enum::<AppToolApproval>(
            server,
            "default_tools_approval_mode",
            &format!("mcp_servers.{server_name}.default_tools_approval_mode"),
            warnings,
        );
        if let Some(tools) = server.get_mut("tools").and_then(TomlValue::as_table_mut) {
            for (tool_name, tool) in tools {
                if let Some(tool) = tool.as_table_mut() {
                    remove_invalid_enum::<AppToolApproval>(
                        tool,
                        "approval_mode",
                        &format!("mcp_servers.{server_name}.tools.{tool_name}.approval_mode"),
                        warnings,
                    );
                }
            }
        }
    }
}

fn sanitize_plugins(table: &mut TomlMap<String, TomlValue>, warnings: &mut Vec<String>) {
    let Some(plugins) = table.get_mut("plugins").and_then(TomlValue::as_table_mut) else {
        return;
    };
    for (plugin_name, plugin) in plugins {
        let Some(mcp_servers) = plugin
            .as_table_mut()
            .and_then(|plugin| plugin.get_mut("mcp_servers"))
            .and_then(TomlValue::as_table_mut)
        else {
            continue;
        };
        for (server_name, server) in mcp_servers {
            let Some(server) = server.as_table_mut() else {
                continue;
            };
            remove_invalid_enum::<AppToolApproval>(
                server,
                "default_tools_approval_mode",
                &format!(
                    "plugins.{plugin_name}.mcp_servers.{server_name}.default_tools_approval_mode"
                ),
                warnings,
            );
            if let Some(tools) = server.get_mut("tools").and_then(TomlValue::as_table_mut) {
                for (tool_name, tool) in tools {
                    if let Some(tool) = tool.as_table_mut() {
                        remove_invalid_enum::<AppToolApproval>(
                            tool,
                            "approval_mode",
                            &format!(
                                "plugins.{plugin_name}.mcp_servers.{server_name}.tools.{tool_name}.approval_mode"
                            ),
                            warnings,
                        );
                    }
                }
            }
        }
    }
}

fn sanitize_otel(table: &mut TomlMap<String, TomlValue>, warnings: &mut Vec<String>) {
    let Some(otel) = table.get_mut("otel").and_then(TomlValue::as_table_mut) else {
        return;
    };
    remove_invalid_enum::<OtelHttpProtocol>(otel, "protocol", "otel.protocol", warnings);
    remove_invalid_enum::<OtelExporterKind>(otel, "exporter", "otel.exporter", warnings);
    remove_invalid_enum::<OtelExporterKind>(
        otel,
        "trace_exporter",
        "otel.trace_exporter",
        warnings,
    );
    remove_invalid_enum::<OtelExporterKind>(
        otel,
        "metrics_exporter",
        "otel.metrics_exporter",
        warnings,
    );
}

fn remove_invalid_map_enums<T>(
    table: &mut TomlMap<String, TomlValue>,
    path: &str,
    warnings: &mut Vec<String>,
) where
    T: DeserializeOwned,
{
    let invalid_keys = table
        .iter()
        .filter_map(|(key, value)| invalid_enum::<T>(value).then_some(key.clone()))
        .collect::<Vec<_>>();
    for key in invalid_keys {
        let Some(value) = table.remove(&key) else {
            continue;
        };
        warnings.push(invalid_enum_warning(&format!("{path}.{key}"), &value));
    }
}

fn remove_invalid_enum<T>(
    table: &mut TomlMap<String, TomlValue>,
    key: &str,
    path: &str,
    warnings: &mut Vec<String>,
) where
    T: DeserializeOwned,
{
    let Some(value) = table.get(key) else {
        return;
    };
    if !invalid_enum::<T>(value) {
        return;
    }
    let Some(value) = table.remove(key) else {
        return;
    };
    warnings.push(invalid_enum_warning(path, &value));
}

fn invalid_enum<T>(value: &TomlValue) -> bool
where
    T: DeserializeOwned,
{
    match value.clone().try_into::<Lenient<T>>() {
        Ok(Lenient::Success(_)) => false,
        Ok(Lenient::Failure) | Err(_) => true,
    }
}

fn invalid_enum_warning(path: &str, value: &TomlValue) -> String {
    format!("Ignoring invalid config value at {path}: {value}")
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ThreadStoreProbe {
    Local {},
    Remote {},
    InMemory {},
}
