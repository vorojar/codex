//! Extension hooks for host-owned session behavior.
//!
//! Core owns the model loop, tools router, task lifecycle, and turn state. Hosts
//! can install one extension to add model-visible tools and react to lifecycle
//! events without baking product-specific policy into `Session`.

use crate::StateDbHandle;
use crate::session::session::Session;
use anyhow::Context;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TurnAbortReason;
use codex_rollout::state_db::reconcile_rollout;
use codex_thread_store::LocalThreadStore;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use futures::future::BoxFuture;
use std::sync::Arc;

/// Read-only context used when asking an extension which tools should be
/// exposed for a turn.
#[derive(Clone, Copy)]
pub struct SessionToolSpecContext {
    pub mode: ModeKind,
    pub ephemeral: bool,
}

/// Lifecycle event delivered to a session runtime extension.
#[derive(Clone)]
pub enum SessionRuntimeEvent {
    TurnStarted {
        turn_id: String,
        mode: ModeKind,
        token_usage: TokenUsage,
    },
    ToolCompleted {
        turn_id: String,
        mode: ModeKind,
        tool_name: ToolName,
    },
    TurnFinished {
        turn_id: String,
        mode: ModeKind,
        turn_completed: bool,
    },
    MaybeContinueIfIdle,
    TaskAborted {
        turn_id: Option<String>,
        reason: TurnAbortReason,
    },
    ThreadResumed,
}

/// Tool invocation delivered to a host extension.
#[derive(Clone)]
pub struct SessionToolInvocation {
    pub tool_name: ToolName,
    pub call_id: String,
    pub turn_id: String,
    pub mode: ModeKind,
    pub arguments: String,
}

/// Model-facing output for an extension-provided tool.
pub struct SessionToolOutput {
    pub body: Vec<FunctionCallOutputContentItem>,
    pub success: Option<bool>,
}

impl SessionToolOutput {
    pub fn from_text(text: String, success: Option<bool>) -> Self {
        Self {
            body: vec![FunctionCallOutputContentItem::InputText { text }],
            success,
        }
    }
}

/// Error shape for extension-provided tools.
pub enum SessionToolError {
    RespondToModel(String),
    Fatal(String),
}

/// Host extension installed into a core session.
///
/// Implementations should keep their own state outside core, keyed by
/// [`codex_protocol::ThreadId`] when needed. The returned futures are boxed
/// explicitly so implementers do not need `async_trait`.
pub trait SessionRuntimeExtension: Send + Sync {
    /// Return model-visible tool specs for the current turn context.
    fn tool_specs(&self, _context: SessionToolSpecContext) -> Vec<ToolSpec> {
        Vec::new()
    }

    /// Handle an invocation for one of the specs returned by [`Self::tool_specs`].
    fn handle_tool_call<'a>(
        &'a self,
        _handle: SessionRuntimeHandle,
        _invocation: SessionToolInvocation,
    ) -> BoxFuture<'a, Result<SessionToolOutput, SessionToolError>> {
        Box::pin(async {
            Err(SessionToolError::Fatal(
                "extension tool handler is not implemented".to_string(),
            ))
        })
    }

    /// React to a core session lifecycle event.
    fn on_event<'a>(
        &'a self,
        _handle: SessionRuntimeHandle,
        _event: SessionRuntimeEvent,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

/// Safe operations exposed by core to a host-owned session extension.
#[derive(Clone)]
pub struct SessionRuntimeHandle {
    session: Arc<Session>,
}

impl SessionRuntimeHandle {
    pub(crate) fn new(session: Arc<Session>) -> Self {
        Self { session }
    }

    pub fn thread_id(&self) -> codex_protocol::ThreadId {
        self.session.conversation_id
    }

    pub async fn collaboration_mode(&self) -> CollaborationMode {
        self.session.collaboration_mode().await
    }

    pub async fn total_token_usage(&self) -> Option<TokenUsage> {
        self.session.total_token_usage().await
    }

    pub async fn active_turn_id(&self) -> Option<String> {
        self.session.active_turn_id().await
    }

    pub async fn emit_event_raw(&self, msg: EventMsg) {
        self.session
            .send_event_raw(Event {
                id: uuid::Uuid::new_v4().to_string(),
                msg,
            })
            .await;
    }

    pub async fn inject_response_items(
        &self,
        items: Vec<ResponseInputItem>,
    ) -> Result<(), Vec<ResponseInputItem>> {
        self.session.inject_response_items(items).await
    }

    pub async fn has_active_turn(&self) -> bool {
        self.session.active_turn.lock().await.is_some()
    }

    pub async fn has_queued_response_items_for_next_turn(&self) -> bool {
        self.session.has_queued_response_items_for_next_turn().await
    }

    pub async fn has_trigger_turn_mailbox_items(&self) -> bool {
        self.session.has_trigger_turn_mailbox_items().await
    }

    pub async fn maybe_start_turn_for_pending_work(&self) {
        self.session.maybe_start_turn_for_pending_work().await;
    }

    pub async fn try_start_idle_background_turn(&self, items: Vec<ResponseInputItem>) -> bool {
        self.session.try_start_idle_background_turn(items).await
    }

    /// Open the state DB for a persisted local thread, materializing and
    /// reconciling the rollout first when necessary.
    pub async fn state_db_for_persisted_thread(&self) -> anyhow::Result<Option<StateDbHandle>> {
        let config = self.session.get_config().await;
        if config.ephemeral {
            return Ok(None);
        }

        self.session
            .try_ensure_rollout_materialized()
            .await
            .context("failed to materialize rollout before opening extension state db")?;

        let state_db = if let Some(state_db) = self.session.state_db() {
            state_db
        } else if let Some(local_store) = self
            .session
            .services
            .thread_store
            .as_any()
            .downcast_ref::<LocalThreadStore>()
        {
            local_store.state_db().await.ok_or_else(|| {
                anyhow::anyhow!("extension state requires a local persisted thread state database")
            })?
        } else {
            anyhow::bail!("extension state requires a local persisted thread state database");
        };

        let thread_metadata_present = state_db
            .get_thread(self.session.conversation_id)
            .await
            .context("failed to read thread metadata before extension state reconciliation")?
            .is_some();
        if !thread_metadata_present {
            let rollout_path = self
                .session
                .current_rollout_path()
                .await
                .context("failed to locate rollout before extension state reconciliation")?
                .ok_or_else(|| {
                    anyhow::anyhow!("extension state requires materialized thread metadata")
                })?;
            reconcile_rollout(
                Some(&state_db),
                rollout_path.as_path(),
                config.model_provider_id.as_str(),
                /*builder*/ None,
                &[],
                /*archived_only*/ None,
                /*new_thread_memory_mode*/ None,
            )
            .await;
            let thread_metadata_present = state_db
                .get_thread(self.session.conversation_id)
                .await
                .context("failed to read thread metadata after extension state reconciliation")?
                .is_some();
            if !thread_metadata_present {
                anyhow::bail!(
                    "thread metadata is unavailable after extension state reconciliation"
                );
            }
        }

        Ok(Some(state_db))
    }
}
