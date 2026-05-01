use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;
use tokio::runtime::Handle;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tracing::debug;
use tracing::warn;

use crate::ExecServerClient;
use crate::ExecServerError;
use crate::client_api::ExecServerTransport;
use crate::client_api::RemoteExecServerConnectArgs;
use crate::client_api::StdioExecServerConnectArgs;
use crate::connection::JsonRpcConnection;

const ENVIRONMENT_CLIENT_NAME: &str = "codex-environment";
const ENVIRONMENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ENVIRONMENT_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(5);

impl ExecServerTransport {
    pub(crate) async fn connect_for_environment(self) -> Result<ExecServerClient, ExecServerError> {
        match self {
            ExecServerTransport::WebSocketUrl(websocket_url) => {
                ExecServerClient::connect_websocket(RemoteExecServerConnectArgs {
                    websocket_url,
                    client_name: ENVIRONMENT_CLIENT_NAME.to_string(),
                    connect_timeout: ENVIRONMENT_CONNECT_TIMEOUT,
                    initialize_timeout: ENVIRONMENT_INITIALIZE_TIMEOUT,
                    resume_session_id: None,
                })
                .await
            }
            ExecServerTransport::StdioShellCommand(shell_command) => {
                ExecServerClient::connect_stdio_command(StdioExecServerConnectArgs {
                    shell_command,
                    client_name: ENVIRONMENT_CLIENT_NAME.to_string(),
                    initialize_timeout: ENVIRONMENT_INITIALIZE_TIMEOUT,
                    resume_session_id: None,
                })
                .await
            }
        }
    }
}

impl ExecServerClient {
    pub async fn connect_websocket(
        args: RemoteExecServerConnectArgs,
    ) -> Result<Self, ExecServerError> {
        let websocket_url = args.websocket_url.clone();
        let connect_timeout = args.connect_timeout;
        let (stream, _) = timeout(connect_timeout, connect_async(websocket_url.as_str()))
            .await
            .map_err(|_| ExecServerError::WebSocketConnectTimeout {
                url: websocket_url.clone(),
                timeout: connect_timeout,
            })?
            .map_err(|source| ExecServerError::WebSocketConnect {
                url: websocket_url.clone(),
                source,
            })?;

        Self::connect(
            JsonRpcConnection::from_websocket(
                stream,
                format!("exec-server websocket {websocket_url}"),
            ),
            args.into(),
        )
        .await
    }

    pub async fn connect_stdio_command(
        args: StdioExecServerConnectArgs,
    ) -> Result<Self, ExecServerError> {
        let shell_command = args.shell_command.clone();
        let mut child = shell_command_process(&shell_command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(ExecServerError::Spawn)?;

        let stdin = child.stdin.take().ok_or_else(|| {
            ExecServerError::Protocol("spawned exec-server command has no stdin".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ExecServerError::Protocol("spawned exec-server command has no stdout".to_string())
        })?;
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => debug!("exec-server stdio stderr: {line}"),
                        Ok(None) => break,
                        Err(err) => {
                            warn!("failed to read exec-server stdio stderr: {err}");
                            break;
                        }
                    }
                }
            });
        }

        Self::connect(
            JsonRpcConnection::from_stdio(
                stdout,
                stdin,
                format!("exec-server stdio command `{shell_command}`"),
            )
            .with_lifetime_guard(Box::new(StdioChildGuard { child: Some(child) })),
            args.into(),
        )
        .await
    }
}

struct StdioChildGuard {
    child: Option<Child>,
}

impl Drop for StdioChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.child.take() else {
            return;
        };

        match Handle::try_current() {
            Ok(handle) => {
                let _terminate_task = handle.spawn(terminate_stdio_child(child));
            }
            Err(_) => {
                terminate_stdio_child_now(child);
            }
        }
    }
}

async fn terminate_stdio_child(mut child: Child) {
    kill_stdio_child(&mut child);
    if let Err(err) = child.wait().await {
        debug!("failed to wait for exec-server stdio child: {err}");
    }
}

fn terminate_stdio_child_now(mut child: Child) {
    kill_stdio_child(&mut child);
}

fn kill_stdio_child(child: &mut Child) {
    if let Err(err) = child.start_kill() {
        debug!("failed to terminate exec-server stdio child: {err}");
    }
}

fn shell_command_process(shell_command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("cmd");
        command.arg("/C").arg(shell_command);
        command
    }

    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        command.arg("-lc").arg(shell_command);
        command
    }
}
