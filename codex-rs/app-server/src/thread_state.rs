use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::ConnectionRequestId;
use chrono::Utc;
use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadGoal;
use codex_app_server_protocol::ThreadHistoryBuilder;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnError;
use codex_core::CodexThread;
use codex_core::ThreadConfigSnapshot;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_rollout::state_db::StateDbHandle;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Weak;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tracing::error;

type PendingInterruptQueue = Vec<ConnectionRequestId>;

fn now_unix_timestamp_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn elapsed_duration_ms(started_at_ms: i64) -> i64 {
    now_unix_timestamp_ms().saturating_sub(started_at_ms)
}

pub(crate) struct PendingThreadResumeRequest {
    pub(crate) request_id: ConnectionRequestId,
    pub(crate) history_items: Vec<RolloutItem>,
    pub(crate) config_snapshot: ThreadConfigSnapshot,
    pub(crate) instruction_sources: Vec<AbsolutePathBuf>,
    pub(crate) thread_summary: codex_app_server_protocol::Thread,
    pub(crate) emit_thread_goal_update: bool,
    pub(crate) thread_goal_state_db: Option<StateDbHandle>,
    pub(crate) include_turns: bool,
}

// ThreadListenerCommand is used to perform operations in the context of the thread listener, for serialization purposes.
pub(crate) enum ThreadListenerCommand {
    // SendThreadResumeResponse is used to resume an already running thread by sending the thread's history to the client and atomically subscribing for new updates.
    SendThreadResumeResponse(Box<PendingThreadResumeRequest>),
    // EmitThreadGoalUpdated is used to order app-server goal updates with running-thread resume responses.
    EmitThreadGoalUpdated {
        goal: ThreadGoal,
    },
    // EmitThreadGoalCleared is used to order app-server goal clears with running-thread resume responses.
    EmitThreadGoalCleared,
    // EmitThreadGoalSnapshot is used to read and emit the latest goal state in the listener order.
    EmitThreadGoalSnapshot {
        state_db: StateDbHandle,
    },
    // ResolveServerRequest is used to notify the client that the request has been resolved.
    // It is executed in the thread listener's context to ensure that the resolved notification is ordered with regard to the request itself.
    ResolveServerRequest {
        request_id: RequestId,
        completion_tx: oneshot::Sender<()>,
    },
}

/// Per-conversation accumulation of the latest states e.g. error message while a turn runs.
#[derive(Default, Clone)]
pub(crate) struct TurnSummary {
    pub(crate) started_at: Option<i64>,
    pub(crate) command_execution_started: HashSet<String>,
    pub(crate) last_error: Option<TurnError>,
}

#[derive(Default)]
pub(crate) struct ThreadState {
    pub(crate) pending_interrupts: PendingInterruptQueue,
    pub(crate) pending_rollbacks: Option<ConnectionRequestId>,
    pub(crate) turn_summary: TurnSummary,
    pub(crate) last_terminal_turn_id: Option<String>,
    pub(crate) cancel_tx: Option<oneshot::Sender<()>>,
    pub(crate) experimental_raw_events: bool,
    pub(crate) listener_generation: u64,
    running_command_started_at_ms: HashMap<String, i64>,
    listener_command_tx: Option<mpsc::UnboundedSender<ThreadListenerCommand>>,
    current_turn_history: ThreadHistoryBuilder,
    listener_thread: Option<Weak<CodexThread>>,
}

impl ThreadState {
    pub(crate) fn listener_matches(&self, conversation: &Arc<CodexThread>) -> bool {
        self.listener_thread
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|existing| Arc::ptr_eq(&existing, conversation))
    }

    pub(crate) fn set_listener(
        &mut self,
        cancel_tx: oneshot::Sender<()>,
        conversation: &Arc<CodexThread>,
    ) -> (mpsc::UnboundedReceiver<ThreadListenerCommand>, u64) {
        if let Some(previous) = self.cancel_tx.replace(cancel_tx) {
            let _ = previous.send(());
        }
        self.listener_generation = self.listener_generation.wrapping_add(1);
        let (listener_command_tx, listener_command_rx) = mpsc::unbounded_channel();
        self.listener_command_tx = Some(listener_command_tx);
        self.listener_thread = Some(Arc::downgrade(conversation));
        (listener_command_rx, self.listener_generation)
    }

    pub(crate) fn clear_listener(&mut self) {
        if let Some(cancel_tx) = self.cancel_tx.take() {
            let _ = cancel_tx.send(());
        }
        self.listener_command_tx = None;
        self.current_turn_history.reset();
        self.listener_thread = None;
    }

    pub(crate) fn set_experimental_raw_events(&mut self, enabled: bool) {
        self.experimental_raw_events = enabled;
    }

    pub(crate) fn listener_command_tx(
        &self,
    ) -> Option<mpsc::UnboundedSender<ThreadListenerCommand>> {
        self.listener_command_tx.clone()
    }

    pub(crate) fn active_turn_snapshot(&self) -> Option<Turn> {
        let mut turn = self.current_turn_history.active_turn_snapshot()?;
        self.refresh_running_command_durations(std::slice::from_mut(&mut turn));
        Some(turn)
    }

    pub(crate) fn refresh_running_command_durations(&self, turns: &mut [Turn]) {
        for turn in turns {
            for item in &mut turn.items {
                let ThreadItem::CommandExecution {
                    id,
                    status: CommandExecutionStatus::InProgress,
                    duration_ms,
                    ..
                } = item
                else {
                    continue;
                };
                let Some(started_at) = self.running_command_started_at_ms.get(id) else {
                    continue;
                };
                *duration_ms = Some(elapsed_duration_ms(*started_at));
            }
        }
    }

    pub(crate) fn track_current_turn_event(&mut self, event_turn_id: &str, event: &EventMsg) {
        match event {
            EventMsg::TurnStarted(payload) => {
                self.turn_summary.started_at = payload.started_at;
            }
            EventMsg::ExecCommandBegin(payload) => {
                self.running_command_started_at_ms.insert(
                    payload.call_id.clone(),
                    payload.started_at_ms.unwrap_or_else(now_unix_timestamp_ms),
                );
            }
            EventMsg::ExecCommandEnd(payload) => {
                self.running_command_started_at_ms.remove(&payload.call_id);
            }
            _ => {}
        }
        self.current_turn_history.handle_event(event);
        if matches!(event, EventMsg::TurnAborted(_) | EventMsg::TurnComplete(_))
            && !self.current_turn_history.has_active_turn()
        {
            self.last_terminal_turn_id = Some(event_turn_id.to_string());
            self.current_turn_history.reset();
        }
    }
}

pub(crate) async fn resolve_server_request_on_thread_listener(
    thread_state: &Arc<Mutex<ThreadState>>,
    request_id: RequestId,
) {
    let (completion_tx, completion_rx) = oneshot::channel();
    let listener_command_tx = {
        let state = thread_state.lock().await;
        state.listener_command_tx()
    };
    let Some(listener_command_tx) = listener_command_tx else {
        error!("failed to remove pending client request: thread listener is not running");
        return;
    };

    if listener_command_tx
        .send(ThreadListenerCommand::ResolveServerRequest {
            request_id,
            completion_tx,
        })
        .is_err()
    {
        error!(
            "failed to remove pending client request: thread listener command channel is closed"
        );
        return;
    }

    if let Err(err) = completion_rx.await {
        error!("failed to remove pending client request: {err}");
    }
}

struct ThreadEntry {
    state: Arc<Mutex<ThreadState>>,
    connection_ids: HashSet<ConnectionId>,
    has_connections_watcher: watch::Sender<bool>,
}

impl Default for ThreadEntry {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(ThreadState::default())),
            connection_ids: HashSet::new(),
            has_connections_watcher: watch::channel(false).0,
        }
    }
}

impl ThreadEntry {
    fn update_has_connections(&self) {
        let _ = self.has_connections_watcher.send_if_modified(|current| {
            let prev = *current;
            *current = !self.connection_ids.is_empty();
            prev != *current
        });
    }
}

#[derive(Default)]
struct ThreadStateManagerInner {
    live_connections: HashSet<ConnectionId>,
    threads: HashMap<ThreadId, ThreadEntry>,
    thread_ids_by_connection: HashMap<ConnectionId, HashSet<ThreadId>>,
}

#[derive(Clone, Default)]
pub(crate) struct ThreadStateManager {
    state: Arc<Mutex<ThreadStateManagerInner>>,
}

impl ThreadStateManager {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn connection_initialized(&self, connection_id: ConnectionId) {
        self.state
            .lock()
            .await
            .live_connections
            .insert(connection_id);
    }

    pub(crate) async fn subscribed_connection_ids(&self, thread_id: ThreadId) -> Vec<ConnectionId> {
        let state = self.state.lock().await;
        state
            .threads
            .get(&thread_id)
            .map(|thread_entry| thread_entry.connection_ids.iter().copied().collect())
            .unwrap_or_default()
    }

    pub(crate) async fn thread_state(&self, thread_id: ThreadId) -> Arc<Mutex<ThreadState>> {
        let mut state = self.state.lock().await;
        state.threads.entry(thread_id).or_default().state.clone()
    }

    pub(crate) async fn remove_thread_state(&self, thread_id: ThreadId) {
        let thread_state = {
            let mut state = self.state.lock().await;
            let thread_state = state
                .threads
                .remove(&thread_id)
                .map(|thread_entry| thread_entry.state);
            state.thread_ids_by_connection.retain(|_, thread_ids| {
                thread_ids.remove(&thread_id);
                !thread_ids.is_empty()
            });
            thread_state
        };

        if let Some(thread_state) = thread_state {
            let mut thread_state = thread_state.lock().await;
            tracing::debug!(
                thread_id = %thread_id,
                listener_generation = thread_state.listener_generation,
                had_listener = thread_state.cancel_tx.is_some(),
                had_active_turn = thread_state.active_turn_snapshot().is_some(),
                "clearing thread listener during thread-state teardown"
            );
            thread_state.clear_listener();
        }
    }

    pub(crate) async fn clear_all_listeners(&self) {
        let thread_states = {
            let state = self.state.lock().await;
            state
                .threads
                .iter()
                .map(|(thread_id, thread_entry)| (*thread_id, thread_entry.state.clone()))
                .collect::<Vec<_>>()
        };

        for (thread_id, thread_state) in thread_states {
            let mut thread_state = thread_state.lock().await;
            tracing::debug!(
                thread_id = %thread_id,
                listener_generation = thread_state.listener_generation,
                had_listener = thread_state.cancel_tx.is_some(),
                had_active_turn = thread_state.active_turn_snapshot().is_some(),
                "clearing thread listener during app-server shutdown"
            );
            thread_state.clear_listener();
        }
    }

    pub(crate) async fn unsubscribe_connection_from_thread(
        &self,
        thread_id: ThreadId,
        connection_id: ConnectionId,
    ) -> bool {
        {
            let mut state = self.state.lock().await;
            if !state.threads.contains_key(&thread_id) {
                return false;
            }

            if !state
                .thread_ids_by_connection
                .get(&connection_id)
                .is_some_and(|thread_ids| thread_ids.contains(&thread_id))
            {
                return false;
            }

            if let Some(thread_ids) = state.thread_ids_by_connection.get_mut(&connection_id) {
                thread_ids.remove(&thread_id);
                if thread_ids.is_empty() {
                    state.thread_ids_by_connection.remove(&connection_id);
                }
            }
            if let Some(thread_entry) = state.threads.get_mut(&thread_id) {
                thread_entry.connection_ids.remove(&connection_id);
                thread_entry.update_has_connections();
            }
        };

        true
    }

    #[cfg(test)]
    pub(crate) async fn has_subscribers(&self, thread_id: ThreadId) -> bool {
        self.state
            .lock()
            .await
            .threads
            .get(&thread_id)
            .is_some_and(|thread_entry| !thread_entry.connection_ids.is_empty())
    }

    pub(crate) async fn try_ensure_connection_subscribed(
        &self,
        thread_id: ThreadId,
        connection_id: ConnectionId,
        experimental_raw_events: bool,
    ) -> Option<Arc<Mutex<ThreadState>>> {
        let thread_state = {
            let mut state = self.state.lock().await;
            if !state.live_connections.contains(&connection_id) {
                return None;
            }
            state
                .thread_ids_by_connection
                .entry(connection_id)
                .or_default()
                .insert(thread_id);
            let thread_entry = state.threads.entry(thread_id).or_default();
            thread_entry.connection_ids.insert(connection_id);
            thread_entry.update_has_connections();
            thread_entry.state.clone()
        };
        {
            let mut thread_state_guard = thread_state.lock().await;
            if experimental_raw_events {
                thread_state_guard.set_experimental_raw_events(/*enabled*/ true);
            }
        }
        Some(thread_state)
    }

    pub(crate) async fn try_add_connection_to_thread(
        &self,
        thread_id: ThreadId,
        connection_id: ConnectionId,
    ) -> bool {
        let mut state = self.state.lock().await;
        if !state.live_connections.contains(&connection_id) {
            return false;
        }
        state
            .thread_ids_by_connection
            .entry(connection_id)
            .or_default()
            .insert(thread_id);
        let thread_entry = state.threads.entry(thread_id).or_default();
        thread_entry.connection_ids.insert(connection_id);
        thread_entry.update_has_connections();
        true
    }

    pub(crate) async fn remove_connection(&self, connection_id: ConnectionId) -> Vec<ThreadId> {
        {
            let mut state = self.state.lock().await;
            state.live_connections.remove(&connection_id);
            let thread_ids = state
                .thread_ids_by_connection
                .remove(&connection_id)
                .unwrap_or_default();
            for thread_id in &thread_ids {
                if let Some(thread_entry) = state.threads.get_mut(thread_id) {
                    thread_entry.connection_ids.remove(&connection_id);
                    thread_entry.update_has_connections();
                }
            }
            thread_ids
                .into_iter()
                .filter(|thread_id| {
                    state
                        .threads
                        .get(thread_id)
                        .is_some_and(|thread_entry| thread_entry.connection_ids.is_empty())
                })
                .collect::<Vec<_>>()
        }
    }

    pub(crate) async fn subscribe_to_has_connections(
        &self,
        thread_id: ThreadId,
    ) -> Option<watch::Receiver<bool>> {
        let state = self.state.lock().await;
        state
            .threads
            .get(&thread_id)
            .map(|thread_entry| thread_entry.has_connections_watcher.subscribe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::CommandAction;
    use codex_app_server_protocol::CommandExecutionSource;
    use codex_app_server_protocol::TurnStatus;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    fn test_absolute_path(path: &str) -> AbsolutePathBuf {
        AbsolutePathBuf::try_from(PathBuf::from(path)).expect("path must be absolute")
    }

    #[test]
    fn refreshes_running_command_durations_for_live_turns() {
        let mut state = ThreadState::default();
        state
            .running_command_started_at_ms
            .insert("exec-call".into(), now_unix_timestamp_ms() - 1_500);
        let mut turns = vec![Turn {
            id: "turn-1".into(),
            items: vec![ThreadItem::CommandExecution {
                id: "exec-call".into(),
                command: "sleep 100".into(),
                cwd: test_absolute_path(if cfg!(windows) { "C:\\tmp" } else { "/tmp" }),
                process_id: Some("pid-1".into()),
                source: CommandExecutionSource::Agent,
                status: CommandExecutionStatus::InProgress,
                command_actions: vec![CommandAction::Unknown {
                    command: "sleep 100".into(),
                }],
                aggregated_output: None,
                exit_code: None,
                duration_ms: Some(0),
            }],
            status: TurnStatus::InProgress,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }];

        state.refresh_running_command_durations(&mut turns);

        let ThreadItem::CommandExecution { duration_ms, .. } = &turns[0].items[0] else {
            panic!("expected command execution item");
        };
        let elapsed = duration_ms.expect("running command duration");
        assert!(elapsed >= 1_500, "elapsed duration should include runtime");
    }

    #[test]
    fn leaves_completed_command_duration_unchanged() {
        let mut state = ThreadState::default();
        state
            .running_command_started_at_ms
            .insert("exec-call".into(), now_unix_timestamp_ms() - 1_500);
        let mut turns = vec![Turn {
            id: "turn-1".into(),
            items: vec![ThreadItem::CommandExecution {
                id: "exec-call".into(),
                command: "sleep 100".into(),
                cwd: test_absolute_path(if cfg!(windows) { "C:\\tmp" } else { "/tmp" }),
                process_id: Some("pid-1".into()),
                source: CommandExecutionSource::Agent,
                status: CommandExecutionStatus::Completed,
                command_actions: vec![CommandAction::Unknown {
                    command: "sleep 100".into(),
                }],
                aggregated_output: Some(String::new()),
                exit_code: Some(0),
                duration_ms: Some(42),
            }],
            status: TurnStatus::Completed,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }];

        state.refresh_running_command_durations(&mut turns);

        let ThreadItem::CommandExecution { duration_ms, .. } = &turns[0].items[0] else {
            panic!("expected command execution item");
        };
        assert_eq!(*duration_ms, Some(42));
    }
}
