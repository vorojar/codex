use crate::config_toml::RealtimeTransport;
use crate::config_toml::RealtimeWsMode;
use crate::config_toml::ThreadStoreToml;
use crate::types::ApprovalsReviewer;
use crate::types::AuthCredentialsStoreMode;
use crate::types::HistoryPersistence;
use crate::types::NotificationCondition;
use crate::types::NotificationMethod;
use crate::types::OAuthCredentialsStoreMode;
use crate::types::UriBasedFileOpener;
use crate::types::WindowsSandboxModeToml;
use codex_protocol::config_types::AltScreenMode;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::config_types::SandboxMode;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::TrustLevel;
use codex_protocol::config_types::Verbosity;
use codex_protocol::config_types::WebSearchContextSize;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AskForApproval;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use toml::Value as TomlValue;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LenientEnum<T> {
    Known(T),
    Unknown(String),
    Other(TomlValue),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathSegment {
    Key(&'static str),
    MapValue,
}

macro_rules! sanitize_config_enums {
    ($root:expr, $warnings:expr, {$($ty:ty => [$($path:tt).+],)+}) => {
        $(
            sanitize_enum::<$ty>($root, &path_segments!($($path).+), $warnings);
        )+
    };
}

macro_rules! path_segments {
    ($($path:tt).+) => {
        vec![$(path_segment!($path)),+]
    };
}

macro_rules! path_segment {
    (*) => {
        PathSegment::MapValue
    };
    (r#type) => {
        PathSegment::Key("type")
    };
    ($key:ident) => {
        PathSegment::Key(stringify!($key))
    };
}

/// Removes unrecognized string values from enum-typed config fields.
///
/// This keeps older clients from failing to load a config written by a newer
/// client that knows about a newly added enum variant. The field is treated as
/// unset, so the normal default/resolution path applies. Non-string shape
/// errors are left intact and still fail during typed deserialization.
pub fn sanitize_unknown_enum_values(root: &mut TomlValue) -> Vec<String> {
    let mut warnings = Vec::new();

    sanitize_config_enums!(root, &mut warnings, {
        AskForApproval => [approval_policy],
        ApprovalsReviewer => [approvals_reviewer],
        SandboxMode => [sandbox_mode],
        ForcedLoginMethod => [forced_login_method],
        AuthCredentialsStoreMode => [cli_auth_credentials_store],
        OAuthCredentialsStoreMode => [mcp_oauth_credentials_store],
        UriBasedFileOpener => [file_opener],
        ReasoningEffort => [model_reasoning_effort],
        ReasoningEffort => [plan_mode_reasoning_effort],
        ReasoningSummary => [model_reasoning_summary],
        Verbosity => [model_verbosity],
        Personality => [personality],
        ServiceTier => [service_tier],
        WebSearchMode => [web_search],
        HistoryPersistence => [history.persistence],
        NotificationMethod => [tui.notification_method],
        NotificationCondition => [tui.notification_condition],
        AltScreenMode => [tui.alternate_screen],
        WebSearchContextSize => [tools.web_search.context_size],
        TrustLevel => [projects.*.trust_level],
        RealtimeWsMode => [realtime.r#type],
        RealtimeTransport => [realtime.transport],
        WindowsSandboxModeToml => [windows.sandbox],
        AskForApproval => [profiles.*.approval_policy],
        ApprovalsReviewer => [profiles.*.approvals_reviewer],
        SandboxMode => [profiles.*.sandbox_mode],
        ReasoningEffort => [profiles.*.model_reasoning_effort],
        ReasoningEffort => [profiles.*.plan_mode_reasoning_effort],
        ReasoningSummary => [profiles.*.model_reasoning_summary],
        Verbosity => [profiles.*.model_verbosity],
        Personality => [profiles.*.personality],
        ServiceTier => [profiles.*.service_tier],
        WebSearchMode => [profiles.*.web_search],
        WebSearchContextSize => [profiles.*.tools.web_search.context_size],
    });
    sanitize_tagged_enum::<ThreadStoreToml>(
        root,
        &path_segments!(experimental_thread_store),
        "type",
        &mut warnings,
    );

    warnings
}

fn sanitize_enum<T>(root: &mut TomlValue, path: &[PathSegment], warnings: &mut Vec<String>)
where
    T: DeserializeOwned,
{
    let paths = matching_paths(root, path);
    for value_path in paths {
        let Some(value) = value_at_path(root, &value_path).cloned() else {
            continue;
        };
        match value.try_into::<LenientEnum<T>>() {
            Ok(LenientEnum::Known(_)) => {}
            Ok(LenientEnum::Unknown(raw_value)) => {
                warn_and_remove(root, &value_path, &value_path, &raw_value, warnings);
            }
            Ok(LenientEnum::Other(_)) | Err(_) => {}
        };
    }
}

fn sanitize_tagged_enum<T>(
    root: &mut TomlValue,
    path: &[PathSegment],
    tag_key: &'static str,
    warnings: &mut Vec<String>,
) where
    T: DeserializeOwned,
{
    let parent_paths = matching_paths(root, path);
    for parent_path in parent_paths {
        let mut tag_path = parent_path.clone();
        tag_path.push(tag_key.to_string());
        let Some(value) = value_at_path(root, &parent_path).cloned() else {
            continue;
        };
        match value.try_into::<LenientEnum<T>>() {
            Ok(LenientEnum::Known(_)) => {}
            Ok(LenientEnum::Other(table_value)) => {
                let Some(raw_value) = table_value
                    .get(tag_key)
                    .and_then(TomlValue::as_str)
                    .map(str::to_string)
                else {
                    continue;
                };
                warn_and_remove(root, &tag_path, &parent_path, &raw_value, warnings);
            }
            Ok(LenientEnum::Unknown(_)) | Err(_) => {}
        };
    }
}

fn warn_and_remove(
    root: &mut TomlValue,
    value_path: &[String],
    remove_path: &[String],
    raw_value: &str,
    warnings: &mut Vec<String>,
) {
    let field_path = value_path.join(".");
    warnings.push(format!(
        "Ignoring unrecognized config value `{raw_value}` for `{field_path}`; using the default for this setting."
    ));
    tracing::warn!(
        field = field_path,
        value = raw_value,
        "ignoring unrecognized config enum value"
    );
    remove_value_at_path(root, remove_path);
}

fn matching_paths(root: &TomlValue, path: &[PathSegment]) -> Vec<Vec<String>> {
    let mut matches = Vec::new();
    collect_matching_paths(root, path, &mut Vec::new(), &mut matches);
    matches
}

fn collect_matching_paths(
    current: &TomlValue,
    remaining_path: &[PathSegment],
    matched_path: &mut Vec<String>,
    matches: &mut Vec<Vec<String>>,
) {
    let Some((segment, remaining_path)) = remaining_path.split_first() else {
        matches.push(matched_path.clone());
        return;
    };

    match segment {
        PathSegment::Key(key) => {
            let Some(next) = current.get(*key) else {
                return;
            };
            matched_path.push((*key).to_string());
            collect_matching_paths(next, remaining_path, matched_path, matches);
            matched_path.pop();
        }
        PathSegment::MapValue => {
            let Some(table) = current.as_table() else {
                return;
            };
            for (key, next) in table {
                matched_path.push(key.clone());
                collect_matching_paths(next, remaining_path, matched_path, matches);
                matched_path.pop();
            }
        }
    }
}

fn value_at_path<'a>(root: &'a TomlValue, path: &[String]) -> Option<&'a TomlValue> {
    let mut current = root;
    for segment in path {
        current = current.get(segment)?;
    }
    Some(current)
}

fn remove_value_at_path(root: &mut TomlValue, path: &[String]) {
    let Some((last_segment, parent_path)) = path.split_last() else {
        return;
    };

    let Some(parent) = value_at_path_mut(root, parent_path) else {
        return;
    };
    let Some(table) = parent.as_table_mut() else {
        return;
    };
    table.remove(last_segment);
}

fn value_at_path_mut<'a>(root: &'a mut TomlValue, path: &[String]) -> Option<&'a mut TomlValue> {
    let mut current = root;
    for segment in path {
        current = current.get_mut(segment)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_toml::ConfigToml;
    use pretty_assertions::assert_eq;

    #[test]
    fn unknown_config_enum_values_are_removed_with_warnings() {
        let mut value = r#"
approval_policy = "maybe"
model = "gpt-5"
service_tier = "ultrafast"

[profiles.work]
model = "gpt-5-codex"
model_reasoning_effort = "maximum"

[projects."/tmp/project"]
trust_level = "somewhat"
"#
        .parse::<TomlValue>()
        .expect("config should parse as toml");

        let warnings = sanitize_unknown_enum_values(&mut value);

        let expected_value = r#"
model = "gpt-5"

[profiles.work]
model = "gpt-5-codex"

[projects."/tmp/project"]
"#
        .parse::<TomlValue>()
        .expect("expected config should parse as toml");
        let expected_warnings = vec![
            "Ignoring unrecognized config value `maybe` for `approval_policy`; using the default for this setting.".to_string(),
            "Ignoring unrecognized config value `ultrafast` for `service_tier`; using the default for this setting.".to_string(),
            "Ignoring unrecognized config value `somewhat` for `projects./tmp/project.trust_level`; using the default for this setting.".to_string(),
            "Ignoring unrecognized config value `maximum` for `profiles.work.model_reasoning_effort`; using the default for this setting.".to_string(),
        ];
        assert_eq!((value, warnings), (expected_value, expected_warnings));
    }

    #[test]
    fn unknown_config_enum_values_allow_config_toml_deserialization() {
        let mut value = r#"
model = "gpt-5"
service_tier = "ultrafast"
"#
        .parse::<TomlValue>()
        .expect("config should parse as toml");

        let warnings = sanitize_unknown_enum_values(&mut value);
        let config: ConfigToml = value.try_into().expect("config should deserialize");

        let expected_warnings = vec![
            "Ignoring unrecognized config value `ultrafast` for `service_tier`; using the default for this setting.".to_string(),
        ];
        assert_eq!((config.service_tier, warnings), (None, expected_warnings));
    }

    #[test]
    fn unknown_tagged_enum_removes_the_parent_field() {
        let mut value = r#"
model = "gpt-5"

[experimental_thread_store]
type = "future_store"
endpoint = "https://example.com"
"#
        .parse::<TomlValue>()
        .expect("config should parse as toml");

        let warnings = sanitize_unknown_enum_values(&mut value);

        let expected_value = r#"
model = "gpt-5"
"#
        .parse::<TomlValue>()
        .expect("expected config should parse as toml");
        let expected_warnings = vec![
            "Ignoring unrecognized config value `future_store` for `experimental_thread_store.type`; using the default for this setting.".to_string(),
        ];
        assert_eq!((value, warnings), (expected_value, expected_warnings));
    }
}
