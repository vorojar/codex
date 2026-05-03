use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::CommandExecParams;
use codex_app_server_protocol::CommandExecResponse;
use codex_app_server_protocol::RequestId;
use uuid::Uuid;

pub(crate) type WorkspaceCommandRunner = Arc<dyn WorkspaceCommandExecutor>;

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceCommand {
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) env: HashMap<String, Option<String>>,
    pub(crate) timeout: Duration,
    pub(crate) output_bytes_cap: usize,
}

impl WorkspaceCommand {
    pub(crate) fn new(argv: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            cwd: None,
            env: HashMap::new(),
            timeout: Duration::from_secs(5),
            output_bytes_cap: 64 * 1024,
        }
    }

    pub(crate) fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub(crate) fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), Some(value.into()));
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceCommandOutput {
    pub(crate) exit_code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl WorkspaceCommandOutput {
    pub(crate) fn success(&self) -> bool {
        self.exit_code == 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceCommandError {
    message: String,
}

impl WorkspaceCommandError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WorkspaceCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WorkspaceCommandError {}

/// Executes non-interactive workspace commands through the active TUI app-server session.
///
/// Implementations decide where the workspace lives. Callers provide argv/cwd/env and should not
/// branch on local versus remote execution.
pub(crate) trait WorkspaceCommandExecutor: Send + Sync {
    fn run(
        &self,
        command: WorkspaceCommand,
    ) -> Pin<
        Box<dyn Future<Output = Result<WorkspaceCommandOutput, WorkspaceCommandError>> + Send + '_>,
    >;
}

#[derive(Clone)]
pub(crate) struct AppServerWorkspaceCommandRunner {
    request_handle: AppServerRequestHandle,
}

impl AppServerWorkspaceCommandRunner {
    pub(crate) fn new(request_handle: AppServerRequestHandle) -> Self {
        Self { request_handle }
    }
}

impl WorkspaceCommandExecutor for AppServerWorkspaceCommandRunner {
    fn run(
        &self,
        command: WorkspaceCommand,
    ) -> Pin<
        Box<dyn Future<Output = Result<WorkspaceCommandOutput, WorkspaceCommandError>> + Send + '_>,
    > {
        Box::pin(async move {
            let timeout_ms = i64::try_from(command.timeout.as_millis()).unwrap_or(i64::MAX);
            let env = if command.env.is_empty() {
                None
            } else {
                Some(command.env)
            };
            let response: CommandExecResponse = self
                .request_handle
                .request_typed(ClientRequest::OneOffCommandExec {
                    request_id: RequestId::String(format!("workspace-command-{}", Uuid::new_v4())),
                    params: CommandExecParams {
                        command: command.argv,
                        process_id: None,
                        tty: false,
                        stream_stdin: false,
                        stream_stdout_stderr: false,
                        output_bytes_cap: Some(command.output_bytes_cap),
                        disable_output_cap: false,
                        disable_timeout: false,
                        timeout_ms: Some(timeout_ms),
                        cwd: command.cwd,
                        env,
                        size: None,
                        sandbox_policy: None,
                        permission_profile: None,
                    },
                })
                .await
                .map_err(|err| WorkspaceCommandError::new(err.to_string()))?;

            Ok(WorkspaceCommandOutput {
                exit_code: response.exit_code,
                stdout: response.stdout,
                stderr: response.stderr,
            })
        })
    }
}
