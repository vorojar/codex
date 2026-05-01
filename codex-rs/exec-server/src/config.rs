use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

pub const EXEC_SERVER_CONFIG_FILE: &str = "exec-server.toml";
pub const DEFAULT_GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecServerRunOptions {
    pub graceful_shutdown_timeout: Duration,
}

impl Default for ExecServerRunOptions {
    fn default() -> Self {
        Self {
            graceful_shutdown_timeout: DEFAULT_GRACEFUL_SHUTDOWN_TIMEOUT,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExecServerConfig {
    pub graceful_shutdown_timeout_ms: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecServerConfigError {
    #[error("failed to read exec-server config `{path}`: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse exec-server config `{path}`: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error(
        "invalid exec-server config `{path}`: graceful_shutdown_timeout_ms must be greater than 0"
    )]
    InvalidTimeout { path: String },
}

impl ExecServerConfig {
    pub async fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ExecServerConfigError> {
        let path = path.as_ref();
        let contents = match tokio::fs::read_to_string(path).await {
            Ok(contents) => contents,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(ExecServerConfigError::Read {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        toml::from_str(&contents).map_err(|source| ExecServerConfigError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    pub fn into_run_options(
        self,
        path: impl AsRef<Path>,
    ) -> Result<ExecServerRunOptions, ExecServerConfigError> {
        let Some(timeout_ms) = self.graceful_shutdown_timeout_ms else {
            return Ok(ExecServerRunOptions::default());
        };
        if timeout_ms == 0 {
            return Err(ExecServerConfigError::InvalidTimeout {
                path: path.as_ref().display().to_string(),
            });
        }
        Ok(ExecServerRunOptions {
            graceful_shutdown_timeout: Duration::from_millis(timeout_ms),
        })
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn missing_config_uses_defaults() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join(EXEC_SERVER_CONFIG_FILE);

        let config = ExecServerConfig::load_from_path(&path)
            .await
            .expect("missing config should load");
        let options = config
            .into_run_options(&path)
            .expect("default options should validate");

        assert_eq!(options, ExecServerRunOptions::default());
    }

    #[tokio::test]
    async fn parses_graceful_shutdown_timeout() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join(EXEC_SERVER_CONFIG_FILE);
        tokio::fs::write(&path, "graceful_shutdown_timeout_ms = 125\n")
            .await
            .expect("write config");

        let config = ExecServerConfig::load_from_path(&path)
            .await
            .expect("config should load");
        let options = config
            .into_run_options(&path)
            .expect("config should validate");

        assert_eq!(
            options,
            ExecServerRunOptions {
                graceful_shutdown_timeout: Duration::from_millis(125),
            }
        );
    }

    #[tokio::test]
    async fn malformed_config_reports_path() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join(EXEC_SERVER_CONFIG_FILE);
        tokio::fs::write(&path, "graceful_shutdown_timeout_ms = ")
            .await
            .expect("write config");

        let err = ExecServerConfig::load_from_path(&path)
            .await
            .expect_err("malformed config should fail");

        assert!(
            err.to_string().contains(path.to_string_lossy().as_ref()),
            "error should mention path: {err}"
        );
    }

    #[tokio::test]
    async fn zero_timeout_is_invalid() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join(EXEC_SERVER_CONFIG_FILE);

        let err = ExecServerConfig {
            graceful_shutdown_timeout_ms: Some(0),
        }
        .into_run_options(&path)
        .expect_err("zero timeout should fail");

        assert_eq!(
            err.to_string(),
            format!(
                "invalid exec-server config `{}`: graceful_shutdown_timeout_ms must be greater than 0",
                path.display()
            )
        );
    }
}
