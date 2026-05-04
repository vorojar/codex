use crate::config_toml::ConfigToml;
use schemars::r#gen::SchemaSettings;
use schemars::schema::RootSchema;
use schemars::schema::Schema;
use schemars::schema::SchemaObject;
use serde_json::Value as JsonValue;
use std::collections::BTreeSet;
use std::sync::OnceLock;
use toml::Value as TomlValue;

static CONFIG_ENUM_FIELDS: OnceLock<Vec<EnumField>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnumField {
    value_path: Vec<PathSegment>,
    remove_path: Vec<PathSegment>,
    allowed_values: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathSegment {
    Key(String),
    MapValue,
}

/// Removes unrecognized string values from enum-typed config fields.
///
/// This keeps older clients from failing to load a config written by a newer
/// client that knows about a newly added enum variant. The field is treated as
/// unset, so the normal default/resolution path applies. Non-string shape
/// errors are left intact and still fail during typed deserialization.
pub fn sanitize_unknown_enum_values(root: &mut TomlValue) -> Vec<String> {
    let mut warnings = Vec::new();

    for field in config_enum_fields() {
        sanitize_enum_field(root, field, &mut warnings);
    }

    warnings
}

fn config_enum_fields() -> &'static [EnumField] {
    CONFIG_ENUM_FIELDS.get_or_init(|| {
        let root_schema = config_root_schema();
        let root = Schema::Object(root_schema.schema.clone());
        let mut fields = Vec::new();
        collect_enum_fields(&root, &root_schema, &mut Vec::new(), &mut fields);
        fields
    })
}

fn config_root_schema() -> RootSchema {
    SchemaSettings::draft07()
        .with(|settings| {
            settings.option_add_null_type = false;
        })
        .into_generator()
        .into_root_schema_for::<ConfigToml>()
}

fn collect_enum_fields(
    schema: &Schema,
    root_schema: &RootSchema,
    path: &mut Vec<PathSegment>,
    fields: &mut Vec<EnumField>,
) {
    let Schema::Object(schema_object) = schema else {
        return;
    };
    let schema_object = resolve_schema_object(schema_object, root_schema);

    if let Some(allowed_values) = string_enum_values(schema_object) {
        push_enum_field(path, path, allowed_values, fields);
        return;
    }

    if collect_string_union_enum(schema_object, path, fields) {
        return;
    }
    if collect_tagged_union_enum(schema_object, path, fields) {
        return;
    }

    if let Some(subschemas) = schema_object.subschemas.as_ref() {
        for schema in subschemas.all_of.iter().flatten() {
            collect_enum_fields(schema, root_schema, path, fields);
        }
        for schema in subschemas.any_of.iter().flatten() {
            collect_enum_fields(schema, root_schema, path, fields);
        }
        for schema in subschemas.one_of.iter().flatten() {
            collect_enum_fields(schema, root_schema, path, fields);
        }
    }

    let Some(object) = schema_object.object.as_ref() else {
        return;
    };

    for (property, property_schema) in &object.properties {
        path.push(PathSegment::Key(property.clone()));
        collect_enum_fields(property_schema, root_schema, path, fields);
        path.pop();
    }

    if let Some(additional_properties) = object.additional_properties.as_ref() {
        path.push(PathSegment::MapValue);
        collect_enum_fields(additional_properties, root_schema, path, fields);
        path.pop();
    }
}

fn resolve_schema_object<'a>(
    schema_object: &'a SchemaObject,
    root_schema: &'a RootSchema,
) -> &'a SchemaObject {
    let Some(reference) = schema_object.reference.as_deref() else {
        return schema_object;
    };
    let Some(definition_name) = reference.strip_prefix("#/definitions/") else {
        return schema_object;
    };
    let Some(Schema::Object(definition)) = root_schema.definitions.get(definition_name) else {
        return schema_object;
    };
    resolve_schema_object(definition, root_schema)
}

fn collect_string_union_enum(
    schema_object: &SchemaObject,
    path: &[PathSegment],
    fields: &mut Vec<EnumField>,
) -> bool {
    let Some(subschemas) = schema_object.subschemas.as_ref() else {
        return false;
    };
    let Some(one_of) = subschemas.one_of.as_ref() else {
        return false;
    };

    let mut allowed_values = BTreeSet::new();
    let mut found_string_enum = false;
    for schema in one_of {
        let Schema::Object(variant) = schema else {
            continue;
        };
        let Some(values) = string_enum_values(variant) else {
            continue;
        };
        found_string_enum = true;
        allowed_values.extend(values);
    }

    if found_string_enum {
        push_enum_field(path, path, allowed_values, fields);
    }
    found_string_enum
}

fn collect_tagged_union_enum(
    schema_object: &SchemaObject,
    path: &[PathSegment],
    fields: &mut Vec<EnumField>,
) -> bool {
    let Some(subschemas) = schema_object.subschemas.as_ref() else {
        return false;
    };
    let Some(one_of) = subschemas.one_of.as_ref() else {
        return false;
    };

    let mut allowed_values = BTreeSet::new();
    for schema in one_of {
        let Schema::Object(variant) = schema else {
            return false;
        };
        let Some(object) = variant.object.as_ref() else {
            return false;
        };
        let Some(tag_schema) = object.properties.get("type") else {
            return false;
        };
        let Schema::Object(tag_schema) = tag_schema else {
            return false;
        };
        let Some(values) = string_enum_values(tag_schema) else {
            return false;
        };
        allowed_values.extend(values);
    }

    if allowed_values.is_empty() {
        return false;
    }

    let mut value_path = path.to_vec();
    value_path.push(PathSegment::Key("type".to_string()));
    push_enum_field(&value_path, path, allowed_values, fields);
    true
}

fn string_enum_values(schema_object: &SchemaObject) -> Option<BTreeSet<String>> {
    let values = schema_object.enum_values.as_ref()?;
    let mut allowed_values = BTreeSet::new();
    for value in values {
        let JsonValue::String(value) = value else {
            return None;
        };
        allowed_values.insert(value.clone());
    }
    (!allowed_values.is_empty()).then_some(allowed_values)
}

fn push_enum_field(
    value_path: &[PathSegment],
    remove_path: &[PathSegment],
    allowed_values: BTreeSet<String>,
    fields: &mut Vec<EnumField>,
) {
    let field = EnumField {
        value_path: value_path.to_vec(),
        remove_path: remove_path.to_vec(),
        allowed_values,
    };
    if !fields.contains(&field) {
        fields.push(field);
    }
}

fn sanitize_enum_field(root: &mut TomlValue, field: &EnumField, warnings: &mut Vec<String>) {
    let paths = matching_paths(root, &field.value_path);
    for value_path in paths {
        let Some(raw_value) = value_at_path(root, &value_path).and_then(TomlValue::as_str) else {
            continue;
        };
        if field.allowed_values.contains(raw_value) {
            continue;
        }

        let field_path = display_path(&value_path);
        warnings.push(format!(
            "Ignoring unrecognized config value `{raw_value}` for `{field_path}`; using the default for this setting."
        ));
        tracing::warn!(
            field = field_path,
            value = raw_value,
            "ignoring unrecognized config enum value"
        );

        let remove_path = remove_path_for_match(field, &value_path);
        remove_value_at_path(root, &remove_path);
    }
}

fn matching_paths(root: &TomlValue, path: &[PathSegment]) -> Vec<Vec<String>> {
    let mut matches = Vec::new();
    collect_matching_paths(root, path, &mut Vec::new(), &mut matches);
    matches
}

fn collect_matching_paths(
    value: &TomlValue,
    path: &[PathSegment],
    current_path: &mut Vec<String>,
    matches: &mut Vec<Vec<String>>,
) {
    let Some((segment, rest)) = path.split_first() else {
        matches.push(current_path.clone());
        return;
    };
    let Some(table) = value.as_table() else {
        return;
    };

    match segment {
        PathSegment::Key(key) => {
            let Some(value) = table.get(key) else {
                return;
            };
            current_path.push(key.clone());
            collect_matching_paths(value, rest, current_path, matches);
            current_path.pop();
        }
        PathSegment::MapValue => {
            for (key, value) in table {
                current_path.push(key.clone());
                collect_matching_paths(value, rest, current_path, matches);
                current_path.pop();
            }
        }
    }
}

fn remove_path_for_match(field: &EnumField, value_path: &[String]) -> Vec<String> {
    field
        .remove_path
        .iter()
        .zip(value_path.iter())
        .map(|(segment, matched)| match segment {
            PathSegment::Key(key) => key.clone(),
            PathSegment::MapValue => matched.clone(),
        })
        .collect()
}

fn value_at_path<'a>(root: &'a TomlValue, path: &[String]) -> Option<&'a TomlValue> {
    let mut value = root;
    for part in path {
        value = value.as_table()?.get(part)?;
    }
    Some(value)
}

fn remove_value_at_path(root: &mut TomlValue, path: &[String]) {
    let Some((last, parent_path)) = path.split_last() else {
        return;
    };

    let mut value = root;
    for part in parent_path {
        let Some(next) = value.as_table_mut().and_then(|table| table.get_mut(part)) else {
            return;
        };
        value = next;
    }

    if let Some(table) = value.as_table_mut() {
        table.remove(last);
    }
}

fn display_path(path: &[String]) -> String {
    path.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn unknown_config_enum_values_are_removed_with_warnings() {
        let mut value: TomlValue = toml::from_str(
            r#"
service_tier = "ultrafast"
sandbox_mode = "workspace-write"

[profiles.future]
model_reasoning_effort = "huge"
model_verbosity = "high"

[projects."/tmp/project"]
trust_level = "very-trusted"

[tools.web_search]
context_size = "massive"
"#,
        )
        .expect("config should parse as TOML");

        let warnings = sanitize_unknown_enum_values(&mut value);
        let expected: TomlValue = toml::from_str(
            r#"
sandbox_mode = "workspace-write"

[profiles.future]
model_verbosity = "high"

[projects."/tmp/project"]

[tools.web_search]
"#,
        )
        .expect("expected TOML should parse");

        assert_eq!(
            (
                expected,
                vec![
                    "Ignoring unrecognized config value `ultrafast` for `service_tier`; using the default for this setting.".to_string(),
                    "Ignoring unrecognized config value `huge` for `profiles.future.model_reasoning_effort`; using the default for this setting.".to_string(),
                    "Ignoring unrecognized config value `very-trusted` for `projects./tmp/project.trust_level`; using the default for this setting.".to_string(),
                    "Ignoring unrecognized config value `massive` for `tools.web_search.context_size`; using the default for this setting.".to_string(),
                ],
            ),
            (value, warnings)
        );
    }

    #[test]
    fn unknown_config_enum_values_allow_config_toml_deserialization() {
        let mut value: TomlValue = toml::from_str(
            r#"
service_tier = "ultrafast"
model_reasoning_summary = "verbose"
approval_policy = "on-request"
"#,
        )
        .expect("config should parse as TOML");

        let warnings = sanitize_unknown_enum_values(&mut value);
        let config: ConfigToml = value.try_into().expect("config should deserialize");

        assert_eq!(
            (
                None,
                None,
                Some(codex_protocol::protocol::AskForApproval::OnRequest),
                vec![
                    "Ignoring unrecognized config value `ultrafast` for `service_tier`; using the default for this setting.".to_string(),
                    "Ignoring unrecognized config value `verbose` for `model_reasoning_summary`; using the default for this setting.".to_string(),
                ],
            ),
            (
                config.service_tier,
                config.model_reasoning_summary,
                config.approval_policy,
                warnings,
            )
        );
    }

    #[test]
    fn unknown_tagged_enum_removes_the_parent_field() {
        let mut value: TomlValue = toml::from_str(
            r#"
[experimental_thread_store]
type = "future"
endpoint = "https://example.test"
"#,
        )
        .expect("config should parse as TOML");

        let warnings = sanitize_unknown_enum_values(&mut value);
        let expected: TomlValue = toml::from_str("").expect("expected TOML should parse");

        assert_eq!(
            (
                expected,
                vec![
                    "Ignoring unrecognized config value `future` for `experimental_thread_store.type`; using the default for this setting.".to_string(),
                ],
            ),
            (value, warnings)
        );
    }
}
