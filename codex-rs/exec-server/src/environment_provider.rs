use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;

use crate::ExecServerError;
use crate::environment::CODEX_EXEC_SERVER_URL_ENV_VAR;
use crate::environment::LOCAL_ENVIRONMENT_ID;
use crate::environment::REMOTE_ENVIRONMENT_ID;

/// Resolved connection details for a provider-supplied environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedEnvironment {
    pub exec_server_url: String,
}

/// Resolves provider-supplied environment connection details on demand.
#[async_trait]
pub trait EnvironmentResolver: Send + Sync + fmt::Debug {
    async fn resolve(&self) -> Result<ResolvedEnvironment, ExecServerError>;
}

/// Provider-supplied environment definition consumed by `EnvironmentManager`.
#[derive(Clone, Debug)]
pub struct EnvironmentConfiguration {
    static_exec_server_url: Option<String>,
    resolver: Arc<dyn EnvironmentResolver>,
}

impl EnvironmentConfiguration {
    pub fn static_url(exec_server_url: String) -> Result<Self, ExecServerError> {
        let exec_server_url = normalize_remote_exec_server_url("<static>", exec_server_url)?;
        Ok(Self {
            static_exec_server_url: Some(exec_server_url.clone()),
            resolver: Arc::new(StaticEnvironmentResolver { exec_server_url }),
        })
    }

    pub fn with_resolver<R>(resolver: R) -> Self
    where
        R: EnvironmentResolver + 'static,
    {
        Self {
            static_exec_server_url: None,
            resolver: Arc::new(resolver),
        }
    }

    pub fn from_resolver_fn<F>(resolver: F) -> Self
    where
        F: Fn() -> BoxFuture<'static, Result<ResolvedEnvironment, ExecServerError>>
            + Send
            + Sync
            + 'static,
    {
        Self::with_resolver(FnEnvironmentResolver {
            resolver: Arc::new(resolver),
        })
    }

    pub async fn resolve(&self) -> Result<ResolvedEnvironment, ExecServerError> {
        self.resolver.resolve().await
    }

    pub(crate) fn static_exec_server_url(&self) -> Option<&str> {
        self.static_exec_server_url.as_deref()
    }

    pub(crate) fn resolver(&self) -> Arc<dyn EnvironmentResolver> {
        Arc::clone(&self.resolver)
    }
}

#[derive(Debug)]
struct StaticEnvironmentResolver {
    exec_server_url: String,
}

#[async_trait]
impl EnvironmentResolver for StaticEnvironmentResolver {
    async fn resolve(&self) -> Result<ResolvedEnvironment, ExecServerError> {
        Ok(ResolvedEnvironment {
            exec_server_url: self.exec_server_url.clone(),
        })
    }
}

struct FnEnvironmentResolver {
    resolver: Arc<
        dyn Fn() -> BoxFuture<'static, Result<ResolvedEnvironment, ExecServerError>> + Send + Sync,
    >,
}

impl fmt::Debug for FnEnvironmentResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnEnvironmentResolver")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl EnvironmentResolver for FnEnvironmentResolver {
    async fn resolve(&self) -> Result<ResolvedEnvironment, ExecServerError> {
        (self.resolver)().await
    }
}

/// Provider-supplied environment snapshot consumed by `EnvironmentManager`.
#[derive(Clone, Debug)]
pub struct EnvironmentConfigurations {
    default_environment_id: Option<String>,
    environments: HashMap<String, EnvironmentConfiguration>,
}

impl EnvironmentConfigurations {
    pub fn new(
        default_environment_id: Option<String>,
        environments: HashMap<String, EnvironmentConfiguration>,
    ) -> Result<Self, ExecServerError> {
        for id in environments.keys() {
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
                EnvironmentConfiguration::static_url(exec_server_url)
                    .expect("remote default provider configuration should be valid"),
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
/// Remote configurations carry their own resolver so providers can choose
/// between static URLs and dynamic, on-demand endpoint lookup.
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

pub(crate) fn normalize_remote_exec_server_url(
    environment_id: &str,
    exec_server_url: String,
) -> Result<String, ExecServerError> {
    match normalize_exec_server_url(Some(exec_server_url)) {
        (Some(exec_server_url), false) => Ok(exec_server_url),
        (None, false) | (None, true) | (Some(_), true) => Err(ExecServerError::Protocol(format!(
            "environment configuration `{environment_id}` must resolve to a remote exec-server URL"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use futures::FutureExt;
    use pretty_assertions::assert_eq;

    use super::*;

    fn assert_local_default(configurations: EnvironmentConfigurations) {
        assert_eq!(
            configurations.default_environment_id(),
            Some(LOCAL_ENVIRONMENT_ID)
        );
        assert!(configurations.environments.is_empty());
    }

    fn assert_disabled(configurations: EnvironmentConfigurations) {
        assert_eq!(configurations.default_environment_id(), None);
        assert!(configurations.environments.is_empty());
    }

    #[tokio::test]
    async fn default_provider_uses_local_environment_when_url_is_missing() {
        let provider = DefaultEnvironmentProvider::new(/*exec_server_url*/ None);

        assert_local_default(provider.get_environments().await.expect("environments"));
    }

    #[tokio::test]
    async fn default_provider_uses_local_environment_when_url_is_empty() {
        let provider = DefaultEnvironmentProvider::new(Some(String::new()));

        assert_local_default(provider.get_environments().await.expect("environments"));
    }

    #[tokio::test]
    async fn default_provider_disables_default_environment_for_none_value() {
        let provider = DefaultEnvironmentProvider::new(Some("none".to_string()));

        assert_disabled(provider.get_environments().await.expect("environments"));
    }

    #[tokio::test]
    async fn default_provider_adds_remote_environment_for_websocket_url() {
        let provider = DefaultEnvironmentProvider::new(Some("ws://127.0.0.1:8765".to_string()));

        let configurations = provider.get_environments().await.expect("environments");
        assert_eq!(
            configurations.default_environment_id(),
            Some(REMOTE_ENVIRONMENT_ID)
        );
        let environment = configurations
            .environments
            .get(REMOTE_ENVIRONMENT_ID)
            .expect("remote configuration");
        assert_eq!(
            environment.static_exec_server_url(),
            Some("ws://127.0.0.1:8765")
        );
        assert_eq!(
            environment.resolve().await.expect("resolved environment"),
            ResolvedEnvironment {
                exec_server_url: "ws://127.0.0.1:8765".to_string(),
            }
        );
    }

    #[test]
    fn environment_configurations_rejects_local_provider_entry() {
        let err = EnvironmentConfigurations::new(
            Some(LOCAL_ENVIRONMENT_ID.to_string()),
            HashMap::from([(
                LOCAL_ENVIRONMENT_ID.to_string(),
                EnvironmentConfiguration::static_url("ws://127.0.0.1:8765".to_string())
                    .expect("static environment configuration"),
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
    fn static_environment_configuration_rejects_empty_exec_server_url() {
        let err =
            EnvironmentConfiguration::static_url(String::new()).expect_err("empty URL should fail");

        assert_eq!(
            err.to_string(),
            "exec-server protocol error: environment configuration `<static>` must resolve to a remote exec-server URL"
        );
    }

    #[test]
    fn static_environment_configuration_rejects_disabled_exec_server_url() {
        let err = EnvironmentConfiguration::static_url("none".to_string())
            .expect_err("disabled URL should fail");

        assert_eq!(
            err.to_string(),
            "exec-server protocol error: environment configuration `<static>` must resolve to a remote exec-server URL"
        );
    }

    #[tokio::test]
    async fn static_environment_configuration_normalizes_exec_server_url() {
        let configuration =
            EnvironmentConfiguration::static_url(" ws://127.0.0.1:8765 ".to_string())
                .expect("environment configurations");

        assert_eq!(
            configuration.static_exec_server_url(),
            Some("ws://127.0.0.1:8765")
        );
        assert_eq!(
            configuration.resolve().await.expect("resolved environment"),
            ResolvedEnvironment {
                exec_server_url: "ws://127.0.0.1:8765".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn environment_configuration_can_resolve_from_closure() {
        let configuration = EnvironmentConfiguration::from_resolver_fn(|| {
            async {
                Ok(ResolvedEnvironment {
                    exec_server_url: "ws://127.0.0.1:8765".to_string(),
                })
            }
            .boxed()
        });

        assert_eq!(configuration.static_exec_server_url(), None);
        assert_eq!(
            configuration.resolve().await.expect("resolved environment"),
            ResolvedEnvironment {
                exec_server_url: "ws://127.0.0.1:8765".to_string(),
            }
        );
    }
}
