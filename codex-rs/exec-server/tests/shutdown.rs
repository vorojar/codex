#![cfg(unix)]

mod common;

use std::time::Duration;

use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCResponse;
use codex_exec_server::ExecResponse;
use codex_exec_server::InitializeParams;
use codex_exec_server::ProcessId;
use common::exec_server::ExecServerHarness;
use common::exec_server::exec_server_with_config;
use pretty_assertions::assert_eq;
use tokio::time::Instant;
use tokio_tungstenite::connect_async;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigterm_drains_active_process_before_exit() -> anyhow::Result<()> {
    let mut server = exec_server_with_config("graceful_shutdown_timeout_ms = 2000\n").await?;
    initialize_exec_server(&mut server).await?;
    start_sleep_process(&mut server, "proc-drain", "0.4").await?;

    server.send_sigterm()?;
    server
        .assert_still_running_for(Duration::from_millis(100))
        .await?;
    let status = server.wait_for_exit(Duration::from_secs(3)).await?;

    assert!(status.success(), "exec-server exited with {status}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_timeout_forces_active_process_shutdown() -> anyhow::Result<()> {
    let mut server = exec_server_with_config("graceful_shutdown_timeout_ms = 100\n").await?;
    initialize_exec_server(&mut server).await?;
    start_sleep_process(&mut server, "proc-timeout", "5").await?;

    server.send_sigterm()?;
    let status = server.wait_for_exit(Duration::from_secs(2)).await?;

    assert!(status.success(), "exec-server exited with {status}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_signal_forces_shutdown_during_drain() -> anyhow::Result<()> {
    let mut server = exec_server_with_config("graceful_shutdown_timeout_ms = 5000\n").await?;
    initialize_exec_server(&mut server).await?;
    start_sleep_process(&mut server, "proc-second-signal", "5").await?;

    server.send_sigint()?;
    server
        .assert_still_running_for(Duration::from_millis(100))
        .await?;
    server.send_sigint()?;
    let status = server.wait_for_exit(Duration::from_secs(2)).await?;

    assert!(status.success(), "exec-server exited with {status}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn draining_rejects_new_process_starts_on_existing_connection() -> anyhow::Result<()> {
    let mut server = exec_server_with_config("graceful_shutdown_timeout_ms = 2000\n").await?;
    initialize_exec_server(&mut server).await?;
    start_sleep_process(&mut server, "proc-existing", "0.8").await?;

    server.send_sigterm()?;
    wait_until_new_connections_are_refused(server.websocket_url()).await?;
    let request_id = server
        .send_request(
            "process/start",
            serde_json::json!({
                "processId": "proc-rejected",
                "argv": ["true"],
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
                JSONRPCMessage::Error(JSONRPCError { id, .. }) if id == &request_id
            )
        })
        .await?;

    let JSONRPCMessage::Error(JSONRPCError { error, .. }) = response else {
        panic!("expected process/start to fail while draining");
    };
    assert_eq!(error.code, -32600);
    assert_eq!(
        error.message,
        "exec-server is draining; new processes are not accepted"
    );

    let status = server.wait_for_exit(Duration::from_secs(3)).await?;
    assert!(status.success(), "exec-server exited with {status}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signal_stops_accepting_new_websocket_connections() -> anyhow::Result<()> {
    let mut server = exec_server_with_config("graceful_shutdown_timeout_ms = 2000\n").await?;
    initialize_exec_server(&mut server).await?;
    start_sleep_process(&mut server, "proc-connection-refused", "0.8").await?;

    server.send_sigterm()?;
    wait_until_new_connections_are_refused(server.websocket_url()).await?;
    let status = server.wait_for_exit(Duration::from_secs(3)).await?;
    assert!(status.success(), "exec-server exited with {status}");
    Ok(())
}

async fn initialize_exec_server(server: &mut ExecServerHarness) -> anyhow::Result<()> {
    let initialize_id = server
        .send_request(
            "initialize",
            serde_json::to_value(InitializeParams {
                client_name: "exec-server-test".to_string(),
                resume_session_id: None,
            })?,
        )
        .await?;
    let _ = wait_for_response(server, initialize_id).await?;
    server
        .send_notification("initialized", serde_json::json!({}))
        .await
}

async fn start_sleep_process(
    server: &mut ExecServerHarness,
    process_id: &str,
    seconds: &str,
) -> anyhow::Result<()> {
    let request_id = server
        .send_request(
            "process/start",
            serde_json::json!({
                "processId": process_id,
                "argv": ["/bin/sh", "-c", format!("sleep {seconds}")],
                "cwd": std::env::current_dir()?,
                "env": {},
                "tty": false,
                "pipeStdin": false,
                "arg0": null
            }),
        )
        .await?;
    let result = wait_for_response(server, request_id).await?;
    let response: ExecResponse = serde_json::from_value(result)?;
    assert_eq!(
        response,
        ExecResponse {
            process_id: ProcessId::from(process_id)
        }
    );
    Ok(())
}

async fn wait_for_response(
    server: &mut ExecServerHarness,
    expected_id: codex_app_server_protocol::RequestId,
) -> anyhow::Result<serde_json::Value> {
    let response = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &expected_id
            )
        })
        .await?;
    let JSONRPCMessage::Response(JSONRPCResponse { result, .. }) = response else {
        panic!("expected JSON-RPC response");
    };
    Ok(result)
}

async fn wait_until_new_connections_are_refused(websocket_url: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match connect_async(websocket_url).await {
            Ok((websocket, _)) => {
                drop(websocket);
                if Instant::now() >= deadline {
                    anyhow::bail!("exec-server kept accepting websocket connections after signal");
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(_) => return Ok(()),
        }
    }
}
