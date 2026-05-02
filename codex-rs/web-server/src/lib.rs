use anyhow::Context;
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::extract::State;
use axum::extract::ws::Message;
use axum::extract::ws::WebSocket;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use clap::Args;
use codex_utils_pty::ProcessHandle;
use codex_utils_pty::SpawnedProcess;
use codex_utils_pty::TerminalSize;
use codex_utils_pty::spawn_pty_process;
use futures::SinkExt;
use futures::StreamExt;
use include_dir::Dir;
use include_dir::include_dir;
use std::collections::HashMap;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::debug;
use tracing::warn;
use url::Position;
use url::Url;

const INPUT_FRAME: u8 = 0x00;
const RESIZE_FRAME: u8 = 0x01;
const DEFAULT_TERMINAL_SIZE: TerminalSize = TerminalSize { rows: 24, cols: 80 };
static WEBUI_ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets");

/// Run Codex in a browser-backed terminal served by the current Codex binary.
#[derive(Clone, Debug, Args)]
pub struct WebCommand {
    /// Address to bind. Only loopback addresses are accepted.
    #[arg(long, default_value = "127.0.0.1:0", value_name = "ADDR")]
    pub listen: SocketAddr,

    /// Working directory for the Codex session.
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<PathBuf>,

    /// Open the served URL in the default browser.
    #[arg(long)]
    pub open: bool,

    /// Internal test hook: command to spawn instead of this Codex executable.
    #[arg(long, hide = true, value_name = "PATH")]
    pub command: Option<PathBuf>,

    /// Internal test hook: argument for --command. May be repeated.
    #[arg(long = "command-arg", hide = true, value_name = "ARG")]
    pub command_args: Vec<String>,

    /// Arguments forwarded to the inner Codex TUI. Pass them after `--`.
    #[arg(last = true, value_name = "CODEX_ARGS")]
    pub codex_args: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    listen: SocketAddr,
    open: bool,
    command: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
}

#[derive(Clone)]
struct ServerState {
    config: Arc<ServerConfig>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClientFrame<'a> {
    Input(&'a [u8]),
    Resize { cols: u16, rows: u16 },
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameDecodeError {
    Empty,
    MalformedResize,
    ZeroResize,
    UnknownFrameType(u8),
}

pub struct StaticAsset {
    pub path: &'static str,
    pub content_type: &'static str,
    pub cache_control: &'static str,
    pub bytes: &'static [u8],
}

impl WebCommand {
    pub fn into_server_config(
        self,
        inherited_config_overrides: Vec<String>,
    ) -> anyhow::Result<ServerConfig> {
        if !self.listen.ip().is_loopback() {
            anyhow::bail!("codex web only accepts loopback --listen addresses");
        }

        let cwd = match self.cwd {
            Some(cwd) => cwd,
            None => std::env::current_dir().context("failed to read current directory")?,
        };
        let (command, args) = match self.command {
            Some(command) => (command, self.command_args),
            None => {
                let command = std::env::current_exe().context("failed to resolve current exe")?;
                let mut args = Vec::new();
                for config_override in inherited_config_overrides {
                    args.push("-c".to_string());
                    args.push(config_override);
                }
                args.extend(self.codex_args);
                (command, args)
            }
        };

        Ok(ServerConfig {
            listen: self.listen,
            open: self.open,
            command,
            args,
            cwd,
        })
    }
}

pub async fn run(
    command: WebCommand,
    inherited_config_overrides: Vec<String>,
) -> anyhow::Result<()> {
    let config = command.into_server_config(inherited_config_overrides)?;
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("failed to bind codex web listener on {}", config.listen))?;
    let url = http_url_for_addr(listener.local_addr()?);
    println!("Codex web listening on {url}");
    if config.open
        && let Err(err) = webbrowser::open(&url)
    {
        eprintln!("Failed to open browser for {url}: {err}");
    }
    serve_listener(listener, config).await
}

pub async fn serve_listener(listener: TcpListener, config: ServerConfig) -> anyhow::Result<()> {
    let state = ServerState {
        config: Arc::new(config),
    };
    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/pty", get(pty_websocket))
        .fallback(get(static_handler))
        .with_state(state);

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("codex web server failed")
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

async fn pty_websocket(
    websocket: WebSocketUpgrade,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Response {
    if !origin_is_allowed(&headers) {
        warn!(%peer_addr, "rejecting codex web websocket due to Origin mismatch");
        return StatusCode::FORBIDDEN.into_response();
    }

    websocket
        .on_upgrade(move |socket| handle_pty_socket(socket, state.config))
        .into_response()
}

async fn static_handler(request: axum::http::Request<Body>) -> Response {
    let path = request.uri().path();
    if path.starts_with("/api/") {
        return StatusCode::NOT_FOUND.into_response();
    }

    match static_asset_for_path(path) {
        Some(asset) => (
            [
                (header::CONTENT_TYPE, asset.content_type),
                (header::CACHE_CONTROL, asset.cache_control),
            ],
            asset.bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub fn static_asset_for_path(request_path: &str) -> Option<StaticAsset> {
    let asset_path = normalize_asset_path(request_path)?;
    let file = WEBUI_ASSETS
        .get_file(&asset_path)
        .or_else(|| WEBUI_ASSETS.get_file("index.html"))?;
    let path = file.path().to_str()?;
    Some(StaticAsset {
        path,
        content_type: content_type_for_path(path),
        cache_control: if path == "index.html" {
            "no-store"
        } else {
            "public, max-age=31536000, immutable"
        },
        bytes: file.contents(),
    })
}

fn normalize_asset_path(request_path: &str) -> Option<String> {
    let trimmed = request_path.strip_prefix('/').unwrap_or(request_path);
    let path = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };
    if path
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
        || path.contains('\\')
    {
        return None;
    }
    Some(path.to_string())
}

fn content_type_for_path(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") | Some("map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

pub fn decode_client_frame(bytes: &[u8]) -> Result<ClientFrame<'_>, FrameDecodeError> {
    let Some(kind) = bytes.first().copied() else {
        return Err(FrameDecodeError::Empty);
    };
    match kind {
        INPUT_FRAME => Ok(ClientFrame::Input(&bytes[1..])),
        RESIZE_FRAME => {
            if bytes.len() != 5 {
                return Err(FrameDecodeError::MalformedResize);
            }
            let cols = u16::from_be_bytes([bytes[1], bytes[2]]);
            let rows = u16::from_be_bytes([bytes[3], bytes[4]]);
            if cols == 0 || rows == 0 {
                return Err(FrameDecodeError::ZeroResize);
            }
            Ok(ClientFrame::Resize { cols, rows })
        }
        other => Err(FrameDecodeError::UnknownFrameType(other)),
    }
}

async fn handle_pty_socket(socket: WebSocket, config: Arc<ServerConfig>) {
    let spawned = match spawn_codex_pty(config.as_ref()).await {
        Ok(spawned) => spawned,
        Err(err) => {
            let mut socket = socket;
            let message = format!("Failed to start PTY: {err}\r\n");
            let _ = socket
                .send(Message::Binary(message.into_bytes().into()))
                .await;
            let _ = socket.close().await;
            return;
        }
    };

    bridge_socket_to_pty(socket, spawned).await;
}

async fn spawn_codex_pty(config: &ServerConfig) -> anyhow::Result<SpawnedProcess> {
    let command = config
        .command
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("codex web command path is not valid UTF-8"))?;
    spawn_pty_process(
        command,
        &config.args,
        &config.cwd,
        &child_environment(),
        /*arg0*/ &None,
        DEFAULT_TERMINAL_SIZE,
    )
    .await
    .with_context(|| format!("failed to spawn {}", config.command.display()))
}

async fn bridge_socket_to_pty(socket: WebSocket, spawned: SpawnedProcess) {
    let SpawnedProcess {
        session,
        mut stdout_rx,
        stderr_rx: _,
        mut exit_rx,
    } = spawned;
    let session = Arc::new(session);
    let writer = session.writer_sender();
    let resize_session = Arc::clone(&session);
    let (mut websocket_writer, mut websocket_reader) = socket.split();

    let mut outbound_task: JoinHandle<()> = tokio::spawn(async move {
        loop {
            tokio::select! {
                output = stdout_rx.recv() => {
                    let Some(output) = output else {
                        break;
                    };
                    if websocket_writer.send(Message::Binary(output.into())).await.is_err() {
                        break;
                    }
                }
                exit = &mut exit_rx => {
                    let code = exit.unwrap_or(-1);
                    let message = format!("\r\n[process exited: {code}]\r\n");
                    let _ = websocket_writer
                        .send(Message::Binary(message.into_bytes().into()))
                        .await;
                    let _ = websocket_writer.close().await;
                    break;
                }
            }
        }
    });

    let mut inbound_task: JoinHandle<()> = tokio::spawn(async move {
        while let Some(message) = websocket_reader.next().await {
            match message {
                Ok(Message::Binary(bytes)) => match decode_client_frame(&bytes) {
                    Ok(ClientFrame::Input(input)) => {
                        if writer.send(input.to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Ok(ClientFrame::Resize { cols, rows }) => {
                        if let Err(err) = resize_session.resize(TerminalSize { rows, cols }) {
                            debug!("failed to resize codex web PTY: {err}");
                        }
                    }
                    Err(err) => {
                        debug!("ignoring malformed codex web frame: {err:?}");
                    }
                },
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(Message::Text(_) | Message::Ping(_) | Message::Pong(_)) => {}
            }
        }
    });

    tokio::select! {
        _ = &mut outbound_task => {
            inbound_task.abort();
        }
        _ = &mut inbound_task => {
            outbound_task.abort();
        }
    }
    terminate_process(&session);
}

fn terminate_process(session: &ProcessHandle) {
    session.terminate();
}

fn child_environment() -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.insert("TERM".to_string(), "xterm-256color".to_string());
    env.insert("COLORTERM".to_string(), "truecolor".to_string());
    env.insert("TERM_PROGRAM".to_string(), "wterm".to_string());
    env.insert(
        "CODEX_TUI_DISABLE_KEYBOARD_ENHANCEMENT".to_string(),
        "1".to_string(),
    );
    env
}

fn origin_is_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(origin) = Url::parse(origin) else {
        return false;
    };
    let Some(host) = headers.get(header::HOST) else {
        return false;
    };
    let Ok(host) = host.to_str() else {
        return false;
    };
    origin[Position::BeforeHost..Position::AfterPort].eq_ignore_ascii_case(host)
}

fn http_url_for_addr(addr: SocketAddr) -> String {
    match addr.ip() {
        IpAddr::V4(ip) => format!("http://{ip}:{}", addr.port()),
        IpAddr::V6(ip) => format!("http://[{ip}]:{}", addr.port()),
    }
}

pub async fn spawn_for_test(
    command: PathBuf,
    args: Vec<String>,
) -> anyhow::Result<(String, oneshot::Sender<()>, JoinHandle<anyhow::Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = http_url_for_addr(listener.local_addr()?);
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let config = ServerConfig {
        listen: listener.local_addr()?,
        open: false,
        command,
        args,
        cwd: std::env::current_dir()?,
    };
    let handle = tokio::spawn(async move {
        let state = ServerState {
            config: Arc::new(config),
        };
        let router = Router::new()
            .route("/healthz", get(healthz))
            .route("/api/pty", get(pty_websocket))
            .fallback(get(static_handler))
            .with_state(state);
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        })
        .await
        .context("codex web test server failed")
    });
    Ok((url, shutdown_tx, handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn decodes_input_frames() {
        let frame = [INPUT_FRAME, b'h', b'i'];
        assert_eq!(decode_client_frame(&frame), Ok(ClientFrame::Input(b"hi")));
    }

    #[test]
    fn decodes_resize_frames() {
        let frame = [RESIZE_FRAME, 0, 120, 0, 40];
        assert_eq!(
            decode_client_frame(&frame),
            Ok(ClientFrame::Resize {
                cols: 120,
                rows: 40
            })
        );
    }

    #[test]
    fn rejects_invalid_frames() {
        assert_eq!(decode_client_frame(&[]), Err(FrameDecodeError::Empty));
        assert_eq!(
            decode_client_frame(&[RESIZE_FRAME, 0, 80]),
            Err(FrameDecodeError::MalformedResize)
        );
        assert_eq!(
            decode_client_frame(&[RESIZE_FRAME, 0, 0, 0, 24]),
            Err(FrameDecodeError::ZeroResize)
        );
        assert_eq!(
            decode_client_frame(&[9]),
            Err(FrameDecodeError::UnknownFrameType(9))
        );
    }

    #[test]
    fn serves_index_for_root_and_spa_routes() {
        let root = static_asset_for_path("/").expect("root asset");
        let route = static_asset_for_path("/thread/123").expect("route asset");

        assert_eq!(root.path, "index.html");
        assert_eq!(route.path, "index.html");
        assert_eq!(root.content_type, "text/html; charset=utf-8");
        assert_eq!(root.cache_control, "no-store");
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(static_asset_for_path("/../Cargo.toml").is_none());
        assert!(static_asset_for_path("/assets\\index.js").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn websocket_bridges_pty_output() -> anyhow::Result<()> {
        let (url, shutdown, handle) = spawn_for_test(
            PathBuf::from("/bin/sh"),
            vec!["-c".to_string(), "printf READY; cat".to_string()],
        )
        .await?;
        let ws_url = format!("{url}/api/pty").replace("http://", "ws://");
        let (mut socket, _) = tokio_tungstenite::connect_async(&ws_url).await?;
        let mut saw_ready = false;

        for _ in 0..8 {
            if let Some(message) = socket.next().await {
                let message = message?;
                if message
                    .into_data()
                    .windows("READY".len())
                    .any(|w| w == b"READY")
                {
                    saw_ready = true;
                    break;
                }
            }
        }

        let _ = socket.close(None).await;
        let _ = shutdown.send(());
        let _ = handle.await?;
        assert!(saw_ready);
        Ok(())
    }
}
