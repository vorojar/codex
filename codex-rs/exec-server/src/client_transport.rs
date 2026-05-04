use std::process::Stdio;
#[cfg(unix)]
use std::thread::sleep;
#[cfg(unix)]
use std::thread::spawn;
use std::time::Duration;

#[cfg(unix)]
use codex_utils_pty::process_group::kill_process_group;
#[cfg(unix)]
use codex_utils_pty::process_group::terminate_process_group;
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
use crate::client_api::RemoteExecServerConnectArgs;
use crate::client_api::StdioExecServerConnectArgs;
use crate::connection::JsonRpcConnection;

const ENVIRONMENT_CLIENT_NAME: &str = "codex-environment";
const ENVIRONMENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ENVIRONMENT_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const STDIO_CHILD_TERM_GRACE_PERIOD: Duration = Duration::from_millis(500);

impl ExecServerClient {
    pub(crate) async fn connect_for_environment(
        transport: crate::client_api::ExecServerTransport,
    ) -> Result<Self, ExecServerError> {
        match transport {
            crate::client_api::ExecServerTransport::WebSocketUrl(websocket_url) => {
                Self::connect_websocket(RemoteExecServerConnectArgs {
                    websocket_url,
                    client_name: ENVIRONMENT_CLIENT_NAME.to_string(),
                    connect_timeout: ENVIRONMENT_CONNECT_TIMEOUT,
                    initialize_timeout: ENVIRONMENT_INITIALIZE_TIMEOUT,
                    resume_session_id: None,
                })
                .await
            }
            crate::client_api::ExecServerTransport::StdioShellCommand(shell_command) => {
                Self::connect_stdio_command(StdioExecServerConnectArgs {
                    shell_command,
                    client_name: ENVIRONMENT_CLIENT_NAME.to_string(),
                    initialize_timeout: ENVIRONMENT_INITIALIZE_TIMEOUT,
                    resume_session_id: None,
                })
                .await
            }
        }
    }

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
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(ExecServerError::Spawn)?;
        let process_id = child.id();

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
            .with_transport_lifetime(Box::new(StdioChildGuard {
                child: Some(child),
                process_id,
            })),
            args.into(),
        )
        .await
    }
}

struct StdioChildGuard {
    child: Option<Child>,
    process_id: Option<u32>,
}

impl Drop for StdioChildGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };

        terminate_stdio_child_process(self.process_id, &mut child);

        if let Ok(handle) = Handle::try_current() {
            let _wait_task = handle.spawn(wait_stdio_child(child));
        }
    }
}

async fn wait_stdio_child(mut child: Child) {
    if let Err(err) = child.wait().await {
        debug!("failed to wait for exec-server stdio child: {err}");
    }
}

#[cfg(unix)]
fn terminate_stdio_child_process(process_group_id: Option<u32>, child: &mut Child) {
    let Some(process_group_id) = process_group_id else {
        kill_stdio_child(child);
        return;
    };

    let should_escalate = match terminate_process_group(process_group_id) {
        Ok(exists) => exists,
        Err(err) => {
            debug!("failed to terminate exec-server stdio process group {process_group_id}: {err}");
            false
        }
    };
    if should_escalate {
        spawn(move || {
            sleep(STDIO_CHILD_TERM_GRACE_PERIOD);
            if let Err(err) = kill_process_group(process_group_id) {
                debug!("failed to kill exec-server stdio process group {process_group_id}: {err}");
            }
        });
    }
}

#[cfg(windows)]
fn terminate_stdio_child_process(process_id: Option<u32>, child: &mut Child) {
    if let Some(process_id) = process_id {
        let _ = std::process::Command::new("taskkill")
            .arg("/PID")
            .arg(process_id.to_string())
            .arg("/T")
            .arg("/F")
            .output();
    }
    kill_stdio_child(child);
}

#[cfg(not(any(unix, windows)))]
fn terminate_stdio_child_process(_process_id: Option<u32>, child: &mut Child) {
    kill_stdio_child(child);
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
        command.process_group(0);
        command
    }
}
