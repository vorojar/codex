use anyhow::Context;
use anyhow::Result;
use codex_core::session_state_sidecar_path;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use codex_terminal_detection::terminal_attachment;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::oneshot;

fn read_sidecar(path: &Path) -> Result<codex_core::SessionStateSidecar> {
    let bytes = std::fs::read(path).with_context(|| format!("read sidecar {}", path.display()))?;
    serde_json::from_slice(&bytes).context("parse sidecar")
}

fn read_sidecar_json(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path).with_context(|| format!("read sidecar {}", path.display()))?;
    serde_json::from_slice(&bytes).context("parse sidecar JSON")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_start_writes_session_state_sidecar() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_session_source(SessionSource::Cli);
    let test = builder.build(&server).await?;
    let rollout_path = test
        .session_configured
        .rollout_path
        .as_deref()
        .expect("rollout path");
    let sidecar_path = session_state_sidecar_path(rollout_path);

    assert!(
        sidecar_path.exists(),
        "expected {} to exist",
        sidecar_path.display()
    );

    let sidecar = read_sidecar(&sidecar_path)?;
    let sidecar_json = read_sidecar_json(&sidecar_path)?;
    assert_eq!(sidecar.schema_version, 2);
    assert_eq!(sidecar.terminal, terminal_attachment());
    assert_eq!(sidecar_json["session"]["status"], "open");
    assert!(sidecar_json["session"]["lease_expires_at"].is_string());
    assert_eq!(sidecar_json["root_turn"]["status"], "idle");
    assert_eq!(
        sidecar_json["background_exec"]["processes"],
        Value::Array(vec![])
    );
    assert_eq!(sidecar_json["owner_watchdogs"]["active_count"], 0);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_turn_updates_session_state_sidecar() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let (complete_tx, complete_rx) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![vec![
        StreamingSseChunk {
            gate: None,
            body: sse(vec![ev_response_created("resp-running")]),
        },
        StreamingSseChunk {
            gate: Some(complete_rx),
            body: sse(vec![ev_completed("resp-running")]),
        },
    ]])
    .await;

    let mut builder = test_codex().with_session_source(SessionSource::Cli);
    let test = builder.build_with_streaming_server(&server).await?;
    let sidecar_path = session_state_sidecar_path(
        test.session_configured
            .rollout_path
            .as_deref()
            .expect("rollout path"),
    );

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hold turn open".to_string(),
                text_elements: Vec::new(),
            }],
            environments: None,
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;
    let turn_id = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnStarted(event) => Some(event.turn_id.clone()),
        _ => None,
    })
    .await;

    let running = read_sidecar_json(&sidecar_path)?;
    assert_eq!(running["root_turn"]["status"], "running");
    assert_eq!(running["root_turn"]["turn_id"], turn_id);

    complete_tx.send(()).expect("release streaming response");
    wait_for_event(&test.codex, |event| match event {
        EventMsg::TurnComplete(event) => event.turn_id == turn_id,
        _ => false,
    })
    .await;

    let completed = read_sidecar_json(&sidecar_path)?;
    assert_eq!(completed["session"]["status"], "open");
    assert_eq!(completed["root_turn"]["status"], "completed");
    assert_eq!(completed["root_turn"]["turn_id"], turn_id);

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_spawn_subagent_writes_and_closes_sidecar_edge() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let parent_thread_id = ThreadId::try_from("00000000-0000-4000-8000-000000000001")?;
    let source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id,
        depth: 1,
        agent_path: None,
        agent_nickname: Some("reviewer".to_string()),
        agent_role: Some("worker".to_string()),
    });
    let server = start_mock_server().await;
    let mut builder = test_codex().with_session_source(source);
    let test = builder.build(&server).await?;
    let sidecar_path = session_state_sidecar_path(
        test.session_configured
            .rollout_path
            .as_deref()
            .expect("rollout path"),
    );

    let open = read_sidecar_json(&sidecar_path)?;
    assert_eq!(open["schema_version"], 2);
    assert_eq!(
        open["subagent"]["parent_thread_id"],
        parent_thread_id.to_string()
    );
    assert_eq!(open["subagent"]["edge_status"], "open");
    assert_eq!(open["subagent"]["agent_nickname"], "reviewer");
    assert_eq!(open["subagent"]["agent_role"], "worker");

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-subagent"),
            ev_completed("resp-subagent"),
        ]),
    )
    .await;
    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "complete subagent turn".to_string(),
                text_elements: Vec::new(),
            }],
            environments: None,
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let closed = read_sidecar_json(&sidecar_path)?;
    assert_eq!(closed["root_turn"]["status"], "completed");
    assert_eq!(closed["subagent"]["edge_status"], "closed");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_resume_refreshes_session_state_sidecar() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_session_source(SessionSource::Cli);
    let initial = builder.build(&server).await?;
    let home = initial.home.clone();
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");
    let sse = sse(vec![
        ev_response_created("resp-resume"),
        ev_assistant_message("msg-resume", "resume source"),
        ev_completed("resp-resume"),
    ]);
    mount_sse_once(&server, sse).await;
    initial.submit_turn("materialize").await?;
    let sidecar_path = session_state_sidecar_path(&rollout_path);
    std::fs::write(
        &sidecar_path,
        r#"{"schema_version":1,"updated_at":"2026-03-09T12:00:00Z","terminal":{"provider":"iterm2","session_id":"stale","tty":"/dev/ttys999"}}"#,
    )?;

    let resumed = builder.resume(&server, home, rollout_path).await?;
    let refreshed_rollout_path = resumed
        .session_configured
        .rollout_path
        .as_deref()
        .expect("resumed rollout path");
    let refreshed = read_sidecar(&session_state_sidecar_path(refreshed_rollout_path))?;
    assert_eq!(refreshed.terminal, terminal_attachment());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_shutdown_closes_session_state_sidecar() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_session_source(SessionSource::Cli);
    let test = builder.build(&server).await?;
    let sidecar_path = session_state_sidecar_path(
        test.session_configured
            .rollout_path
            .as_deref()
            .expect("rollout path"),
    );

    test.codex.shutdown_and_wait().await?;

    let sidecar = read_sidecar_json(&sidecar_path)?;
    assert_eq!(sidecar["session"]["status"], "closed");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_fork_writes_session_state_sidecar() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_session_source(SessionSource::Cli);
    let test = builder.build(&server).await?;
    let codex = Arc::clone(&test.codex);
    let rollout_path = test
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    let sse = sse(vec![
        ev_response_created("resp-1"),
        ev_assistant_message("msg-1", "fork source"),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, sse).await;
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "materialize".to_string(),
                text_elements: Vec::new(),
            }],
            environments: None,
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;
    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let forked = test
        .thread_manager
        .fork_thread(
            usize::MAX,
            test.config.clone(),
            rollout_path,
            /*persist_extended_history*/ false,
            /*parent_trace*/ None,
        )
        .await?;
    let forked_rollout_path = forked
        .session_configured
        .rollout_path
        .as_deref()
        .expect("forked rollout path");
    let sidecar = read_sidecar(&session_state_sidecar_path(forked_rollout_path))?;
    assert_eq!(sidecar.terminal, terminal_attachment());

    Ok(())
}
