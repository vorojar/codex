use std::collections::HashMap;

use anyhow::Result;
use codex_config::McpServerConfig;
use codex_config::McpServerTransportConfig;
use codex_config::types::OAuthCredentialsStoreMode;
use codex_exec_server::HttpClient;
use codex_exec_server::ReqwestHttpClient;
use codex_login::CodexAuth;
use codex_protocol::protocol::McpAuthStatus;
use codex_rmcp_client::OAuthProviderError;
use codex_rmcp_client::OauthLoginHandle;
use codex_rmcp_client::determine_streamable_http_auth_status;
use codex_rmcp_client::determine_streamable_http_auth_status_with_client;
use codex_rmcp_client::discover_streamable_http_oauth_with_client;
use codex_rmcp_client::perform_oauth_login_return_url_with_client;
use codex_rmcp_client::perform_oauth_login_silent_with_client;
use codex_rmcp_client::perform_oauth_login_with_client;
use futures::future::join_all;
use std::sync::Arc;
use tracing::warn;

use crate::runtime::McpRuntimeEnvironment;

use super::CODEX_APPS_MCP_SERVER_NAME;

#[derive(Debug, Clone)]
pub struct McpOAuthLoginConfig {
    pub url: String,
    pub http_headers: Option<HashMap<String, String>>,
    pub env_http_headers: Option<HashMap<String, String>>,
    pub discovered_scopes: Option<Vec<String>>,
}

#[derive(Debug)]
pub enum McpOAuthLoginSupport {
    Supported(McpOAuthLoginConfig),
    Unsupported,
    Unknown(anyhow::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpOAuthScopesSource {
    Explicit,
    Configured,
    Discovered,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMcpOAuthScopes {
    pub scopes: Vec<String>,
    pub source: McpOAuthScopesSource,
}

#[derive(Debug, Clone)]
pub struct McpAuthStatusEntry {
    pub config: McpServerConfig,
    pub auth_status: McpAuthStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpOAuthLoginOutcome {
    Completed,
    Unsupported,
}

pub async fn oauth_login_support(transport: &McpServerTransportConfig) -> McpOAuthLoginSupport {
    oauth_login_support_with_client(transport, Arc::new(ReqwestHttpClient)).await
}

pub async fn oauth_login_support_for_server(
    config: &McpServerConfig,
    runtime_environment: McpRuntimeEnvironment,
) -> McpOAuthLoginSupport {
    let http_client = match http_client_for_server(config, runtime_environment) {
        Ok(http_client) => http_client,
        Err(err) => return McpOAuthLoginSupport::Unknown(err),
    };
    oauth_login_support_with_client(&config.transport, http_client).await
}

async fn oauth_login_support_with_client(
    transport: &McpServerTransportConfig,
    http_client: Arc<dyn HttpClient>,
) -> McpOAuthLoginSupport {
    let McpServerTransportConfig::StreamableHttp {
        url,
        bearer_token_env_var,
        http_headers,
        env_http_headers,
    } = transport
    else {
        return McpOAuthLoginSupport::Unsupported;
    };

    if bearer_token_env_var.is_some() {
        return McpOAuthLoginSupport::Unsupported;
    }

    match discover_streamable_http_oauth_with_client(
        url,
        http_headers.clone(),
        env_http_headers.clone(),
        http_client,
    )
    .await
    {
        Ok(Some(discovery)) => McpOAuthLoginSupport::Supported(McpOAuthLoginConfig {
            url: url.clone(),
            http_headers: http_headers.clone(),
            env_http_headers: env_http_headers.clone(),
            discovered_scopes: discovery.scopes_supported,
        }),
        Ok(None) => McpOAuthLoginSupport::Unsupported,
        Err(err) => McpOAuthLoginSupport::Unknown(err),
    }
}

pub async fn discover_supported_scopes(
    transport: &McpServerTransportConfig,
) -> Option<Vec<String>> {
    discover_supported_scopes_with_client(transport, Arc::new(ReqwestHttpClient)).await
}

pub async fn discover_supported_scopes_for_server(
    config: &McpServerConfig,
    runtime_environment: McpRuntimeEnvironment,
) -> Option<Vec<String>> {
    match oauth_login_support_for_server(config, runtime_environment).await {
        McpOAuthLoginSupport::Supported(config) => config.discovered_scopes,
        McpOAuthLoginSupport::Unsupported | McpOAuthLoginSupport::Unknown(_) => None,
    }
}

async fn discover_supported_scopes_with_client(
    transport: &McpServerTransportConfig,
    http_client: Arc<dyn HttpClient>,
) -> Option<Vec<String>> {
    match oauth_login_support_with_client(transport, http_client).await {
        McpOAuthLoginSupport::Supported(config) => config.discovered_scopes,
        McpOAuthLoginSupport::Unsupported | McpOAuthLoginSupport::Unknown(_) => None,
    }
}

pub fn resolve_oauth_scopes(
    explicit_scopes: Option<Vec<String>>,
    configured_scopes: Option<Vec<String>>,
    discovered_scopes: Option<Vec<String>>,
) -> ResolvedMcpOAuthScopes {
    if let Some(scopes) = explicit_scopes {
        return ResolvedMcpOAuthScopes {
            scopes,
            source: McpOAuthScopesSource::Explicit,
        };
    }

    if let Some(scopes) = configured_scopes {
        return ResolvedMcpOAuthScopes {
            scopes,
            source: McpOAuthScopesSource::Configured,
        };
    }

    if let Some(scopes) = discovered_scopes
        && !scopes.is_empty()
    {
        return ResolvedMcpOAuthScopes {
            scopes,
            source: McpOAuthScopesSource::Discovered,
        };
    }

    ResolvedMcpOAuthScopes {
        scopes: Vec::new(),
        source: McpOAuthScopesSource::Empty,
    }
}

pub fn should_retry_without_scopes(scopes: &ResolvedMcpOAuthScopes, error: &anyhow::Error) -> bool {
    scopes.source == McpOAuthScopesSource::Discovered
        && error.downcast_ref::<OAuthProviderError>().is_some()
}

#[allow(clippy::too_many_arguments)]
pub async fn perform_oauth_login_return_url_for_server(
    server_name: &str,
    config: &McpServerConfig,
    store_mode: OAuthCredentialsStoreMode,
    explicit_scopes: Option<Vec<String>>,
    timeout_secs: Option<i64>,
    callback_port: Option<u16>,
    callback_url: Option<&str>,
    runtime_environment: McpRuntimeEnvironment,
) -> Result<OauthLoginHandle> {
    let McpServerTransportConfig::StreamableHttp {
        url,
        http_headers,
        env_http_headers,
        ..
    } = &config.transport
    else {
        anyhow::bail!("OAuth login is only supported for streamable HTTP servers.");
    };

    let http_client = http_client_for_server(config, runtime_environment)?;
    let discovered_scopes = if explicit_scopes.is_none() && config.scopes.is_none() {
        discover_supported_scopes_with_client(&config.transport, http_client.clone()).await
    } else {
        None
    };
    let resolved_scopes =
        resolve_oauth_scopes(explicit_scopes, config.scopes.clone(), discovered_scopes);

    perform_oauth_login_return_url_with_client(
        server_name,
        url,
        store_mode,
        http_headers.clone(),
        env_http_headers.clone(),
        &resolved_scopes.scopes,
        config.oauth_resource.as_deref(),
        timeout_secs,
        callback_port,
        callback_url,
        http_client,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn perform_oauth_login_silent_for_server(
    server_name: &str,
    config: &McpServerConfig,
    store_mode: OAuthCredentialsStoreMode,
    explicit_scopes: Option<Vec<String>>,
    callback_port: Option<u16>,
    callback_url: Option<&str>,
    runtime_environment: McpRuntimeEnvironment,
) -> Result<McpOAuthLoginOutcome> {
    let http_client = http_client_for_server(config, runtime_environment)?;
    let oauth_config =
        match oauth_login_support_with_client(&config.transport, http_client.clone()).await {
            McpOAuthLoginSupport::Supported(config) => config,
            McpOAuthLoginSupport::Unsupported => return Ok(McpOAuthLoginOutcome::Unsupported),
            McpOAuthLoginSupport::Unknown(err) => return Err(err),
        };

    let resolved_scopes = resolve_oauth_scopes(
        explicit_scopes,
        config.scopes.clone(),
        oauth_config.discovered_scopes.clone(),
    );

    let first_attempt = perform_oauth_login_silent_with_client(
        server_name,
        &oauth_config.url,
        store_mode,
        oauth_config.http_headers.clone(),
        oauth_config.env_http_headers.clone(),
        &resolved_scopes.scopes,
        config.oauth_resource.as_deref(),
        callback_port,
        callback_url,
        http_client.clone(),
    )
    .await;

    let final_result = match first_attempt {
        Err(err) if should_retry_without_scopes(&resolved_scopes, &err) => {
            perform_oauth_login_silent_with_client(
                server_name,
                &oauth_config.url,
                store_mode,
                oauth_config.http_headers,
                oauth_config.env_http_headers,
                &[],
                config.oauth_resource.as_deref(),
                callback_port,
                callback_url,
                http_client,
            )
            .await
        }
        result => result,
    };

    final_result.map(|()| McpOAuthLoginOutcome::Completed)
}

#[allow(clippy::too_many_arguments)]
pub async fn perform_oauth_login_for_server(
    server_name: &str,
    config: &McpServerConfig,
    store_mode: OAuthCredentialsStoreMode,
    explicit_scopes: Option<Vec<String>>,
    callback_port: Option<u16>,
    callback_url: Option<&str>,
    runtime_environment: McpRuntimeEnvironment,
) -> Result<McpOAuthLoginOutcome> {
    let http_client = http_client_for_server(config, runtime_environment)?;
    let oauth_config =
        match oauth_login_support_with_client(&config.transport, http_client.clone()).await {
            McpOAuthLoginSupport::Supported(config) => config,
            McpOAuthLoginSupport::Unsupported => return Ok(McpOAuthLoginOutcome::Unsupported),
            McpOAuthLoginSupport::Unknown(err) => return Err(err),
        };

    let resolved_scopes = resolve_oauth_scopes(
        explicit_scopes,
        config.scopes.clone(),
        oauth_config.discovered_scopes.clone(),
    );

    let first_attempt = perform_oauth_login_with_client(
        server_name,
        &oauth_config.url,
        store_mode,
        oauth_config.http_headers.clone(),
        oauth_config.env_http_headers.clone(),
        &resolved_scopes.scopes,
        config.oauth_resource.as_deref(),
        callback_port,
        callback_url,
        http_client.clone(),
    )
    .await;

    let final_result = match first_attempt {
        Err(err) if should_retry_without_scopes(&resolved_scopes, &err) => {
            perform_oauth_login_with_client(
                server_name,
                &oauth_config.url,
                store_mode,
                oauth_config.http_headers,
                oauth_config.env_http_headers,
                &[],
                config.oauth_resource.as_deref(),
                callback_port,
                callback_url,
                http_client,
            )
            .await
        }
        result => result,
    };

    final_result.map(|()| McpOAuthLoginOutcome::Completed)
}

pub async fn compute_auth_statuses<'a, I>(
    servers: I,
    store_mode: OAuthCredentialsStoreMode,
    auth: Option<&CodexAuth>,
    runtime_environment: McpRuntimeEnvironment,
) -> HashMap<String, McpAuthStatusEntry>
where
    I: IntoIterator<Item = (&'a String, &'a McpServerConfig)>,
{
    let futures = servers.into_iter().map(|(name, config)| {
        let name = name.clone();
        let config = config.clone();
        let runtime_environment = runtime_environment.clone();
        let has_runtime_auth = name == CODEX_APPS_MCP_SERVER_NAME
            && auth.is_some_and(CodexAuth::uses_codex_backend)
            && matches!(
                &config.transport,
                McpServerTransportConfig::StreamableHttp {
                    bearer_token_env_var: None,
                    ..
                }
            );
        async move {
            let auth_status = match compute_auth_status(
                &name,
                &config,
                store_mode,
                has_runtime_auth,
                runtime_environment,
            )
            .await
            {
                Ok(status) => status,
                Err(error) => {
                    warn!("failed to determine auth status for MCP server `{name}`: {error:?}");
                    McpAuthStatus::Unsupported
                }
            };
            let entry = McpAuthStatusEntry {
                config,
                auth_status,
            };
            (name, entry)
        }
    });

    join_all(futures).await.into_iter().collect()
}

async fn compute_auth_status(
    server_name: &str,
    config: &McpServerConfig,
    store_mode: OAuthCredentialsStoreMode,
    has_runtime_auth: bool,
    runtime_environment: McpRuntimeEnvironment,
) -> Result<McpAuthStatus> {
    if !config.enabled {
        return Ok(McpAuthStatus::Unsupported);
    }

    if has_runtime_auth {
        return Ok(McpAuthStatus::BearerToken);
    }

    match &config.transport {
        McpServerTransportConfig::Stdio { .. } => Ok(McpAuthStatus::Unsupported),
        McpServerTransportConfig::StreamableHttp {
            url,
            bearer_token_env_var,
            http_headers,
            env_http_headers,
        } => match config.experimental_environment.as_deref() {
            Some("remote") => {
                let http_client = http_client_for_server(config, runtime_environment)?;
                determine_streamable_http_auth_status_with_client(
                    server_name,
                    url,
                    bearer_token_env_var.as_deref(),
                    http_headers.clone(),
                    env_http_headers.clone(),
                    store_mode,
                    http_client,
                )
                .await
            }
            None | Some("local") => {
                determine_streamable_http_auth_status(
                    server_name,
                    url,
                    bearer_token_env_var.as_deref(),
                    http_headers.clone(),
                    env_http_headers.clone(),
                    store_mode,
                )
                .await
            }
            Some(environment) => {
                anyhow::bail!("unsupported experimental_environment `{environment}`")
            }
        },
    }
}

pub fn http_client_for_server(
    config: &McpServerConfig,
    runtime_environment: McpRuntimeEnvironment,
) -> Result<Arc<dyn HttpClient>> {
    match config.experimental_environment.as_deref() {
        None | Some("local") => Ok(Arc::new(ReqwestHttpClient)),
        Some("remote") => {
            let environment = runtime_environment.environment();
            if !environment.is_remote() {
                anyhow::bail!("remote MCP server requires a remote environment");
            }
            Ok(environment.get_http_client())
        }
        Some(environment) => anyhow::bail!("unsupported experimental_environment `{environment}`"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::anyhow;
    use codex_config::McpServerConfig;
    use codex_config::McpServerTransportConfig;
    use codex_exec_server::Environment;
    use pretty_assertions::assert_eq;

    use crate::runtime::McpRuntimeEnvironment;

    use super::McpOAuthScopesSource;
    use super::OAuthProviderError;
    use super::ResolvedMcpOAuthScopes;
    use super::http_client_for_server;
    use super::resolve_oauth_scopes;
    use super::should_retry_without_scopes;

    #[test]
    fn resolve_oauth_scopes_prefers_explicit() {
        let resolved = resolve_oauth_scopes(
            Some(vec!["explicit".to_string()]),
            Some(vec!["configured".to_string()]),
            Some(vec!["discovered".to_string()]),
        );

        assert_eq!(
            resolved,
            ResolvedMcpOAuthScopes {
                scopes: vec!["explicit".to_string()],
                source: McpOAuthScopesSource::Explicit,
            }
        );
    }

    #[test]
    fn resolve_oauth_scopes_prefers_configured_over_discovered() {
        let resolved = resolve_oauth_scopes(
            /*explicit_scopes*/ None,
            Some(vec!["configured".to_string()]),
            Some(vec!["discovered".to_string()]),
        );

        assert_eq!(
            resolved,
            ResolvedMcpOAuthScopes {
                scopes: vec!["configured".to_string()],
                source: McpOAuthScopesSource::Configured,
            }
        );
    }

    #[test]
    fn resolve_oauth_scopes_uses_discovered_when_needed() {
        let resolved = resolve_oauth_scopes(
            /*explicit_scopes*/ None,
            /*configured_scopes*/ None,
            Some(vec!["discovered".to_string()]),
        );

        assert_eq!(
            resolved,
            ResolvedMcpOAuthScopes {
                scopes: vec!["discovered".to_string()],
                source: McpOAuthScopesSource::Discovered,
            }
        );
    }

    #[test]
    fn resolve_oauth_scopes_preserves_explicitly_empty_configured_scopes() {
        let resolved = resolve_oauth_scopes(
            /*explicit_scopes*/ None,
            Some(Vec::new()),
            Some(vec!["ignored".into()]),
        );

        assert_eq!(
            resolved,
            ResolvedMcpOAuthScopes {
                scopes: Vec::new(),
                source: McpOAuthScopesSource::Configured,
            }
        );
    }

    #[test]
    fn resolve_oauth_scopes_falls_back_to_empty() {
        let resolved = resolve_oauth_scopes(
            /*explicit_scopes*/ None, /*configured_scopes*/ None,
            /*discovered_scopes*/ None,
        );

        assert_eq!(
            resolved,
            ResolvedMcpOAuthScopes {
                scopes: Vec::new(),
                source: McpOAuthScopesSource::Empty,
            }
        );
    }

    #[test]
    fn should_retry_without_scopes_only_for_discovered_provider_errors() {
        let discovered = ResolvedMcpOAuthScopes {
            scopes: vec!["scope".to_string()],
            source: McpOAuthScopesSource::Discovered,
        };
        let provider_error = anyhow!(OAuthProviderError::new(
            Some("invalid_scope".to_string()),
            Some("scope rejected".to_string()),
        ));

        assert!(should_retry_without_scopes(&discovered, &provider_error));

        let configured = ResolvedMcpOAuthScopes {
            scopes: vec!["scope".to_string()],
            source: McpOAuthScopesSource::Configured,
        };
        assert!(!should_retry_without_scopes(&configured, &provider_error));
        assert!(!should_retry_without_scopes(
            &discovered,
            &anyhow!("timed out waiting for OAuth callback"),
        ));
    }

    #[test]
    fn local_server_uses_local_http_client_even_with_remote_runtime() {
        let config = streamable_config(/*experimental_environment*/ None);
        let runtime_environment = remote_runtime_environment();

        assert!(http_client_for_server(&config, runtime_environment).is_ok());
    }

    #[test]
    fn remote_server_uses_runtime_http_client_when_runtime_is_remote() {
        let config = streamable_config(Some("remote"));
        let runtime_environment = remote_runtime_environment();

        assert!(http_client_for_server(&config, runtime_environment).is_ok());
    }

    #[tokio::test]
    async fn remote_server_without_remote_runtime_returns_clear_error() {
        let config = streamable_config(Some("remote"));
        let runtime_environment = McpRuntimeEnvironment::new(
            Arc::new(Environment::default_for_tests()),
            PathBuf::from("/tmp"),
        );

        let Err(error) = http_client_for_server(&config, runtime_environment) else {
            panic!("remote server should require remote runtime");
        };
        assert_eq!(
            error.to_string(),
            "remote MCP server requires a remote environment"
        );
    }

    fn streamable_config(experimental_environment: Option<&str>) -> McpServerConfig {
        McpServerConfig {
            transport: McpServerTransportConfig::StreamableHttp {
                url: "http://mcp.example.test/mcp".to_string(),
                bearer_token_env_var: None,
                http_headers: None,
                env_http_headers: None,
            },
            experimental_environment: experimental_environment.map(str::to_string),
            enabled: true,
            required: false,
            supports_parallel_tool_calls: false,
            disabled_reason: None,
            startup_timeout_sec: Some(Duration::from_secs(30)),
            tool_timeout_sec: None,
            default_tools_approval_mode: None,
            enabled_tools: None,
            disabled_tools: None,
            scopes: None,
            oauth_resource: None,
            tools: HashMap::new(),
        }
    }

    fn remote_runtime_environment() -> McpRuntimeEnvironment {
        McpRuntimeEnvironment::new(
            Arc::new(
                Environment::create_for_tests(Some("ws://127.0.0.1:65535".to_string()))
                    .expect("create remote environment"),
            ),
            PathBuf::from("/tmp"),
        )
    }
}
