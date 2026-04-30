mod discoverable;
pub mod installed_marketplaces;
pub mod loader;
mod manager;
pub mod manifest;
pub mod marketplace;
pub mod marketplace_add;
pub mod marketplace_remove;
pub mod marketplace_upgrade;
pub mod remote;
pub mod remote_bundle;
pub mod remote_legacy;
mod remote_startup_sync;
pub mod startup_sync;
pub mod store;
pub mod toggles;

#[cfg(test)]
mod manager_tests;

use codex_config::types::McpServerConfig;

pub const OPENAI_CURATED_MARKETPLACE_NAME: &str = "openai-curated";
pub const OPENAI_BUNDLED_MARKETPLACE_NAME: &str = "openai-bundled";

pub const TOOL_SUGGEST_DISCOVERABLE_PLUGIN_ALLOWLIST: &[&str] = &[
    "github@openai-curated",
    "notion@openai-curated",
    "slack@openai-curated",
    "gmail@openai-curated",
    "google-calendar@openai-curated",
    "google-drive@openai-curated",
    "teams@openai-curated",
    "sharepoint@openai-curated",
    "outlook-email@openai-curated",
    "outlook-calendar@openai-curated",
    "linear@openai-curated",
    "figma@openai-curated",
    "computer-use@openai-bundled",
];

pub use codex_plugin::AppConnectorId;
pub use codex_plugin::PluginId;
pub use discoverable::list_tool_suggest_discoverable_plugins;
pub use manager::ConfiguredMarketplace;
pub use manager::ConfiguredMarketplaceListOutcome;
pub use manager::ConfiguredMarketplacePlugin;
pub use manager::PluginDetail;
pub use manager::PluginDetailsUnavailableReason;
pub use manager::PluginFeatureFlags;
pub use manager::PluginInstallError;
pub use manager::PluginInstallOutcome;
pub use manager::PluginInstallRequest;
pub use manager::PluginReadOutcome;
pub use manager::PluginReadRequest;
pub use manager::PluginRemoteSyncError;
pub use manager::PluginUninstallError;
pub use manager::PluginsManager;
pub use manager::RemotePluginSyncResult;

#[cfg(test)]
pub(crate) type LoadedPlugin = codex_plugin::LoadedPlugin<McpServerConfig>;
pub type PluginLoadOutcome = codex_plugin::PluginLoadOutcome<McpServerConfig>;
