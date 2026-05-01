use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;

use crate::DefaultEnvironmentProvider;
use crate::DefaultEnvironmentSelection;
use crate::Environment;
use crate::EnvironmentProvider;
use crate::ExecServerError;
use crate::ExecServerRuntimePaths;
use crate::environment::LOCAL_ENVIRONMENT_ID;

const ENVIRONMENTS_TOML_FILE: &str = "environments.toml";

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
struct EnvironmentsToml {
    default: Option<String>,

    #[serde(default)]
    items: Vec<EnvironmentToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
struct EnvironmentToml {
    id: String,
    url: Option<String>,
    command: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TomlEnvironmentProvider {
    config: EnvironmentsToml,
}

impl TomlEnvironmentProvider {
    fn new(config: EnvironmentsToml) -> Result<Self, ExecServerError> {
        let ids = validate_config(&config)?;
        validate_default(config.default.as_deref(), &ids)?;
        Ok(Self { config })
    }
}

#[async_trait]
impl EnvironmentProvider for TomlEnvironmentProvider {
    async fn get_environments(
        &self,
        local_runtime_paths: &ExecServerRuntimePaths,
    ) -> Result<HashMap<String, Environment>, ExecServerError> {
        let mut environments = HashMap::from([(
            LOCAL_ENVIRONMENT_ID.to_string(),
            Environment::local(local_runtime_paths.clone()),
        )]);

        for item in &self.config.items {
            environments.insert(
                item.id.clone(),
                item.environment(Some(local_runtime_paths.clone())),
            );
        }

        Ok(environments)
    }

    fn default_environment_selection(&self) -> DefaultEnvironmentSelection {
        match self.config.default.as_deref().map(str::trim) {
            None => DefaultEnvironmentSelection::Environment(LOCAL_ENVIRONMENT_ID.to_string()),
            Some(default) if default.eq_ignore_ascii_case("none") => {
                DefaultEnvironmentSelection::Disabled
            }
            Some(default) => DefaultEnvironmentSelection::Environment(default.to_string()),
        }
    }
}

impl EnvironmentToml {
    fn environment(&self, local_runtime_paths: Option<ExecServerRuntimePaths>) -> Environment {
        match (self.url.as_deref(), self.command.as_deref()) {
            (Some(url), None) => {
                Environment::remote_inner(url.trim().to_string(), local_runtime_paths)
            }
            (None, Some(command)) => Environment::remote_stdio_shell_command(
                command.trim().to_string(),
                local_runtime_paths,
            ),
            _ => unreachable!("transport shape validated by TomlEnvironmentProvider::new"),
        }
    }
}

pub fn environment_provider_from_codex_home(
    codex_home: &Path,
) -> Result<Box<dyn EnvironmentProvider>, ExecServerError> {
    let path = codex_home.join(ENVIRONMENTS_TOML_FILE);
    if !path.try_exists().map_err(|err| {
        ExecServerError::Protocol(format!(
            "failed to inspect environment config `{}`: {err}",
            path.display()
        ))
    })? {
        return Ok(Box::new(DefaultEnvironmentProvider::from_env()));
    }

    let environments = load_environments_toml(&path)?;
    Ok(Box::new(TomlEnvironmentProvider::new(environments)?))
}

fn validate_config(config: &EnvironmentsToml) -> Result<HashSet<String>, ExecServerError> {
    let mut ids = HashSet::from([LOCAL_ENVIRONMENT_ID.to_string()]);
    for item in &config.items {
        validate_environment_item(item)?;
        if !ids.insert(item.id.clone()) {
            return Err(ExecServerError::Protocol(format!(
                "environment id `{}` is duplicated",
                item.id
            )));
        }
    }
    Ok(ids)
}

fn validate_default(default: Option<&str>, ids: &HashSet<String>) -> Result<(), ExecServerError> {
    let Some(default) = default.map(str::trim) else {
        return Ok(());
    };
    if default.is_empty() {
        return Err(ExecServerError::Protocol(
            "default environment id cannot be empty".to_string(),
        ));
    }
    if !default.eq_ignore_ascii_case("none") && !ids.contains(default) {
        return Err(ExecServerError::Protocol(format!(
            "default environment `{default}` is not configured"
        )));
    }
    Ok(())
}

fn validate_environment_item(item: &EnvironmentToml) -> Result<(), ExecServerError> {
    let id = item.id.trim();
    if id.is_empty() {
        return Err(ExecServerError::Protocol(
            "environment id cannot be empty".to_string(),
        ));
    }
    if id != item.id {
        return Err(ExecServerError::Protocol(format!(
            "environment id `{}` must not contain surrounding whitespace",
            item.id
        )));
    }
    if item.id == LOCAL_ENVIRONMENT_ID || item.id.eq_ignore_ascii_case("none") {
        return Err(ExecServerError::Protocol(format!(
            "environment id `{}` is reserved",
            item.id
        )));
    }

    match (item.url.as_deref(), item.command.as_deref()) {
        (Some(url), None) => validate_websocket_url(url),
        (None, Some(command)) if command.trim().is_empty() => Err(ExecServerError::Protocol(
            format!("environment `{}` command cannot be empty", item.id),
        )),
        (None, Some(_)) => Ok(()),
        (None, None) | (Some(_), Some(_)) => Err(ExecServerError::Protocol(format!(
            "environment `{}` must set exactly one of url or command",
            item.id
        ))),
    }
}

fn validate_websocket_url(url: &str) -> Result<(), ExecServerError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(ExecServerError::Protocol(
            "environment url cannot be empty".to_string(),
        ));
    }
    if !url.starts_with("ws://") && !url.starts_with("wss://") {
        return Err(ExecServerError::Protocol(format!(
            "environment url `{url}` must use ws:// or wss://"
        )));
    }
    Ok(())
}

fn load_environments_toml(path: &Path) -> Result<EnvironmentsToml, ExecServerError> {
    let contents = std::fs::read_to_string(path).map_err(|err| {
        ExecServerError::Protocol(format!(
            "failed to read environment config `{}`: {err}",
            path.display()
        ))
    })?;

    toml::from_str(&contents).map_err(|err| {
        ExecServerError::Protocol(format!(
            "failed to parse environment config `{}`: {err}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;

    fn test_runtime_paths() -> ExecServerRuntimePaths {
        ExecServerRuntimePaths::new(
            std::env::current_exe().expect("current exe"),
            /*codex_linux_sandbox_exe*/ None,
        )
        .expect("runtime paths")
    }

    #[tokio::test]
    async fn toml_provider_adds_implicit_local_and_configured_environments() {
        let provider = TomlEnvironmentProvider::new(EnvironmentsToml {
            default: Some("ssh-dev".to_string()),
            items: vec![
                EnvironmentToml {
                    id: "devbox".to_string(),
                    url: Some(" ws://127.0.0.1:8765 ".to_string()),
                    command: None,
                },
                EnvironmentToml {
                    id: "ssh-dev".to_string(),
                    url: None,
                    command: Some(" ssh dev \"codex exec-server --listen stdio\" ".to_string()),
                },
            ],
        })
        .expect("provider");
        let runtime_paths = test_runtime_paths();

        let environments = provider
            .get_environments(&runtime_paths)
            .await
            .expect("environments");

        assert!(!environments[LOCAL_ENVIRONMENT_ID].is_remote());
        assert_eq!(
            environments["devbox"].exec_server_url(),
            Some("ws://127.0.0.1:8765")
        );
        assert!(environments["ssh-dev"].is_remote());
        assert_eq!(
            provider.default_environment_selection(),
            DefaultEnvironmentSelection::Environment("ssh-dev".to_string())
        );
    }

    #[test]
    fn toml_provider_default_omitted_selects_local() {
        let provider = TomlEnvironmentProvider::new(EnvironmentsToml::default()).expect("provider");

        assert_eq!(
            provider.default_environment_selection(),
            DefaultEnvironmentSelection::Environment(LOCAL_ENVIRONMENT_ID.to_string())
        );
    }

    #[test]
    fn toml_provider_default_none_disables_default() {
        let provider = TomlEnvironmentProvider::new(EnvironmentsToml {
            default: Some("none".to_string()),
            items: Vec::new(),
        })
        .expect("provider");

        assert_eq!(
            provider.default_environment_selection(),
            DefaultEnvironmentSelection::Disabled
        );
    }

    #[test]
    fn toml_provider_rejects_invalid_items() {
        let cases = [
            (
                EnvironmentToml {
                    id: "local".to_string(),
                    url: Some("ws://127.0.0.1:8765".to_string()),
                    command: None,
                },
                "environment id `local` is reserved",
            ),
            (
                EnvironmentToml {
                    id: " devbox ".to_string(),
                    url: Some("ws://127.0.0.1:8765".to_string()),
                    command: None,
                },
                "environment id ` devbox ` must not contain surrounding whitespace",
            ),
            (
                EnvironmentToml {
                    id: "devbox".to_string(),
                    url: Some("http://127.0.0.1:8765".to_string()),
                    command: None,
                },
                "environment url `http://127.0.0.1:8765` must use ws:// or wss://",
            ),
            (
                EnvironmentToml {
                    id: "devbox".to_string(),
                    url: Some("ws://127.0.0.1:8765".to_string()),
                    command: Some("codex exec-server --listen stdio".to_string()),
                },
                "environment `devbox` must set exactly one of url or command",
            ),
            (
                EnvironmentToml {
                    id: "devbox".to_string(),
                    url: None,
                    command: Some(" ".to_string()),
                },
                "environment `devbox` command cannot be empty",
            ),
        ];

        for (item, expected) in cases {
            let err = TomlEnvironmentProvider::new(EnvironmentsToml {
                default: None,
                items: vec![item],
            })
            .expect_err("invalid item should fail");

            assert_eq!(
                err.to_string(),
                format!("exec-server protocol error: {expected}")
            );
        }
    }

    #[test]
    fn toml_provider_rejects_duplicate_ids() {
        let err = TomlEnvironmentProvider::new(EnvironmentsToml {
            default: None,
            items: vec![
                EnvironmentToml {
                    id: "devbox".to_string(),
                    url: Some("ws://127.0.0.1:8765".to_string()),
                    command: None,
                },
                EnvironmentToml {
                    id: "devbox".to_string(),
                    url: None,
                    command: Some("codex exec-server --listen stdio".to_string()),
                },
            ],
        })
        .expect_err("duplicate id should fail");

        assert_eq!(
            err.to_string(),
            "exec-server protocol error: environment id `devbox` is duplicated"
        );
    }

    #[test]
    fn toml_provider_rejects_unknown_default() {
        let err = TomlEnvironmentProvider::new(EnvironmentsToml {
            default: Some("missing".to_string()),
            items: Vec::new(),
        })
        .expect_err("unknown default should fail");

        assert_eq!(
            err.to_string(),
            "exec-server protocol error: default environment `missing` is not configured"
        );
    }

    #[test]
    fn load_environments_toml_reads_root_environment_list() {
        let codex_home = tempdir().expect("tempdir");
        let path = codex_home.path().join(ENVIRONMENTS_TOML_FILE);
        std::fs::write(
            &path,
            r#"
default = "ssh-dev"

[[items]]
id = "devbox"
url = "ws://127.0.0.1:4512"

[[items]]
id = "ssh-dev"
command = 'ssh dev "codex exec-server --listen stdio"'
"#,
        )
        .expect("write environments.toml");

        let environments = load_environments_toml(&path).expect("environments.toml");

        assert_eq!(environments.default.as_deref(), Some("ssh-dev"));
        assert_eq!(environments.items.len(), 2);
        assert_eq!(environments.items[0].id, "devbox");
        assert_eq!(
            environments.items[1].command.as_deref(),
            Some("ssh dev \"codex exec-server --listen stdio\"")
        );
    }

    #[test]
    fn environment_provider_from_codex_home_uses_present_environments_file() {
        let codex_home = tempdir().expect("tempdir");
        std::fs::write(
            codex_home.path().join(ENVIRONMENTS_TOML_FILE),
            r#"
default = "none"
"#,
        )
        .expect("write environments.toml");

        let provider =
            environment_provider_from_codex_home(codex_home.path()).expect("environment provider");

        assert_eq!(
            provider.default_environment_selection(),
            DefaultEnvironmentSelection::Disabled
        );
    }
}
