use std::time::Duration;

use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use codex_app_server_protocol::HookEventName;
use codex_app_server_protocol::HookSource;
use codex_app_server_protocol::HooksConfigWriteParams;
use codex_app_server_protocol::HooksConfigWriteResponse;
use codex_app_server_protocol::HooksListParams;
use codex_app_server_protocol::HooksListResponse;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

fn write_user_hook_config(codex_home: &std::path::Path) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        r#"[hooks]

[[hooks.PreToolUse]]
matcher = "Bash"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "python3 /tmp/listed-hook.py"
timeout = 5
statusMessage = "running listed hook"
"#,
    )?;
    Ok(())
}

#[tokio::test]
async fn hooks_list_shows_discovered_hook() -> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    write_user_hook_config(codex_home.path())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_hooks_list_request(HooksListParams {
            cwds: vec![cwd.path().to_path_buf()],
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let HooksListResponse { data } = to_response(response)?;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].cwd.as_path(), cwd.path());
    assert_eq!(data[0].hooks.len(), 1);
    let hook = &data[0].hooks[0];
    assert_eq!(hook.event_name, HookEventName::PreToolUse);
    assert_eq!(hook.matcher.as_deref(), Some("Bash"));
    assert_eq!(hook.command.as_deref(), Some("python3 /tmp/listed-hook.py"));
    assert_eq!(hook.timeout_sec, 5);
    assert_eq!(hook.status_message.as_deref(), Some("running listed hook"));
    assert_eq!(hook.source, HookSource::User);
    assert!(!hook.is_managed);
    Ok(())
}

#[tokio::test]
async fn hooks_config_write_disables_user_hook() -> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    write_user_hook_config(codex_home.path())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_hooks_list_request(HooksListParams {
            cwds: vec![cwd.path().to_path_buf()],
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let HooksListResponse { data } = to_response(response)?;
    let hook = &data[0].hooks[0];
    assert_eq!(hook.enabled, true);

    let write_id = mcp
        .send_hooks_config_write_request(HooksConfigWriteParams {
            key: hook.key.clone(),
            enabled: false,
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(write_id)),
    )
    .await??;
    let HooksConfigWriteResponse { effective_enabled } = to_response(response)?;
    assert_eq!(effective_enabled, false);

    let request_id = mcp
        .send_hooks_list_request(HooksListParams {
            cwds: vec![cwd.path().to_path_buf()],
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let HooksListResponse { data } = to_response(response)?;
    assert_eq!(data[0].hooks.len(), 1);
    assert_eq!(data[0].hooks[0].key, hook.key);
    assert_eq!(data[0].hooks[0].enabled, false);
    Ok(())
}
