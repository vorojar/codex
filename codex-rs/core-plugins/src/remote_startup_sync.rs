use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::manager::PluginsManager;
use crate::manager::remote_plugin_service_config;
use crate::startup_sync::has_local_curated_plugins_snapshot;
use codex_login::AuthManager;
use tracing::info;
use tracing::warn;

const STARTUP_REMOTE_PLUGIN_SYNC_MARKER_FILE: &str = ".tmp/app-server-remote-plugin-sync-v1";
const STARTUP_REMOTE_PLUGIN_SYNC_PREREQUISITE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct RemoteStartupPluginSyncRequest {
    pub(crate) manager: Arc<PluginsManager>,
    pub(crate) codex_home: PathBuf,
    pub(crate) plugins_enabled: bool,
    pub(crate) remote_plugins_enabled: bool,
    pub(crate) chatgpt_base_url: String,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) on_effective_plugins_changed: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
}

pub(crate) fn start_startup_remote_plugin_sync_once(request: RemoteStartupPluginSyncRequest) {
    let RemoteStartupPluginSyncRequest {
        manager,
        codex_home,
        plugins_enabled,
        remote_plugins_enabled,
        chatgpt_base_url,
        auth_manager,
        on_effective_plugins_changed,
    } = request;
    if !plugins_enabled || !remote_plugins_enabled {
        return;
    }

    let marker_path = startup_remote_plugin_sync_marker_path(codex_home.as_path());
    if marker_path.is_file() {
        return;
    }

    tokio::spawn(async move {
        if marker_path.is_file() {
            return;
        }

        if !wait_for_startup_remote_plugin_sync_prerequisites(codex_home.as_path()).await {
            warn!(
                codex_home = %codex_home.display(),
                "skipping startup remote plugin sync because curated marketplace is not ready"
            );
            return;
        }

        let auth = auth_manager.auth().await;
        match manager
            .refresh_remote_installed_plugins_cache(
                &remote_plugin_service_config(&chatgpt_base_url),
                auth.as_ref(),
            )
            .await
        {
            Ok(cache_changed) => {
                info!(cache_changed, "completed startup remote plugin sync");
                if cache_changed
                    && let Some(on_effective_plugins_changed) = on_effective_plugins_changed
                {
                    on_effective_plugins_changed();
                }
                if let Err(err) =
                    write_startup_remote_plugin_sync_marker(codex_home.as_path()).await
                {
                    warn!(
                        error = %err,
                        path = %marker_path.display(),
                        "failed to persist startup remote plugin sync marker"
                    );
                }
            }
            Err(err) => {
                warn!(
                    error = %err,
                    "startup remote plugin sync failed; will retry on next app-server start"
                );
            }
        }
    });
}

fn startup_remote_plugin_sync_marker_path(codex_home: &Path) -> PathBuf {
    codex_home.join(STARTUP_REMOTE_PLUGIN_SYNC_MARKER_FILE)
}

async fn wait_for_startup_remote_plugin_sync_prerequisites(codex_home: &Path) -> bool {
    let deadline = tokio::time::Instant::now() + STARTUP_REMOTE_PLUGIN_SYNC_PREREQUISITE_TIMEOUT;
    loop {
        if has_local_curated_plugins_snapshot(codex_home) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn write_startup_remote_plugin_sync_marker(codex_home: &Path) -> std::io::Result<()> {
    let marker_path = startup_remote_plugin_sync_marker_path(codex_home);
    if let Some(parent) = marker_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(marker_path, b"ok\n").await
}

#[cfg(test)]
#[path = "remote_startup_sync_tests.rs"]
mod tests;
