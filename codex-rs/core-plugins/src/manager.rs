use crate::OPENAI_CURATED_MARKETPLACE_NAME;
use crate::PluginLoadOutcome;
use crate::installed_marketplaces::installed_marketplace_roots_from_layer_stack;
use crate::loader::configured_curated_plugin_ids_from_codex_home;
use crate::loader::curated_plugin_cache_version;
use crate::loader::installed_plugin_telemetry_metadata;
use crate::loader::load_plugin_apps;
use crate::loader::load_plugin_mcp_servers;
use crate::loader::load_plugin_skills;
use crate::loader::load_plugins_from_layer_stack;
use crate::loader::log_plugin_load_errors;
use crate::loader::materialize_marketplace_plugin_source;
use crate::loader::plugin_telemetry_metadata_from_root;
use crate::loader::refresh_curated_plugin_cache;
use crate::loader::refresh_non_curated_plugin_cache;
use crate::loader::refresh_non_curated_plugin_cache_force_reinstall;
use crate::loader::remote_installed_plugins_to_config;
use crate::manifest::PluginManifestInterface;
use crate::manifest::load_plugin_manifest;
use crate::marketplace::MarketplaceError;
use crate::marketplace::MarketplaceInterface;
use crate::marketplace::MarketplaceListError;
use crate::marketplace::MarketplacePluginAuthPolicy;
use crate::marketplace::MarketplacePluginPolicy;
use crate::marketplace::MarketplacePluginSource;
use crate::marketplace::ResolvedMarketplacePlugin;
use crate::marketplace::find_installable_marketplace_plugin;
use crate::marketplace::find_marketplace_plugin;
use crate::marketplace::list_marketplaces;
use crate::marketplace::plugin_interface_with_marketplace_category;
use crate::marketplace_upgrade::ConfiguredMarketplaceUpgradeError;
use crate::marketplace_upgrade::ConfiguredMarketplaceUpgradeOutcome;
use crate::marketplace_upgrade::configured_git_marketplace_names;
use crate::marketplace_upgrade::upgrade_configured_git_marketplaces;
use crate::remote::RemoteInstalledPlugin;
use crate::remote::RemotePluginCatalogError;
use crate::remote::RemotePluginServiceConfig;
use crate::remote_legacy::RemotePluginFetchError;
use crate::remote_legacy::RemotePluginMutationError;
use crate::remote_startup_sync::RemoteStartupPluginSyncRequest;
use crate::remote_startup_sync::start_startup_remote_plugin_sync_once;
use crate::startup_sync::curated_plugins_repo_path;
use crate::startup_sync::read_curated_plugins_sha;
use crate::startup_sync::sync_openai_plugins_repo;
use crate::store::PluginInstallResult as StorePluginInstallResult;
use crate::store::PluginStore;
use crate::store::PluginStoreError;
use codex_analytics::AnalyticsEventsClient;
use codex_config::ConfigLayerStack;
use codex_config::types::PluginConfig;
use codex_config::version_for_toml;
use codex_core_skills::SkillMetadata;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_plugin::AppConnectorId;
use codex_plugin::PluginCapabilitySummary;
use codex_plugin::PluginId;
use codex_plugin::PluginIdError;
use codex_plugin::prompt_safe_plugin_description;
use codex_protocol::protocol::Product;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tracing::warn;

static CURATED_REPO_SYNC_STARTED: AtomicBool = AtomicBool::new(false);
const FEATURED_PLUGIN_IDS_CACHE_TTL: std::time::Duration =
    std::time::Duration::from_secs(60 * 60 * 3);

#[derive(Clone, PartialEq, Eq)]
struct FeaturedPluginIdsCacheKey {
    chatgpt_base_url: String,
    account_id: Option<String>,
    chatgpt_user_id: Option<String>,
    is_workspace_account: bool,
}

#[derive(Clone)]
struct CachedFeaturedPluginIds {
    key: FeaturedPluginIdsCacheKey,
    expires_at: Instant,
    featured_plugin_ids: Vec<String>,
}

struct RemoteInstalledPluginsCacheRefreshRequest {
    service_config: RemotePluginServiceConfig,
    auth: Option<CodexAuth>,
    notify: RemoteInstalledPluginsCacheRefreshNotify,
    // App-server attaches side effects such as skills metadata invalidation and MCP refreshes when
    // remote installed state changes.
    on_effective_plugins_changed: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
}

#[derive(Clone, Copy)]
enum RemoteInstalledPluginsCacheRefreshNotify {
    IfCacheChanged,
    // Remote mutations may change local bundles or active MCP state even when the installed set is
    // unchanged. Notify after `/installed` succeeds so MCP refreshes are ordered after the remote
    // installed cache.
    AfterSuccessfulRefresh,
}

#[derive(Default)]
struct RemoteInstalledPluginsCacheRefreshState {
    requested: Option<RemoteInstalledPluginsCacheRefreshRequest>,
    in_flight: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct NonCuratedCacheRefreshRequest {
    roots: Vec<AbsolutePathBuf>,
    mode: NonCuratedCacheRefreshMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NonCuratedCacheRefreshMode {
    IfVersionChanged,
    ForceReinstall,
}

#[derive(Default)]
struct NonCuratedCacheRefreshState {
    requested: Option<NonCuratedCacheRefreshRequest>,
    last_refreshed: Option<NonCuratedCacheRefreshRequest>,
    in_flight: bool,
}

#[derive(Default)]
struct ConfiguredMarketplaceUpgradeState {
    in_flight: bool,
}

pub(crate) fn remote_plugin_service_config(chatgpt_base_url: &str) -> RemotePluginServiceConfig {
    RemotePluginServiceConfig {
        chatgpt_base_url: chatgpt_base_url.to_string(),
    }
}

fn featured_plugin_ids_cache_key(
    chatgpt_base_url: &str,
    auth: Option<&CodexAuth>,
) -> FeaturedPluginIdsCacheKey {
    FeaturedPluginIdsCacheKey {
        chatgpt_base_url: chatgpt_base_url.to_string(),
        account_id: auth.and_then(CodexAuth::get_account_id),
        chatgpt_user_id: auth.and_then(CodexAuth::get_chatgpt_user_id),
        is_workspace_account: auth.is_some_and(CodexAuth::is_workspace_account),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstallRequest {
    pub plugin_name: String,
    pub marketplace_path: AbsolutePathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginReadRequest {
    pub plugin_name: String,
    pub marketplace_path: AbsolutePathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstallOutcome {
    pub plugin_id: PluginId,
    pub plugin_version: String,
    pub installed_path: AbsolutePathBuf,
    pub auth_policy: MarketplacePluginAuthPolicy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PluginReadOutcome {
    pub marketplace_name: String,
    pub marketplace_path: Option<AbsolutePathBuf>,
    pub plugin: PluginDetail,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PluginDetail {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub source: MarketplacePluginSource,
    pub policy: MarketplacePluginPolicy,
    pub interface: Option<PluginManifestInterface>,
    pub installed: bool,
    pub enabled: bool,
    pub skills: Vec<SkillMetadata>,
    pub disabled_skill_paths: HashSet<AbsolutePathBuf>,
    pub apps: Vec<AppConnectorId>,
    pub mcp_server_names: Vec<String>,
    pub details_unavailable_reason: Option<PluginDetailsUnavailableReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginDetailsUnavailableReason {
    InstallRequiredForRemoteSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredMarketplace {
    pub name: String,
    pub path: AbsolutePathBuf,
    pub interface: Option<MarketplaceInterface>,
    pub plugins: Vec<ConfiguredMarketplacePlugin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredMarketplacePlugin {
    pub id: String,
    pub name: String,
    pub source: MarketplacePluginSource,
    pub policy: MarketplacePluginPolicy,
    pub interface: Option<PluginManifestInterface>,
    pub installed: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfiguredMarketplaceListOutcome {
    pub marketplaces: Vec<ConfiguredMarketplace>,
    pub errors: Vec<MarketplaceListError>,
}

impl From<PluginDetail> for PluginCapabilitySummary {
    fn from(value: PluginDetail) -> Self {
        let has_skills = value.skills.iter().any(|skill| {
            !value
                .disabled_skill_paths
                .contains(&skill.path_to_skills_md)
        });
        Self {
            config_name: value.id,
            display_name: value.name,
            description: prompt_safe_plugin_description(value.description.as_deref()),
            has_skills,
            mcp_server_names: value.mcp_server_names,
            app_connector_ids: value.apps,
        }
    }
}

pub struct PluginsManager {
    codex_home: PathBuf,
    store: PluginStore,
    featured_plugin_ids_cache: RwLock<Option<CachedFeaturedPluginIds>>,
    configured_marketplace_upgrade_state: RwLock<ConfiguredMarketplaceUpgradeState>,
    non_curated_cache_refresh_state: RwLock<NonCuratedCacheRefreshState>,
    cached_enabled_outcome: RwLock<Option<CachedPluginLoadOutcome>>,
    // TODO(remote plugins): reset this cache when ChatGPT auth/account state changes so stale
    // remote installed state cannot remain effective for a different account.
    remote_installed_plugins_cache: RwLock<Option<Vec<RemoteInstalledPlugin>>>,
    remote_installed_plugins_cache_refresh_state: RwLock<RemoteInstalledPluginsCacheRefreshState>,
    restriction_product: Option<Product>,
    analytics_events_client: RwLock<Option<AnalyticsEventsClient>>,
}

#[derive(Clone)]
struct CachedPluginLoadOutcome {
    config_version: String,
    plugin_hooks_enabled: bool,
    outcome: PluginLoadOutcome,
}

impl PluginsManager {
    pub fn new(codex_home: PathBuf) -> Self {
        Self::new_with_restriction_product(codex_home, Some(Product::Codex))
    }

    pub fn new_with_restriction_product(
        codex_home: PathBuf,
        restriction_product: Option<Product>,
    ) -> Self {
        // Product restrictions are enforced at marketplace admission time for a given CODEX_HOME:
        // listing, install, and curated refresh all consult this restriction context before new
        // plugins enter local config or cache. After admission, runtime plugin loading trusts the
        // contents of that CODEX_HOME and does not re-filter configured plugins by product, so
        // already-admitted plugins may continue exposing MCP servers/tools from shared local state.
        //
        // This assumes a single CODEX_HOME is only used by one product.
        Self {
            codex_home: codex_home.clone(),
            store: PluginStore::new(codex_home),
            featured_plugin_ids_cache: RwLock::new(None),
            configured_marketplace_upgrade_state: RwLock::new(
                ConfiguredMarketplaceUpgradeState::default(),
            ),
            non_curated_cache_refresh_state: RwLock::new(NonCuratedCacheRefreshState::default()),
            cached_enabled_outcome: RwLock::new(None),
            remote_installed_plugins_cache: RwLock::new(None),
            remote_installed_plugins_cache_refresh_state: RwLock::new(
                RemoteInstalledPluginsCacheRefreshState::default(),
            ),
            restriction_product,
            analytics_events_client: RwLock::new(None),
        }
    }

    pub fn set_analytics_events_client(&self, analytics_events_client: AnalyticsEventsClient) {
        let mut stored_client = match self.analytics_events_client.write() {
            Ok(client_guard) => client_guard,
            Err(err) => err.into_inner(),
        };
        *stored_client = Some(analytics_events_client);
    }

    fn restriction_product_matches(&self, products: Option<&[Product]>) -> bool {
        match products {
            None => true,
            Some([]) => false,
            Some(products) => self
                .restriction_product
                .is_some_and(|product| product.matches_product_restriction(products)),
        }
    }

    pub async fn plugins_for_config(
        &self,
        config_layer_stack: &ConfigLayerStack,
        plugins_enabled: bool,
        remote_plugins_enabled: bool,
        plugin_hooks_enabled: bool,
    ) -> PluginLoadOutcome {
        self.plugins_for_config_with_force_reload(
            config_layer_stack,
            plugins_enabled,
            remote_plugins_enabled,
            plugin_hooks_enabled,
            /*force_reload*/ false,
        )
        .await
    }

    pub(crate) async fn plugins_for_config_with_force_reload(
        &self,
        config_layer_stack: &ConfigLayerStack,
        plugins_enabled: bool,
        remote_plugins_enabled: bool,
        plugin_hooks_enabled: bool,
        force_reload: bool,
    ) -> PluginLoadOutcome {
        if !plugins_enabled {
            return PluginLoadOutcome::default();
        }

        let config_version = version_for_toml(&config_layer_stack.effective_config());
        if !force_reload
            && let Some(outcome) =
                self.cached_enabled_outcome(&config_version, plugin_hooks_enabled)
        {
            return outcome;
        }

        let outcome = load_plugins_from_layer_stack(
            config_layer_stack,
            self.remote_installed_plugin_configs(remote_plugins_enabled),
            &self.store,
            self.restriction_product,
            plugin_hooks_enabled,
        )
        .await;
        log_plugin_load_errors(&outcome);
        let mut cache = match self.cached_enabled_outcome.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        *cache = Some(CachedPluginLoadOutcome {
            config_version,
            plugin_hooks_enabled,
            outcome: outcome.clone(),
        });
        outcome
    }

    pub fn clear_cache(&self) {
        self.clear_enabled_outcome_cache();
        let mut featured_plugin_ids_cache = match self.featured_plugin_ids_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        *featured_plugin_ids_cache = None;
    }

    fn clear_enabled_outcome_cache(&self) {
        let mut cached_enabled_outcome = match self.cached_enabled_outcome.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        *cached_enabled_outcome = None;
    }

    /// Load plugins for a config layer stack without touching the plugins cache.
    pub async fn plugins_for_layer_stack(
        &self,
        config_layer_stack: &ConfigLayerStack,
        plugins_enabled: bool,
        remote_plugins_enabled: bool,
        plugin_hooks_feature_enabled: bool,
    ) -> PluginLoadOutcome {
        if !plugins_enabled {
            return PluginLoadOutcome::default();
        }
        load_plugins_from_layer_stack(
            config_layer_stack,
            self.remote_installed_plugin_configs(remote_plugins_enabled),
            &self.store,
            self.restriction_product,
            plugin_hooks_feature_enabled,
        )
        .await
    }

    /// Resolve plugin skill roots for a config layer stack without touching the plugins cache.
    pub async fn effective_skill_roots_for_layer_stack(
        &self,
        config_layer_stack: &ConfigLayerStack,
        plugins_enabled: bool,
        remote_plugins_enabled: bool,
        plugin_hooks_enabled: bool,
    ) -> Vec<AbsolutePathBuf> {
        self.plugins_for_layer_stack(
            config_layer_stack,
            plugins_enabled,
            remote_plugins_enabled,
            plugin_hooks_enabled,
        )
        .await
        .effective_skill_roots()
    }

    fn cached_enabled_outcome(
        &self,
        config_version: &str,
        plugin_hooks_enabled: bool,
    ) -> Option<PluginLoadOutcome> {
        match self.cached_enabled_outcome.read() {
            Ok(cache) => cache
                .as_ref()
                .filter(|cached| {
                    cached.config_version == config_version
                        && cached.plugin_hooks_enabled == plugin_hooks_enabled
                })
                .map(|cached| cached.outcome.clone()),
            Err(err) => err
                .into_inner()
                .as_ref()
                .filter(|cached| {
                    cached.config_version == config_version
                        && cached.plugin_hooks_enabled == plugin_hooks_enabled
                })
                .map(|cached| cached.outcome.clone()),
        }
    }

    fn remote_installed_plugin_configs(
        &self,
        remote_plugins_enabled: bool,
    ) -> HashMap<String, PluginConfig> {
        if !remote_plugins_enabled {
            return HashMap::new();
        }

        let cache = match self.remote_installed_plugins_cache.read() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        let Some(plugins) = cache.as_ref() else {
            return HashMap::new();
        };

        remote_installed_plugins_to_config(plugins, &self.store)
    }

    #[doc(hidden)]
    pub fn write_remote_installed_plugins_cache(
        &self,
        plugins: Vec<RemoteInstalledPlugin>,
    ) -> bool {
        let mut cache = match self.remote_installed_plugins_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        if cache.as_ref().is_some_and(|cache| cache.eq(&plugins)) {
            return false;
        }
        *cache = Some(plugins);
        drop(cache);
        self.clear_enabled_outcome_cache();
        true
    }

    pub fn clear_remote_installed_plugins_cache(&self) -> bool {
        let mut cache = match self.remote_installed_plugins_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        if cache.is_none() {
            return false;
        }
        *cache = None;
        drop(cache);
        self.clear_enabled_outcome_cache();
        true
    }

    pub fn maybe_start_remote_installed_plugins_cache_refresh(
        self: &Arc<Self>,
        plugins_enabled: bool,
        remote_plugins_enabled: bool,
        chatgpt_base_url: String,
        auth: Option<CodexAuth>,
        on_effective_plugins_changed: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    ) {
        self.maybe_start_remote_installed_plugins_cache_refresh_with_notify(
            plugins_enabled,
            remote_plugins_enabled,
            chatgpt_base_url,
            auth,
            RemoteInstalledPluginsCacheRefreshNotify::IfCacheChanged,
            on_effective_plugins_changed,
        );
    }

    pub fn maybe_start_remote_installed_plugins_cache_refresh_after_mutation(
        self: &Arc<Self>,
        plugins_enabled: bool,
        remote_plugins_enabled: bool,
        chatgpt_base_url: String,
        auth: Option<CodexAuth>,
        on_effective_plugins_changed: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    ) {
        self.maybe_start_remote_installed_plugins_cache_refresh_with_notify(
            plugins_enabled,
            remote_plugins_enabled,
            chatgpt_base_url,
            auth,
            RemoteInstalledPluginsCacheRefreshNotify::AfterSuccessfulRefresh,
            on_effective_plugins_changed,
        );
    }

    fn maybe_start_remote_installed_plugins_cache_refresh_with_notify(
        self: &Arc<Self>,
        plugins_enabled: bool,
        remote_plugins_enabled: bool,
        chatgpt_base_url: String,
        auth: Option<CodexAuth>,
        notify: RemoteInstalledPluginsCacheRefreshNotify,
        on_effective_plugins_changed: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    ) {
        if !plugins_enabled || !remote_plugins_enabled {
            return;
        }

        self.schedule_remote_installed_plugins_cache_refresh(
            RemoteInstalledPluginsCacheRefreshRequest {
                service_config: remote_plugin_service_config(&chatgpt_base_url),
                auth,
                notify,
                on_effective_plugins_changed,
            },
        );
    }

    pub fn maybe_start_plugin_list_background_tasks(
        self: &Arc<Self>,
        plugins_enabled: bool,
        remote_plugins_enabled: bool,
        chatgpt_base_url: String,
        auth: Option<CodexAuth>,
        roots: &[AbsolutePathBuf],
        on_effective_plugins_changed: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    ) {
        self.maybe_start_non_curated_plugin_cache_refresh(roots);
        self.maybe_start_remote_installed_plugins_cache_refresh(
            plugins_enabled,
            remote_plugins_enabled,
            chatgpt_base_url,
            auth,
            on_effective_plugins_changed,
        );
    }

    fn cached_featured_plugin_ids(
        &self,
        cache_key: &FeaturedPluginIdsCacheKey,
    ) -> Option<Vec<String>> {
        {
            let cache = match self.featured_plugin_ids_cache.read() {
                Ok(cache) => cache,
                Err(err) => err.into_inner(),
            };
            let now = Instant::now();
            if let Some(cached) = cache.as_ref()
                && now < cached.expires_at
                && cached.key == *cache_key
            {
                return Some(cached.featured_plugin_ids.clone());
            }
        }

        let mut cache = match self.featured_plugin_ids_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        let now = Instant::now();
        if cache
            .as_ref()
            .is_some_and(|cached| now >= cached.expires_at || cached.key != *cache_key)
        {
            *cache = None;
        }
        None
    }

    fn write_featured_plugin_ids_cache(
        &self,
        cache_key: FeaturedPluginIdsCacheKey,
        featured_plugin_ids: &[String],
    ) {
        let mut cache = match self.featured_plugin_ids_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        *cache = Some(CachedFeaturedPluginIds {
            key: cache_key,
            expires_at: Instant::now() + FEATURED_PLUGIN_IDS_CACHE_TTL,
            featured_plugin_ids: featured_plugin_ids.to_vec(),
        });
    }

    pub async fn featured_plugin_ids(
        &self,
        plugins_enabled: bool,
        chatgpt_base_url: &str,
        auth: Option<&CodexAuth>,
    ) -> Result<Vec<String>, RemotePluginFetchError> {
        if !plugins_enabled {
            return Ok(Vec::new());
        }

        let cache_key = featured_plugin_ids_cache_key(chatgpt_base_url, auth);
        if let Some(featured_plugin_ids) = self.cached_featured_plugin_ids(&cache_key) {
            return Ok(featured_plugin_ids);
        }
        let featured_plugin_ids = crate::remote_legacy::fetch_remote_featured_plugin_ids(
            &remote_plugin_service_config(chatgpt_base_url),
            auth,
            self.restriction_product,
        )
        .await?;
        self.write_featured_plugin_ids_cache(cache_key, &featured_plugin_ids);
        Ok(featured_plugin_ids)
    }

    pub async fn install_plugin(
        &self,
        request: PluginInstallRequest,
    ) -> Result<PluginInstallOutcome, PluginInstallError> {
        let resolved = find_installable_marketplace_plugin(
            &request.marketplace_path,
            &request.plugin_name,
            self.restriction_product,
        )?;
        self.install_resolved_plugin(resolved).await
    }

    pub async fn install_plugin_with_remote_sync(
        &self,
        chatgpt_base_url: &str,
        auth: Option<&CodexAuth>,
        request: PluginInstallRequest,
    ) -> Result<PluginInstallOutcome, PluginInstallError> {
        let resolved = find_installable_marketplace_plugin(
            &request.marketplace_path,
            &request.plugin_name,
            self.restriction_product,
        )?;
        let plugin_id = resolved.plugin_id.as_key();
        // This only forwards the backend mutation before the local install flow.
        crate::remote_legacy::enable_remote_plugin(
            &remote_plugin_service_config(chatgpt_base_url),
            auth,
            &plugin_id,
        )
        .await
        .map_err(PluginInstallError::from)?;
        self.install_resolved_plugin(resolved).await
    }

    async fn install_resolved_plugin(
        &self,
        resolved: ResolvedMarketplacePlugin,
    ) -> Result<PluginInstallOutcome, PluginInstallError> {
        let auth_policy = resolved.policy.authentication;
        let plugin_version =
            if resolved.plugin_id.marketplace_name == OPENAI_CURATED_MARKETPLACE_NAME {
                let curated_plugin_version = read_curated_plugins_sha(self.codex_home.as_path())
                    .ok_or_else(|| {
                        PluginStoreError::Invalid(
                            "local curated marketplace sha is not available".to_string(),
                        )
                    })?;
                Some(curated_plugin_cache_version(&curated_plugin_version))
            } else {
                None
            };
        let store = self.store.clone();
        let codex_home = self.codex_home.clone();
        let result: StorePluginInstallResult = tokio::task::spawn_blocking(move || {
            let materialized =
                materialize_marketplace_plugin_source(codex_home.as_path(), &resolved.source)
                    .map_err(PluginStoreError::Invalid)?;
            let source_path = materialized.path;
            if let Some(plugin_version) = plugin_version {
                store.install_with_version(source_path, resolved.plugin_id, plugin_version)
            } else {
                store.install(source_path, resolved.plugin_id)
            }
        })
        .await
        .map_err(PluginInstallError::join)??;

        let analytics_events_client = match self.analytics_events_client.read() {
            Ok(client) => client.clone(),
            Err(err) => err.into_inner().clone(),
        };
        if let Some(analytics_events_client) = analytics_events_client {
            analytics_events_client.track_plugin_installed(
                plugin_telemetry_metadata_from_root(&result.plugin_id, &result.installed_path)
                    .await,
            );
        }

        Ok(PluginInstallOutcome {
            plugin_id: result.plugin_id,
            plugin_version: result.plugin_version,
            installed_path: result.installed_path,
            auth_policy,
        })
    }

    pub async fn uninstall_plugin(&self, plugin_id: String) -> Result<(), PluginUninstallError> {
        let plugin_id = PluginId::parse(&plugin_id)?;
        self.uninstall_plugin_id(plugin_id).await
    }

    pub async fn uninstall_plugin_with_remote_sync(
        &self,
        chatgpt_base_url: &str,
        auth: Option<&CodexAuth>,
        plugin_id: String,
    ) -> Result<(), PluginUninstallError> {
        // TODO: Remove this legacy remote-sync path once remote plugins have
        // their own manager and installed-state API.
        let plugin_id = PluginId::parse(&plugin_id)?;
        let plugin_key = plugin_id.as_key();
        // This only forwards the backend mutation before the local uninstall flow.
        crate::remote_legacy::uninstall_remote_plugin(
            &remote_plugin_service_config(chatgpt_base_url),
            auth,
            &plugin_key,
        )
        .await
        .map_err(PluginUninstallError::from)?;
        self.uninstall_plugin_id(plugin_id).await
    }

    async fn uninstall_plugin_id(&self, plugin_id: PluginId) -> Result<(), PluginUninstallError> {
        let plugin_telemetry = if self.store.active_plugin_root(&plugin_id).is_some() {
            Some(installed_plugin_telemetry_metadata(self.codex_home.as_path(), &plugin_id).await)
        } else {
            None
        };
        let store = self.store.clone();
        let plugin_id_for_store = plugin_id.clone();
        tokio::task::spawn_blocking(move || store.uninstall(&plugin_id_for_store))
            .await
            .map_err(PluginUninstallError::join)??;

        let analytics_events_client = match self.analytics_events_client.read() {
            Ok(client) => client.clone(),
            Err(err) => err.into_inner().clone(),
        };
        if let Some(plugin_telemetry) = plugin_telemetry
            && let Some(analytics_events_client) = analytics_events_client
        {
            analytics_events_client.track_plugin_uninstalled(plugin_telemetry);
        }

        Ok(())
    }

    pub fn list_marketplaces_for_config(
        &self,
        config_layer_stack: &ConfigLayerStack,
        plugins_enabled: bool,
        additional_roots: &[AbsolutePathBuf],
    ) -> Result<ConfiguredMarketplaceListOutcome, MarketplaceError> {
        if !plugins_enabled {
            return Ok(ConfiguredMarketplaceListOutcome::default());
        }

        let (installed_plugins, enabled_plugins) =
            self.configured_plugin_states(config_layer_stack);
        let marketplace_outcome =
            list_marketplaces(&self.marketplace_roots(config_layer_stack, additional_roots))?;
        let mut seen_plugin_keys = HashSet::new();
        let marketplaces = marketplace_outcome
            .marketplaces
            .into_iter()
            .filter_map(|marketplace| {
                let marketplace_name = marketplace.name.clone();
                let plugins = marketplace
                    .plugins
                    .into_iter()
                    .filter_map(|plugin| {
                        let plugin_key = format!("{}@{marketplace_name}", plugin.name);
                        if !seen_plugin_keys.insert(plugin_key.clone()) {
                            return None;
                        }
                        if !self.restriction_product_matches(plugin.policy.products.as_deref()) {
                            return None;
                        }

                        Some(ConfiguredMarketplacePlugin {
                            // Enabled state is keyed by `<plugin>@<marketplace>`, so duplicate
                            // plugin entries from duplicate marketplace files intentionally
                            // resolve to the first discovered source.
                            id: plugin_key.clone(),
                            installed: installed_plugins.contains(&plugin_key),
                            enabled: enabled_plugins.contains(&plugin_key),
                            name: plugin.name,
                            source: plugin.source,
                            policy: plugin.policy,
                            interface: plugin.interface,
                        })
                    })
                    .collect::<Vec<_>>();

                (!plugins.is_empty()).then_some(ConfiguredMarketplace {
                    name: marketplace.name,
                    path: marketplace.path,
                    interface: marketplace.interface,
                    plugins,
                })
            })
            .collect();

        Ok(ConfiguredMarketplaceListOutcome {
            marketplaces,
            errors: marketplace_outcome.errors,
        })
    }

    pub async fn read_plugin_for_config(
        &self,
        config_layer_stack: &ConfigLayerStack,
        plugins_enabled: bool,
        request: &PluginReadRequest,
    ) -> Result<PluginReadOutcome, MarketplaceError> {
        if !plugins_enabled {
            return Err(MarketplaceError::PluginsDisabled);
        }

        let plugin = find_marketplace_plugin(&request.marketplace_path, &request.plugin_name)?;
        if !self.restriction_product_matches(plugin.policy.products.as_deref()) {
            return Err(MarketplaceError::PluginNotFound {
                plugin_name: plugin.plugin_id.plugin_name,
                marketplace_name: plugin.plugin_id.marketplace_name,
            });
        }

        let marketplace_name = plugin.plugin_id.marketplace_name.clone();
        let plugin_key = plugin.plugin_id.as_key();
        let (installed_plugins, enabled_plugins) =
            self.configured_plugin_states(config_layer_stack);
        let plugin = self
            .read_plugin_detail_for_marketplace_plugin(
                config_layer_stack,
                &marketplace_name,
                ConfiguredMarketplacePlugin {
                    id: plugin_key.clone(),
                    name: plugin.plugin_id.plugin_name,
                    source: plugin.source,
                    policy: plugin.policy,
                    interface: plugin.interface,
                    installed: installed_plugins.contains(&plugin_key),
                    enabled: enabled_plugins.contains(&plugin_key),
                },
            )
            .await?;

        Ok(PluginReadOutcome {
            marketplace_name,
            marketplace_path: Some(request.marketplace_path.clone()),
            plugin,
        })
    }

    pub async fn read_plugin_detail_for_marketplace_plugin(
        &self,
        config_layer_stack: &ConfigLayerStack,
        marketplace_name: &str,
        plugin: ConfiguredMarketplacePlugin,
    ) -> Result<PluginDetail, MarketplaceError> {
        if !self.restriction_product_matches(plugin.policy.products.as_deref()) {
            return Err(MarketplaceError::PluginNotFound {
                plugin_name: plugin.name,
                marketplace_name: marketplace_name.to_string(),
            });
        }

        let plugin_id =
            PluginId::new(plugin.name.clone(), marketplace_name.to_string()).map_err(|err| {
                match err {
                    PluginIdError::Invalid(message) => MarketplaceError::InvalidPlugin(message),
                }
            })?;
        let plugin_key = plugin_id.as_key();
        if matches!(plugin.source, MarketplacePluginSource::Git { .. }) && !plugin.installed {
            let description = remote_plugin_install_required_description(&plugin.source);
            return Ok(PluginDetail {
                id: plugin_key,
                name: plugin.name,
                description: Some(description),
                source: plugin.source,
                policy: plugin.policy,
                interface: plugin.interface,
                installed: plugin.installed,
                enabled: plugin.enabled,
                skills: Vec::new(),
                disabled_skill_paths: HashSet::new(),
                apps: Vec::new(),
                mcp_server_names: Vec::new(),
                details_unavailable_reason: Some(
                    PluginDetailsUnavailableReason::InstallRequiredForRemoteSource,
                ),
            });
        }

        let source_path =
            if matches!(plugin.source, MarketplacePluginSource::Git { .. }) && plugin.installed {
                self.store.active_plugin_root(&plugin_id).ok_or_else(|| {
                    MarketplaceError::InvalidPlugin(format!(
                        "installed plugin cache entry is missing for {plugin_key}"
                    ))
                })?
            } else {
                let codex_home = self.codex_home.clone();
                let source = plugin.source.clone();
                let materialized = tokio::task::spawn_blocking(move || {
                    materialize_marketplace_plugin_source(codex_home.as_path(), &source)
                })
                .await
                .map_err(|err| {
                    MarketplaceError::InvalidPlugin(format!(
                        "failed to materialize plugin source: {err}"
                    ))
                })?
                .map_err(MarketplaceError::InvalidPlugin)?;
                materialized.path.clone()
            };
        if !source_path.as_path().is_dir() {
            return Err(MarketplaceError::InvalidPlugin(
                "path does not exist or is not a directory".to_string(),
            ));
        }
        let manifest = load_plugin_manifest(source_path.as_path()).ok_or_else(|| {
            MarketplaceError::InvalidPlugin("missing or invalid plugin.json".to_string())
        })?;
        let description = manifest.description.clone();
        let marketplace_category = plugin
            .interface
            .as_ref()
            .and_then(|interface| interface.category.clone());
        let interface = plugin_interface_with_marketplace_category(
            manifest.interface.clone(),
            marketplace_category,
        );
        let resolved_skills = load_plugin_skills(
            &source_path,
            &manifest.paths,
            self.restriction_product,
            &codex_core_skills::config_rules::skill_config_rules_from_stack(config_layer_stack),
        )
        .await;
        let apps = load_plugin_apps(source_path.as_path()).await;
        let mut mcp_server_names = load_plugin_mcp_servers(source_path.as_path())
            .await
            .into_keys()
            .collect::<Vec<_>>();
        mcp_server_names.sort_unstable();
        mcp_server_names.dedup();

        Ok(PluginDetail {
            id: plugin.id,
            name: plugin.name,
            description,
            source: plugin.source,
            policy: plugin.policy,
            interface,
            installed: plugin.installed,
            enabled: plugin.enabled,
            skills: resolved_skills.skills,
            disabled_skill_paths: resolved_skills.disabled_skill_paths,
            apps,
            mcp_server_names,
            details_unavailable_reason: None,
        })
    }

    pub fn maybe_start_plugin_startup_tasks(
        self: &Arc<Self>,
        config_layer_stack: ConfigLayerStack,
        plugins_enabled: bool,
        remote_plugins_enabled: bool,
        chatgpt_base_url: String,
        auth_manager: Arc<AuthManager>,
        on_effective_plugins_changed: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    ) {
        if plugins_enabled {
            self.start_curated_repo_sync();
            let should_spawn_marketplace_auto_upgrade = {
                let mut state = match self.configured_marketplace_upgrade_state.write() {
                    Ok(state) => state,
                    Err(err) => err.into_inner(),
                };
                if state.in_flight {
                    false
                } else {
                    state.in_flight = true;
                    true
                }
            };
            if should_spawn_marketplace_auto_upgrade {
                let manager = Arc::clone(self);
                let config_layer_stack_for_upgrade = config_layer_stack;
                if let Err(err) = std::thread::Builder::new()
                    .name("plugins-marketplace-auto-upgrade".to_string())
                    .spawn(move || {
                        let outcome = manager.upgrade_configured_marketplaces(
                            &config_layer_stack_for_upgrade,
                            /*marketplace_name*/ None,
                        );
                        match outcome {
                            Ok(outcome) => {
                                for error in outcome.errors {
                                    warn!(
                                        marketplace = error.marketplace_name,
                                        error = %error.message,
                                        "failed to auto-upgrade configured marketplace"
                                    );
                                }
                            }
                            Err(err) => {
                                warn!("failed to auto-upgrade configured marketplaces: {err}");
                            }
                        }

                        let mut state = match manager.configured_marketplace_upgrade_state.write() {
                            Ok(state) => state,
                            Err(err) => err.into_inner(),
                        };
                        state.in_flight = false;
                    })
                {
                    let mut state = match self.configured_marketplace_upgrade_state.write() {
                        Ok(state) => state,
                        Err(err) => err.into_inner(),
                    };
                    state.in_flight = false;
                    warn!("failed to start configured marketplace auto-upgrade task: {err}");
                }
            }

            if remote_plugins_enabled {
                start_startup_remote_plugin_sync_once(RemoteStartupPluginSyncRequest {
                    manager: Arc::clone(self),
                    codex_home: self.codex_home.clone(),
                    plugins_enabled,
                    remote_plugins_enabled,
                    chatgpt_base_url: chatgpt_base_url.clone(),
                    auth_manager: auth_manager.clone(),
                    on_effective_plugins_changed: on_effective_plugins_changed.clone(),
                });
            }

            let manager = Arc::clone(self);
            tokio::spawn(async move {
                let auth = auth_manager.auth().await;
                if let Err(err) = manager
                    .featured_plugin_ids(plugins_enabled, &chatgpt_base_url, auth.as_ref())
                    .await
                {
                    warn!(
                        error = %err,
                        "failed to warm featured plugin ids cache"
                    );
                }
            });
        }
    }

    pub fn upgrade_configured_marketplaces(
        &self,
        config_layer_stack: &ConfigLayerStack,
        marketplace_name: Option<&str>,
    ) -> Result<ConfiguredMarketplaceUpgradeOutcome, String> {
        if let Some(marketplace_name) = marketplace_name
            && !configured_git_marketplace_names(config_layer_stack)
                .iter()
                .any(|name| name == marketplace_name)
        {
            return Err(format!(
                "marketplace `{marketplace_name}` is not configured as a Git marketplace"
            ));
        }

        let mut outcome = upgrade_configured_git_marketplaces(
            self.codex_home.as_path(),
            config_layer_stack,
            marketplace_name,
        );
        if !outcome.upgraded_roots.is_empty() {
            match refresh_non_curated_plugin_cache_force_reinstall(
                self.codex_home.as_path(),
                &outcome.upgraded_roots,
            ) {
                Ok(cache_refreshed) => {
                    if cache_refreshed {
                        self.clear_cache();
                    }
                }
                Err(err) => {
                    self.clear_cache();
                    outcome.errors.push(ConfiguredMarketplaceUpgradeError {
                        marketplace_name: marketplace_name
                            .unwrap_or("all configured marketplaces")
                            .to_string(),
                        message: format!(
                            "failed to refresh installed plugin cache after marketplace upgrade: {err}"
                        ),
                    });
                }
            }
        }
        Ok(outcome)
    }

    pub fn maybe_start_non_curated_plugin_cache_refresh(
        self: &Arc<Self>,
        roots: &[AbsolutePathBuf],
    ) {
        self.schedule_non_curated_plugin_cache_refresh(
            roots,
            NonCuratedCacheRefreshMode::IfVersionChanged,
        );
    }

    fn schedule_remote_installed_plugins_cache_refresh(
        self: &Arc<Self>,
        mut request: RemoteInstalledPluginsCacheRefreshRequest,
    ) {
        let should_spawn = {
            let mut state = match self.remote_installed_plugins_cache_refresh_state.write() {
                Ok(state) => state,
                Err(err) => err.into_inner(),
            };
            if let Some(existing_request) = state.requested.as_ref() {
                if matches!(
                    existing_request.notify,
                    RemoteInstalledPluginsCacheRefreshNotify::AfterSuccessfulRefresh
                ) {
                    request.notify =
                        RemoteInstalledPluginsCacheRefreshNotify::AfterSuccessfulRefresh;
                }
                if request.on_effective_plugins_changed.is_none() {
                    request.on_effective_plugins_changed =
                        existing_request.on_effective_plugins_changed.clone();
                }
            }
            state.requested = Some(request);
            if state.in_flight {
                false
            } else {
                state.in_flight = true;
                true
            }
        };
        if !should_spawn {
            return;
        }

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            manager
                .run_remote_installed_plugins_cache_refresh_loop()
                .await;
        });
    }

    fn schedule_non_curated_plugin_cache_refresh(
        self: &Arc<Self>,
        roots: &[AbsolutePathBuf],
        mode: NonCuratedCacheRefreshMode,
    ) {
        let mut roots = roots.to_vec();
        roots.sort_unstable();
        roots.dedup();
        if roots.is_empty() {
            return;
        }
        let request = NonCuratedCacheRefreshRequest { roots, mode };

        let should_spawn = {
            let mut state = match self.non_curated_cache_refresh_state.write() {
                Ok(state) => state,
                Err(err) => err.into_inner(),
            };
            // Collapse repeated plugin/list requests onto one worker and only queue another pass
            // when the requested roots set actually changes. Forced reinstall requests are not
            // deduped against the last completed pass because the same marketplace root path can
            // point at newly activated files after an auto-upgrade.
            if state.requested.as_ref() == Some(&request)
                || (mode == NonCuratedCacheRefreshMode::IfVersionChanged
                    && !state.in_flight
                    && state.last_refreshed.as_ref() == Some(&request))
            {
                return;
            }
            if mode == NonCuratedCacheRefreshMode::IfVersionChanged
                && state.requested.as_ref().is_some_and(|requested| {
                    requested.mode == NonCuratedCacheRefreshMode::ForceReinstall
                        && requested.roots == request.roots
                })
            {
                return;
            }
            state.requested = Some(request);
            if state.in_flight {
                false
            } else {
                state.in_flight = true;
                true
            }
        };
        if !should_spawn {
            return;
        }

        let manager = Arc::clone(self);
        if let Err(err) = std::thread::Builder::new()
            .name("plugins-non-curated-cache-refresh".to_string())
            .spawn(move || manager.run_non_curated_plugin_cache_refresh_loop())
        {
            let mut state = match self.non_curated_cache_refresh_state.write() {
                Ok(state) => state,
                Err(err) => err.into_inner(),
            };
            state.in_flight = false;
            state.requested = None;
            warn!("failed to start non-curated plugin cache refresh task: {err}");
        }
    }

    fn start_curated_repo_sync(self: &Arc<Self>) {
        if CURATED_REPO_SYNC_STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        let manager = Arc::clone(self);
        let codex_home = self.codex_home.clone();
        if let Err(err) = std::thread::Builder::new()
            .name("plugins-curated-repo-sync".to_string())
            .spawn(
                move || match sync_openai_plugins_repo(codex_home.as_path()) {
                    Ok(curated_plugin_version) => {
                        let configured_curated_plugin_ids =
                            configured_curated_plugin_ids_from_codex_home(codex_home.as_path());
                        match refresh_curated_plugin_cache(
                            codex_home.as_path(),
                            &curated_plugin_version,
                            &configured_curated_plugin_ids,
                        ) {
                            Ok(cache_refreshed) => {
                                if cache_refreshed {
                                    manager.clear_cache();
                                }
                            }
                            Err(err) => {
                                manager.clear_cache();
                                CURATED_REPO_SYNC_STARTED.store(false, Ordering::SeqCst);
                                warn!("failed to refresh curated plugin cache after sync: {err}");
                            }
                        }
                    }
                    Err(err) => {
                        CURATED_REPO_SYNC_STARTED.store(false, Ordering::SeqCst);
                        warn!("failed to sync curated plugins repo: {err}");
                    }
                },
            )
        {
            CURATED_REPO_SYNC_STARTED.store(false, Ordering::SeqCst);
            warn!("failed to start curated plugins repo sync task: {err}");
        }
    }

    async fn run_remote_installed_plugins_cache_refresh_loop(self: Arc<Self>) {
        loop {
            let request = {
                let mut state = match self.remote_installed_plugins_cache_refresh_state.write() {
                    Ok(state) => state,
                    Err(err) => err.into_inner(),
                };
                match state.requested.take() {
                    Some(request) => request,
                    None => {
                        state.in_flight = false;
                        return;
                    }
                }
            };

            match self
                .refresh_remote_installed_plugins_cache(
                    &request.service_config,
                    request.auth.as_ref(),
                )
                .await
            {
                Ok(changed) => {
                    let should_notify = changed
                        || matches!(
                            request.notify,
                            RemoteInstalledPluginsCacheRefreshNotify::AfterSuccessfulRefresh
                        );
                    if should_notify
                        && let Some(on_effective_plugins_changed) =
                            request.on_effective_plugins_changed
                    {
                        on_effective_plugins_changed();
                    }
                }
                Err(
                    RemotePluginCatalogError::AuthRequired
                    | RemotePluginCatalogError::UnsupportedAuthMode,
                ) => {
                    let changed = self.clear_remote_installed_plugins_cache();
                    if changed
                        && let Some(on_effective_plugins_changed) =
                            request.on_effective_plugins_changed
                    {
                        on_effective_plugins_changed();
                    }
                }
                Err(err) => {
                    warn!(
                        error = %err,
                        "failed to refresh remote installed plugins cache"
                    );
                }
            }
        }
    }

    pub(crate) async fn refresh_remote_installed_plugins_cache(
        &self,
        service_config: &RemotePluginServiceConfig,
        auth: Option<&CodexAuth>,
    ) -> Result<bool, RemotePluginCatalogError> {
        let installed_plugins =
            crate::remote::fetch_remote_installed_plugins(service_config, auth).await?;
        // TODO(remote plugins): reconcile missing or stale local bundles before
        // publishing remote installed state as effective local plugin config.
        Ok(self.write_remote_installed_plugins_cache(installed_plugins))
    }

    fn run_non_curated_plugin_cache_refresh_loop(self: Arc<Self>) {
        loop {
            let request = {
                let state = match self.non_curated_cache_refresh_state.read() {
                    Ok(state) => state,
                    Err(err) => err.into_inner(),
                };
                state.requested.clone()
            };

            let Some(request) = request else {
                let mut state = match self.non_curated_cache_refresh_state.write() {
                    Ok(state) => state,
                    Err(err) => err.into_inner(),
                };
                state.in_flight = false;
                return;
            };

            let refresh_result = match request.mode {
                NonCuratedCacheRefreshMode::IfVersionChanged => {
                    refresh_non_curated_plugin_cache(self.codex_home.as_path(), &request.roots)
                }
                NonCuratedCacheRefreshMode::ForceReinstall => {
                    refresh_non_curated_plugin_cache_force_reinstall(
                        self.codex_home.as_path(),
                        &request.roots,
                    )
                }
            };
            let refreshed = match refresh_result {
                Ok(cache_refreshed) => {
                    if cache_refreshed {
                        self.clear_cache();
                    }
                    true
                }
                Err(err) => {
                    self.clear_cache();
                    warn!("failed to refresh non-curated plugin cache: {err}");
                    false
                }
            };

            let mut state = match self.non_curated_cache_refresh_state.write() {
                Ok(state) => state,
                Err(err) => err.into_inner(),
            };
            if refreshed {
                state.last_refreshed = Some(request.clone());
            }
            if state.requested.as_ref() == Some(&request) {
                state.requested = None;
                state.in_flight = false;
                return;
            }
        }
    }

    fn configured_plugin_states(
        &self,
        config_layer_stack: &ConfigLayerStack,
    ) -> (HashSet<String>, HashSet<String>) {
        let configured_plugins = configured_plugins_from_stack(config_layer_stack);
        let installed_plugins = configured_plugins
            .keys()
            .filter(|plugin_key| {
                PluginId::parse(plugin_key)
                    .ok()
                    .is_some_and(|plugin_id| self.store.is_installed(&plugin_id))
            })
            .cloned()
            .collect::<HashSet<_>>();
        let enabled_plugins = configured_plugins
            .into_iter()
            .filter_map(|(plugin_key, plugin)| plugin.enabled.then_some(plugin_key))
            .collect::<HashSet<_>>();
        (installed_plugins, enabled_plugins)
    }

    fn marketplace_roots(
        &self,
        config_layer_stack: &ConfigLayerStack,
        additional_roots: &[AbsolutePathBuf],
    ) -> Vec<AbsolutePathBuf> {
        // Treat the curated catalog as an extra marketplace root so plugin listing can surface it
        // without requiring every caller to know where it is stored.
        let mut roots = additional_roots.to_vec();
        roots.extend(installed_marketplace_roots_from_layer_stack(
            config_layer_stack,
            self.codex_home.as_path(),
        ));
        let curated_repo_root = curated_plugins_repo_path(self.codex_home.as_path());
        if curated_repo_root.is_dir()
            && let Ok(curated_repo_root) = AbsolutePathBuf::try_from(curated_repo_root)
        {
            roots.push(curated_repo_root);
        }
        roots.sort_unstable();
        roots.dedup();
        roots
    }
}

fn remote_plugin_install_required_description(source: &MarketplacePluginSource) -> String {
    let source_description = match source {
        MarketplacePluginSource::Git {
            url,
            path,
            ref_name,
            sha,
        } => {
            let mut parts = vec![url.clone()];
            if let Some(path) = path {
                parts.push(format!("path `{path}`"));
            }
            if let Some(ref_name) = ref_name {
                parts.push(format!("ref `{ref_name}`"));
            }
            if let Some(sha) = sha {
                parts.push(format!("sha `{sha}`"));
            }
            parts.join(", ")
        }
        MarketplacePluginSource::Local { path } => path.as_path().display().to_string(),
    };

    format!(
        "This is a cross-repo plugin. Install it to view more detailed information. The source of the plugin is {source_description}."
    )
}

#[derive(Debug, thiserror::Error)]
pub enum PluginInstallError {
    #[error("{0}")]
    Marketplace(#[from] MarketplaceError),

    #[error("{0}")]
    Remote(#[from] RemotePluginMutationError),

    #[error("{0}")]
    Store(#[from] PluginStoreError),

    #[error("failed to join plugin install task: {0}")]
    Join(#[from] tokio::task::JoinError),
}

impl PluginInstallError {
    fn join(source: tokio::task::JoinError) -> Self {
        Self::Join(source)
    }

    pub fn is_invalid_request(&self) -> bool {
        matches!(
            self,
            Self::Marketplace(
                MarketplaceError::MarketplaceNotFound { .. }
                    | MarketplaceError::InvalidMarketplaceFile { .. }
                    | MarketplaceError::PluginNotFound { .. }
                    | MarketplaceError::PluginNotAvailable { .. }
                    | MarketplaceError::InvalidPlugin(_)
            ) | Self::Store(PluginStoreError::Invalid(_))
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PluginUninstallError {
    #[error("{0}")]
    InvalidPluginId(#[from] PluginIdError),

    #[error("{0}")]
    Remote(#[from] RemotePluginMutationError),

    #[error("{0}")]
    Store(#[from] PluginStoreError),

    #[error("failed to join plugin uninstall task: {0}")]
    Join(#[from] tokio::task::JoinError),
}

impl PluginUninstallError {
    fn join(source: tokio::task::JoinError) -> Self {
        Self::Join(source)
    }

    pub fn is_invalid_request(&self) -> bool {
        matches!(self, Self::InvalidPluginId(_))
    }
}

pub(crate) fn configured_plugins_from_stack(
    config_layer_stack: &ConfigLayerStack,
) -> HashMap<String, PluginConfig> {
    // Plugin entries remain persisted user config only.
    let Some(user_layer) = config_layer_stack.get_user_layer() else {
        return HashMap::new();
    };
    configured_plugins_from_user_config_value(&user_layer.config)
}

fn configured_plugins_from_user_config_value(
    user_config: &toml::Value,
) -> HashMap<String, PluginConfig> {
    let Some(plugins_value) = user_config.get("plugins") else {
        return HashMap::new();
    };
    match plugins_value.clone().try_into() {
        Ok(plugins) => plugins,
        Err(err) => {
            warn!("invalid plugins config: {err}");
            HashMap::new()
        }
    }
}
