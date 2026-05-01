mod drain;
mod file_system_handler;
mod handler;
mod process_handler;
mod processor;
mod registry;
mod session_registry;
mod transport;

pub(crate) use handler::ExecServerHandler;
pub use transport::DEFAULT_LISTEN_URL;
pub use transport::ExecServerListenUrlParseError;

use crate::ExecServerRunOptions;
use crate::ExecServerRuntimePaths;

pub async fn run_main(
    listen_url: &str,
    runtime_paths: ExecServerRuntimePaths,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_main_with_options(listen_url, runtime_paths, ExecServerRunOptions::default()).await
}

pub async fn run_main_with_options(
    listen_url: &str,
    runtime_paths: ExecServerRuntimePaths,
    options: ExecServerRunOptions,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    transport::run_transport(listen_url, runtime_paths, options).await
}
