//! App-server-owned runtime for goals, backed by codex-state persistence.

mod accounting;
mod prompts;
mod state;
#[cfg(test)]
mod tests;
mod tools;

use codex_core::SessionRuntimeEvent;
use codex_core::SessionRuntimeExtension;
use codex_core::SessionRuntimeHandle;
use codex_core::SessionToolError;
use codex_core::SessionToolInvocation;
use codex_core::SessionToolOutput;
use codex_core::SessionToolSpecContext;
use codex_protocol::ThreadId;
use codex_rollout::state_db::StateDbHandle;
use codex_tools::ToolSpec;
use futures::future::BoxFuture;
use state::GoalRuntimeState;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Default)]
pub(crate) struct GoalRuntime {
    states: Mutex<HashMap<ThreadId, Arc<GoalRuntimeState>>>,
}

impl GoalRuntime {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn prepare_external_goal_mutation(&self, handle: SessionRuntimeHandle) {
        if let Err(err) = self.account_goal_before_external_mutation(&handle).await {
            tracing::warn!("failed to account goal progress before external mutation: {err}");
        }
    }

    pub(crate) async fn apply_external_goal_set(
        &self,
        handle: SessionRuntimeHandle,
        status: codex_state::ThreadGoalStatus,
    ) {
        self.apply_external_goal_status(&handle, status).await;
    }

    pub(crate) async fn apply_external_goal_clear(&self, thread_id: ThreadId) {
        self.clear_stopped_goal_runtime_state(thread_id).await;
    }

    pub(crate) async fn clear_thread_state(&self, thread_id: ThreadId) {
        self.states.lock().await.remove(&thread_id);
    }

    async fn state(&self, thread_id: ThreadId) -> Arc<GoalRuntimeState> {
        let mut states = self.states.lock().await;
        states
            .entry(thread_id)
            .or_insert_with(|| Arc::new(GoalRuntimeState::new()))
            .clone()
    }

    async fn maybe_state(&self, thread_id: ThreadId) -> Option<Arc<GoalRuntimeState>> {
        self.states.lock().await.get(&thread_id).cloned()
    }

    async fn require_state_db(
        &self,
        handle: &SessionRuntimeHandle,
    ) -> anyhow::Result<StateDbHandle> {
        handle
            .state_db_for_persisted_thread()
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("goals require a persisted thread; this thread is ephemeral")
            })
    }
}

impl SessionRuntimeExtension for GoalRuntime {
    fn tool_specs(&self, context: SessionToolSpecContext) -> Vec<ToolSpec> {
        if context.ephemeral {
            return Vec::new();
        }
        vec![
            codex_tools::create_get_goal_tool(),
            codex_tools::create_create_goal_tool(),
            codex_tools::create_update_goal_tool(),
        ]
    }

    fn handle_tool_call<'a>(
        &'a self,
        handle: SessionRuntimeHandle,
        invocation: SessionToolInvocation,
    ) -> BoxFuture<'a, Result<SessionToolOutput, SessionToolError>> {
        Box::pin(async move { self.handle_tool(handle, invocation).await })
    }

    fn on_event<'a>(
        &'a self,
        handle: SessionRuntimeHandle,
        event: SessionRuntimeEvent,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move { self.apply_event(handle, event).await })
    }
}
