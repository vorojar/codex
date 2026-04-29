use std::time::Duration;

use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::DEFAULT_CLIENT_NAME;
use app_test_support::McpProcess;
use app_test_support::to_response;
use app_test_support::write_chatgpt_auth;
use app_test_support::write_mock_responses_config_toml_with_chatgpt_base_url;
use codex_app_server_protocol::RemoteControlEnrollment;
use codex_app_server_protocol::RemoteControlEnrollmentReadResponse;
use codex_app_server_protocol::RequestId;
use codex_config::types::AuthCredentialsStoreMode;
use codex_state::RemoteControlEnrollmentRecord;
use codex_state::StateRuntime;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api";
const REMOTE_CONTROL_WEBSOCKET_URL: &str =
    "wss://chatgpt.com/backend-api/wham/remote/control/server";

#[tokio::test]
async fn reads_persisted_remote_control_enrollment_for_initialized_client() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml_with_chatgpt_base_url(
        codex_home.path(),
        "http://localhost:0",
        CHATGPT_BASE_URL,
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("access-token")
            .account_id("account-123")
            .chatgpt_account_id("account-123"),
        AuthCredentialsStoreMode::File,
    )?;
    let state_db =
        StateRuntime::init(codex_home.path().to_path_buf(), "mock_provider".into()).await?;
    state_db
        .upsert_remote_control_enrollment(&RemoteControlEnrollmentRecord {
            websocket_url: REMOTE_CONTROL_WEBSOCKET_URL.to_string(),
            account_id: "account-123".to_string(),
            app_server_client_name: Some(DEFAULT_CLIENT_NAME.to_string()),
            server_id: "srv_e_test".to_string(),
            environment_id: "env_test".to_string(),
            server_name: "test-server".to_string(),
        })
        .await?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    assert_eq!(
        read_remote_control_enrollment(&mut mcp).await?,
        RemoteControlEnrollmentReadResponse {
            enrollment: Some(RemoteControlEnrollment {
                server_id: "srv_e_test".to_string(),
                environment_id: "env_test".to_string(),
            }),
        }
    );
    Ok(())
}

#[tokio::test]
async fn returns_null_when_remote_control_enrollment_is_unknown() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml_with_chatgpt_base_url(
        codex_home.path(),
        "http://localhost:0",
        CHATGPT_BASE_URL,
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    assert_eq!(
        read_remote_control_enrollment(&mut mcp).await?,
        RemoteControlEnrollmentReadResponse { enrollment: None }
    );
    Ok(())
}

async fn read_remote_control_enrollment(
    mcp: &mut McpProcess,
) -> Result<RemoteControlEnrollmentReadResponse> {
    let request_id = mcp
        .send_raw_request("remoteControl/enrollment/read", Some(serde_json::json!({})))
        .await?;
    let response = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    to_response(response)
}
