use serde::de::DeserializeOwned;
use toml::Value as TomlValue;

pub(crate) fn deserialize_with_enum_warnings<T>(
    mut value: TomlValue,
) -> Result<(TomlValue, T, Vec<String>), toml::de::Error>
where
    T: DeserializeOwned,
{
    let mut warnings = Vec::new();

    loop {
        match serde_path_to_error::deserialize(value.clone()) {
            Ok(parsed) => return Ok((value, parsed, warnings)),
            Err(err) => {
                let path = err.path().to_string();
                let toml_error = err.into_inner();
                if !is_unknown_variant_error(&toml_error) {
                    return Err(toml_error);
                }

                let Some(invalid_value) = remove_value_at_path(&mut value, &path) else {
                    return Err(toml_error);
                };
                warnings.push(format!(
                    "Ignoring invalid config value at {path}: {invalid_value}"
                ));
            }
        }
    }
}

fn is_unknown_variant_error(err: &toml::de::Error) -> bool {
    err.message().contains("unknown variant")
}

fn remove_value_at_path(value: &mut TomlValue, path: &str) -> Option<TomlValue> {
    let mut parts = path.split('.').peekable();
    let mut current = value;

    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return current.as_table_mut()?.remove(part);
        }
        current = current.as_table_mut()?.get_mut(part)?;
    }

    None
}
