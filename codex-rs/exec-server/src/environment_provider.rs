use std::collections::HashMap;

use async_trait::async_trait;

use crate::ExecServerError;
use crate::environment::CODEX_EXEC_SERVER_URL_ENV_VAR;
use crate::environment::LOCAL_ENVIRONMENT_ID;
use crate::environment::REMOTE_ENVIRONMENT_ID;

/// Provider-supplied environment definition consumed by `EnvironmentManager`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentConfiguration {
    pub exec_server_url: String,
}

/// Provider-supplied environment snapshot consumed by `EnvironmentManager`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentConfigurations {
    default_environment_id: Option<String>,
    environments: HashMap<String, EnvironmentConfiguration>,
}

impl EnvironmentConfigurations {
    pub fn new(
        default_environment_id: Option<String>,
        mut environments: HashMap<String, EnvironmentConfiguration>,
    ) -> Result<Self, ExecServerError> {
        for (id, configuration) in &mut environments {
            if id.is_empty() {
                return Err(ExecServerError::Protocol(
                    "environment configuration id cannot be empty".to_string(),
                ));
            }
            if id == LOCAL_ENVIRONMENT_ID {
                return Err(ExecServerError::Protocol(format!(
                    "provider environment configurations must not include `{LOCAL_ENVIRONMENT_ID}`"
                )));
            }

            match normalize_exec_server_url(Some(configuration.exec_server_url.clone())) {
                (Some(exec_server_url), false) => {
                    configuration.exec_server_url = exec_server_url;
                }
                (None, false) | (None, true) | (Some(_), true) => {
                    return Err(ExecServerError::Protocol(format!(
                        "environment configuration `{id}` must set a remote exec-server URL"
                    )));
                }
            }
        }

        if let Some(default_environment_id) = default_environment_id.as_deref()
            && default_environment_id != LOCAL_ENVIRONMENT_ID
            && !environments.contains_key(default_environment_id)
        {
            return Err(ExecServerError::Protocol(format!(
                "default environment id `{default_environment_id}` has no environment configuration"
            )));
        }

        Ok(Self {
            default_environment_id,
            environments,
        })
    }

    pub fn disabled() -> Self {
        Self {
            default_environment_id: None,
            environments: HashMap::new(),
        }
    }

    pub fn local_default() -> Self {
        Self {
            default_environment_id: Some(LOCAL_ENVIRONMENT_ID.to_string()),
            environments: HashMap::new(),
        }
    }

    fn remote_default(exec_server_url: String) -> Self {
        Self {
            default_environment_id: Some(REMOTE_ENVIRONMENT_ID.to_string()),
            environments: HashMap::from([(
                REMOTE_ENVIRONMENT_ID.to_string(),
                EnvironmentConfiguration { exec_server_url },
            )]),
        }
    }

    pub fn default_environment_id(&self) -> Option<&str> {
        self.default_environment_id.as_deref()
    }

    pub fn into_environments(self) -> HashMap<String, EnvironmentConfiguration> {
        self.environments
    }
}

/// Lists the concrete environment configurations available to Codex.
///
/// Implementations should return the provider-owned portion of the startup
/// snapshot that `EnvironmentManager` will cache. The local environment is
/// always supplied by `EnvironmentManager`; providers only need to set
/// `local` as the default when they want local to be selected by default.
#[async_trait]
pub trait EnvironmentProvider: Send + Sync {
    /// Returns the environment configurations available for a new manager.
    async fn get_environments(&self) -> Result<EnvironmentConfigurations, ExecServerError>;
}

/// Default provider backed by `CODEX_EXEC_SERVER_URL`.
#[derive(Clone, Debug)]
pub struct DefaultEnvironmentProvider {
    exec_server_url: Option<String>,
}

impl DefaultEnvironmentProvider {
    /// Builds a provider from an already-read raw `CODEX_EXEC_SERVER_URL` value.
    pub fn new(exec_server_url: Option<String>) -> Self {
        Self { exec_server_url }
    }

    /// Builds a provider by reading `CODEX_EXEC_SERVER_URL`.
    pub fn from_env() -> Self {
        Self::new(std::env::var(CODEX_EXEC_SERVER_URL_ENV_VAR).ok())
    }

    pub(crate) fn environment_configurations(&self) -> EnvironmentConfigurations {
        let (exec_server_url, environment_disabled) =
            normalize_exec_server_url(self.exec_server_url.clone());

        if let Some(exec_server_url) = exec_server_url {
            EnvironmentConfigurations::remote_default(exec_server_url)
        } else if !environment_disabled {
            EnvironmentConfigurations::local_default()
        } else {
            EnvironmentConfigurations::disabled()
        }
    }
}

#[async_trait]
impl EnvironmentProvider for DefaultEnvironmentProvider {
    async fn get_environments(&self) -> Result<EnvironmentConfigurations, ExecServerError> {
        Ok(self.environment_configurations())
    }
}

pub(crate) fn normalize_exec_server_url(exec_server_url: Option<String>) -> (Option<String>, bool) {
    match exec_server_url.as_deref().map(str::trim) {
        None | Some("") => (None, false),
        Some(url) if url.eq_ignore_ascii_case("none") => (None, true),
        Some(url) => (Some(url.to_string()), false),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test]
    async fn default_provider_uses_local_environment_when_url_is_missing() {
        let provider = DefaultEnvironmentProvider::new(/*exec_server_url*/ None);

        assert_eq!(
            provider.get_environments().await.expect("environments"),
            EnvironmentConfigurations::local_default()
        );
    }

    #[tokio::test]
    async fn default_provider_uses_local_environment_when_url_is_empty() {
        let provider = DefaultEnvironmentProvider::new(Some(String::new()));

        assert_eq!(
            provider.get_environments().await.expect("environments"),
            EnvironmentConfigurations::local_default()
        );
    }

    #[tokio::test]
    async fn default_provider_disables_default_environment_for_none_value() {
        let provider = DefaultEnvironmentProvider::new(Some("none".to_string()));

        assert_eq!(
            provider.get_environments().await.expect("environments"),
            EnvironmentConfigurations::disabled()
        );
    }

    #[tokio::test]
    async fn default_provider_adds_remote_environment_for_websocket_url() {
        let provider = DefaultEnvironmentProvider::new(Some("ws://127.0.0.1:8765".to_string()));

        assert_eq!(
            provider.get_environments().await.expect("environments"),
            EnvironmentConfigurations::new(
                Some(REMOTE_ENVIRONMENT_ID.to_string()),
                HashMap::from([(
                    REMOTE_ENVIRONMENT_ID.to_string(),
                    EnvironmentConfiguration {
                        exec_server_url: "ws://127.0.0.1:8765".to_string(),
                    },
                )]),
            )
            .expect("environment configurations")
        );
    }

    #[test]
    fn environment_configurations_rejects_local_provider_entry() {
        let err = EnvironmentConfigurations::new(
            Some(LOCAL_ENVIRONMENT_ID.to_string()),
            HashMap::from([(
                LOCAL_ENVIRONMENT_ID.to_string(),
                EnvironmentConfiguration {
                    exec_server_url: "ws://127.0.0.1:8765".to_string(),
                },
            )]),
        )
        .expect_err("local provider entry should fail");

        assert_eq!(
            err.to_string(),
            "exec-server protocol error: provider environment configurations must not include `local`"
        );
    }

    #[test]
    fn environment_configurations_rejects_missing_default() {
        let err =
            EnvironmentConfigurations::new(Some(REMOTE_ENVIRONMENT_ID.to_string()), HashMap::new())
                .expect_err("missing default should fail");

        assert_eq!(
            err.to_string(),
            "exec-server protocol error: default environment id `remote` has no environment configuration"
        );
    }

    #[test]
    fn environment_configurations_rejects_empty_exec_server_url() {
        let err = EnvironmentConfigurations::new(
            Some(REMOTE_ENVIRONMENT_ID.to_string()),
            HashMap::from([(
                REMOTE_ENVIRONMENT_ID.to_string(),
                EnvironmentConfiguration {
                    exec_server_url: String::new(),
                },
            )]),
        )
        .expect_err("empty URL should fail");

        assert_eq!(
            err.to_string(),
            "exec-server protocol error: environment configuration `remote` must set a remote exec-server URL"
        );
    }

    #[test]
    fn environment_configurations_rejects_disabled_exec_server_url() {
        let err = EnvironmentConfigurations::new(
            Some(REMOTE_ENVIRONMENT_ID.to_string()),
            HashMap::from([(
                REMOTE_ENVIRONMENT_ID.to_string(),
                EnvironmentConfiguration {
                    exec_server_url: "none".to_string(),
                },
            )]),
        )
        .expect_err("disabled URL should fail");

        assert_eq!(
            err.to_string(),
            "exec-server protocol error: environment configuration `remote` must set a remote exec-server URL"
        );
    }

    #[test]
    fn environment_configurations_normalizes_exec_server_url() {
        let configurations = EnvironmentConfigurations::new(
            Some(REMOTE_ENVIRONMENT_ID.to_string()),
            HashMap::from([(
                REMOTE_ENVIRONMENT_ID.to_string(),
                EnvironmentConfiguration {
                    exec_server_url: " ws://127.0.0.1:8765 ".to_string(),
                },
            )]),
        )
        .expect("environment configurations");

        assert_eq!(
            configurations,
            EnvironmentConfigurations::remote_default("ws://127.0.0.1:8765".to_string())
        );
    }
}
