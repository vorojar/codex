#![cfg(unix)]

mod common;

use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCResponse;
use codex_exec_server::ExecResponse;
use codex_exec_server::InitializeParams;
use codex_exec_server::InitializeResponse;
use codex_exec_server::ProcessId;
use common::exec_server::exec_server;
use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_server_serves_status_endpoints_on_same_listener() -> anyhow::Result<()> {
    let mut server = exec_server().await?;
    let client = reqwest::Client::new();

    let healthz = client
        .get(http_url(server.websocket_url(), "/healthz"))
        .send()
        .await?;
    assert_eq!(healthz.status(), StatusCode::OK);
    assert_eq!(healthz.text().await?, "ok\n");

    let readyz = client
        .get(http_url(server.websocket_url(), "/readyz"))
        .send()
        .await?;
    assert_eq!(readyz.status(), StatusCode::OK);
    assert_eq!(readyz.text().await?, "ready\n");

    let initial_status: serde_json::Value = client
        .get(http_url(server.websocket_url(), "/status"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(initial_status["service"], "codex-exec-server");
    assert_eq!(initial_status["status"], "ready");
    assert_eq!(
        initial_status["connections"]["active"],
        serde_json::json!(1)
    );
    assert_eq!(initial_status["sessions"]["active"], serde_json::json!(0));

    let initialize_id = server
        .send_request(
            "initialize",
            serde_json::to_value(InitializeParams {
                client_name: "exec-server-test".to_string(),
                resume_session_id: None,
            })?,
        )
        .await?;
    let response = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &initialize_id
            )
        })
        .await?;
    let JSONRPCMessage::Response(JSONRPCResponse { result, .. }) = response else {
        panic!("expected initialize response");
    };
    let initialize_response: InitializeResponse = serde_json::from_value(result)?;
    Uuid::parse_str(&initialize_response.session_id)?;

    server
        .send_notification("initialized", serde_json::json!({}))
        .await?;
    let process_start_id = server
        .send_request(
            "process/start",
            serde_json::json!({
                "processId": "proc-status",
                "argv": ["sh", "-c", "sleep 5"],
                "cwd": std::env::current_dir()?,
                "env": {},
                "tty": false,
                "pipeStdin": false,
                "arg0": null
            }),
        )
        .await?;
    let response = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &process_start_id
            )
        })
        .await?;
    let JSONRPCMessage::Response(JSONRPCResponse { result, .. }) = response else {
        panic!("expected process/start response");
    };
    let process_start_response: ExecResponse = serde_json::from_value(result)?;
    assert_eq!(
        process_start_response,
        ExecResponse {
            process_id: ProcessId::from("proc-status")
        }
    );

    let status_after_process: serde_json::Value = client
        .get(http_url(server.websocket_url(), "/status"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        status_after_process["sessions"]["active"],
        serde_json::json!(1)
    );
    assert_eq!(
        status_after_process["processes"]["running"],
        serde_json::json!(1)
    );
    assert_eq!(
        status_after_process["processes"]["totalStarted"],
        serde_json::json!(1)
    );
    assert_eq!(
        status_after_process["requests"]["succeeded"],
        serde_json::json!(2)
    );

    let metrics = client
        .get(http_url(server.websocket_url(), "/metrics"))
        .send()
        .await?
        .text()
        .await?;
    assert!(metrics.contains("codex_exec_server_uptime_seconds"));
    assert!(metrics.contains("codex_exec_server_connections{state=\"active\"} 1"));
    assert!(
        metrics.contains("codex_exec_server_requests_total{method=\"initialize\",result=\"ok\"} 1")
    );
    assert!(
        metrics
            .contains("codex_exec_server_requests_total{method=\"process/start\",result=\"ok\"} 1")
    );

    server.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_server_reports_malformed_websocket_json_and_keeps_running() -> anyhow::Result<()> {
    let mut server = exec_server().await?;
    server.send_raw_text("not-json").await?;

    let response = server
        .wait_for_event(|event| matches!(event, JSONRPCMessage::Error(_)))
        .await?;
    let JSONRPCMessage::Error(JSONRPCError { id, error }) = response else {
        panic!("expected malformed-message error response");
    };
    assert_eq!(id, codex_app_server_protocol::RequestId::Integer(-1));
    assert_eq!(error.code, -32600);
    assert!(
        error
            .message
            .starts_with("failed to parse websocket JSON-RPC message from exec-server websocket"),
        "unexpected malformed-message error: {}",
        error.message
    );

    let initialize_id = server
        .send_request(
            "initialize",
            serde_json::to_value(InitializeParams {
                client_name: "exec-server-test".to_string(),
                resume_session_id: None,
            })?,
        )
        .await?;

    let response = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &initialize_id
            )
        })
        .await?;
    let JSONRPCMessage::Response(JSONRPCResponse { id, result }) = response else {
        panic!("expected initialize response after malformed input");
    };
    assert_eq!(id, initialize_id);
    let initialize_response: InitializeResponse = serde_json::from_value(result)?;
    Uuid::parse_str(&initialize_response.session_id)?;

    server.shutdown().await?;
    Ok(())
}

fn http_url(websocket_url: &str, path: &str) -> String {
    let http_authority = match websocket_url.strip_prefix("ws://") {
        Some(http_authority) => http_authority,
        None => panic!("exec-server harness should expose a ws:// URL: {websocket_url}"),
    };
    format!("http://{http_authority}{path}",)
}
