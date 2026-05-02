//! Goal lifecycle accounting and continuation scheduling for running app-server threads.

use super::GoalRuntime;
use super::prompts::budget_limit_steering_item;
use super::prompts::continuation_prompt;
use super::prompts::protocol_goal_from_state;
use super::prompts::should_ignore_goal_for_mode;
use super::state::BudgetLimitSteering;
use super::state::GoalContinuationCandidate;
use super::state::GoalTurnAccountingSnapshot;
use anyhow::Context;
use codex_core::SessionRuntimeEvent;
use codex_core::SessionRuntimeHandle;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadGoal;
use codex_protocol::protocol::ThreadGoalUpdatedEvent;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TurnAbortReason;
use codex_rollout::state_db::StateDbHandle;
use codex_tools::UPDATE_GOAL_TOOL_NAME;

impl GoalRuntime {
    pub(super) async fn apply_event(
        &self,
        handle: SessionRuntimeHandle,
        event: SessionRuntimeEvent,
    ) -> anyhow::Result<()> {
        match event {
            SessionRuntimeEvent::TurnStarted {
                turn_id,
                mode,
                token_usage,
            } => {
                self.mark_goal_turn_started(&handle, turn_id, mode, token_usage)
                    .await;
                Ok(())
            }
            SessionRuntimeEvent::ToolCompleted {
                turn_id,
                mode,
                tool_name,
            } => {
                if !should_ignore_goal_for_mode(mode)
                    && tool_name.name.as_str() != UPDATE_GOAL_TOOL_NAME
                {
                    self.account_goal_progress(
                        &handle,
                        turn_id.as_str(),
                        BudgetLimitSteering::Allowed,
                    )
                    .await?;
                }
                Ok(())
            }
            SessionRuntimeEvent::TurnFinished {
                turn_id,
                mode,
                turn_completed,
            } => {
                self.finish_goal_turn(&handle, turn_id.as_str(), mode, turn_completed)
                    .await;
                Ok(())
            }
            SessionRuntimeEvent::MaybeContinueIfIdle => {
                self.maybe_continue_goal_if_idle_runtime(&handle).await;
                Ok(())
            }
            SessionRuntimeEvent::TaskAborted { turn_id, reason } => {
                self.handle_goal_task_abort(&handle, turn_id, reason).await;
                Ok(())
            }
            SessionRuntimeEvent::ThreadResumed => {
                self.activate_paused_goal_after_resume(&handle).await?;
                Ok(())
            }
        }
    }

    pub(super) async fn apply_external_goal_status(
        &self,
        handle: &SessionRuntimeHandle,
        status: codex_state::ThreadGoalStatus,
    ) {
        match status {
            codex_state::ThreadGoalStatus::Active => {
                match handle.state_db_for_persisted_thread().await {
                    Ok(Some(state_db)) => {
                        match state_db.get_thread_goal(handle.thread_id()).await {
                            Ok(Some(goal))
                                if goal.status == codex_state::ThreadGoalStatus::Active =>
                            {
                                let turn_id = handle.active_turn_id().await;
                                let current_token_usage =
                                    handle.total_token_usage().await.unwrap_or_default();
                                self.mark_active_goal_accounting(
                                    handle.thread_id(),
                                    goal.goal_id,
                                    turn_id,
                                    current_token_usage,
                                )
                                .await;
                            }
                            Ok(Some(_)) | Ok(None) => {}
                            Err(err) => {
                                tracing::warn!(
                                    "failed to read active goal after external set: {err}"
                                );
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!("failed to open state db after external goal set: {err}");
                    }
                    Ok(None) => {}
                }
                self.maybe_continue_goal_if_idle_runtime(handle).await;
            }
            codex_state::ThreadGoalStatus::BudgetLimited => {
                if !handle.has_active_turn().await {
                    self.clear_stopped_goal_runtime_state(handle.thread_id())
                        .await;
                }
            }
            codex_state::ThreadGoalStatus::Paused | codex_state::ThreadGoalStatus::Complete => {
                self.clear_stopped_goal_runtime_state(handle.thread_id())
                    .await;
            }
        }
    }

    pub(super) async fn clear_stopped_goal_runtime_state(&self, thread_id: ThreadId) {
        let Some(state) = self.maybe_state(thread_id).await else {
            return;
        };
        *state.budget_limit_reported_goal_id.lock().await = None;
        let mut accounting = state.accounting.lock().await;
        if let Some(turn) = accounting.turn.as_mut() {
            turn.clear_active_goal();
        }
        accounting.wall_clock.clear_active_goal();
    }

    pub(super) async fn clear_active_goal_accounting(&self, thread_id: ThreadId, turn_id: &str) {
        let state = self.state(thread_id).await;
        let mut accounting = state.accounting.lock().await;
        if let Some(turn) = accounting.turn.as_mut()
            && turn.turn_id == turn_id
        {
            turn.clear_active_goal();
        }
        accounting.wall_clock.clear_active_goal();
    }

    pub(super) async fn mark_active_goal_accounting(
        &self,
        thread_id: ThreadId,
        goal_id: String,
        turn_id: Option<String>,
        token_usage: TokenUsage,
    ) {
        let state = self.state(thread_id).await;
        let mut accounting = state.accounting.lock().await;
        if let Some(turn_id) = turn_id {
            match accounting.turn.as_mut() {
                Some(turn) if turn.turn_id == turn_id => {
                    turn.reset_baseline(token_usage);
                    turn.mark_active_goal(goal_id.clone());
                }
                _ => {
                    let mut turn = GoalTurnAccountingSnapshot::new(turn_id, token_usage);
                    turn.mark_active_goal(goal_id.clone());
                    accounting.turn = Some(turn);
                }
            }
        }
        accounting.wall_clock.mark_active_goal(goal_id);
    }

    async fn mark_goal_turn_started(
        &self,
        handle: &SessionRuntimeHandle,
        turn_id: String,
        mode: ModeKind,
        token_usage: TokenUsage,
    ) {
        let state = self.state(handle.thread_id()).await;
        state.accounting.lock().await.turn = Some(GoalTurnAccountingSnapshot::new(
            turn_id.clone(),
            token_usage,
        ));

        if should_ignore_goal_for_mode(mode) {
            self.clear_active_goal_accounting(handle.thread_id(), turn_id.as_str())
                .await;
            return;
        }
        let state_db = match handle.state_db_for_persisted_thread().await {
            Ok(Some(state_db)) => state_db,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!("failed to open state db at turn start: {err}");
                return;
            }
        };
        match state_db.get_thread_goal(handle.thread_id()).await {
            Ok(Some(goal))
                if matches!(
                    goal.status,
                    codex_state::ThreadGoalStatus::Active
                        | codex_state::ThreadGoalStatus::BudgetLimited
                ) =>
            {
                let mut accounting = state.accounting.lock().await;
                if let Some(turn) = accounting.turn.as_mut()
                    && turn.turn_id == turn_id
                {
                    turn.mark_active_goal(goal.goal_id.clone());
                }
                accounting.wall_clock.mark_active_goal(goal.goal_id);
            }
            Ok(Some(_)) | Ok(None) => {
                state.accounting.lock().await.wall_clock.clear_active_goal();
            }
            Err(err) => {
                tracing::warn!("failed to read goal at turn start: {err}");
            }
        }
    }

    async fn finish_goal_turn(
        &self,
        handle: &SessionRuntimeHandle,
        turn_id: &str,
        mode: ModeKind,
        turn_completed: bool,
    ) {
        if turn_completed
            && !should_ignore_goal_for_mode(mode)
            && let Err(err) = self
                .account_goal_progress(handle, turn_id, BudgetLimitSteering::Suppressed)
                .await
        {
            tracing::warn!("failed to account goal progress at turn end: {err}");
        }

        let Some(state) = self.maybe_state(handle.thread_id()).await else {
            return;
        };
        if turn_completed {
            let mut accounting = state.accounting.lock().await;
            if accounting
                .turn
                .as_ref()
                .is_some_and(|turn| turn.turn_id == turn_id)
            {
                accounting.turn = None;
            }
        }
    }

    async fn handle_goal_task_abort(
        &self,
        handle: &SessionRuntimeHandle,
        turn_id: Option<String>,
        reason: TurnAbortReason,
    ) {
        if let Some(turn_id) = turn_id {
            if let Err(err) = self
                .account_goal_progress(handle, turn_id.as_str(), BudgetLimitSteering::Suppressed)
                .await
            {
                tracing::warn!("failed to account goal progress after abort: {err}");
            }
            if let Some(state) = self.maybe_state(handle.thread_id()).await {
                let mut accounting = state.accounting.lock().await;
                if accounting
                    .turn
                    .as_ref()
                    .is_some_and(|turn| turn.turn_id == turn_id)
                {
                    accounting.turn = None;
                }
            }
        }

        if reason == TurnAbortReason::Interrupted
            && let Err(err) = self.pause_active_goal_for_interrupt(handle).await
        {
            tracing::warn!("failed to pause active goal after interrupt: {err}");
        }
    }

    pub(super) async fn account_goal_progress(
        &self,
        handle: &SessionRuntimeHandle,
        turn_id: &str,
        budget_limit_steering: BudgetLimitSteering,
    ) -> anyhow::Result<()> {
        let Some(state_db) = handle.state_db_for_persisted_thread().await? else {
            return Ok(());
        };
        let state = self.state(handle.thread_id()).await;
        let _accounting_permit = state.accounting_permit().await?;
        let current_token_usage = handle.total_token_usage().await.unwrap_or_default();
        let (token_delta, expected_goal_id, time_delta_seconds) = {
            let accounting = state.accounting.lock().await;
            let Some(turn) = accounting
                .turn
                .as_ref()
                .filter(|turn| turn.turn_id == turn_id)
            else {
                return Ok(());
            };
            if !turn.active_this_turn() {
                return Ok(());
            }
            (
                turn.token_delta_since_last_accounting(&current_token_usage),
                turn.active_goal_id(),
                accounting.wall_clock.time_delta_since_last_accounting(),
            )
        };
        if time_delta_seconds == 0 && token_delta <= 0 {
            return Ok(());
        }
        let outcome = state_db
            .account_thread_goal_usage(
                handle.thread_id(),
                time_delta_seconds,
                token_delta,
                codex_state::ThreadGoalAccountingMode::ActiveOnly,
                expected_goal_id.as_deref(),
            )
            .await?;
        let budget_limit_was_already_reported = {
            let reported_goal_id = state.budget_limit_reported_goal_id.lock().await;
            expected_goal_id
                .as_deref()
                .is_some_and(|goal_id| reported_goal_id.as_deref() == Some(goal_id))
        };
        let goal = match outcome {
            codex_state::ThreadGoalAccountingOutcome::Updated(goal) => {
                let clear_active_goal = match goal.status {
                    codex_state::ThreadGoalStatus::Active => false,
                    codex_state::ThreadGoalStatus::BudgetLimited => {
                        matches!(budget_limit_steering, BudgetLimitSteering::Suppressed)
                    }
                    codex_state::ThreadGoalStatus::Paused
                    | codex_state::ThreadGoalStatus::Complete => true,
                };
                {
                    let mut accounting = state.accounting.lock().await;
                    if let Some(turn) = accounting
                        .turn
                        .as_mut()
                        .filter(|turn| turn.turn_id == turn_id)
                    {
                        turn.mark_accounted(current_token_usage);
                        if clear_active_goal {
                            turn.clear_active_goal();
                        }
                    }
                    accounting.wall_clock.mark_accounted(time_delta_seconds);
                    if clear_active_goal {
                        accounting.wall_clock.clear_active_goal();
                    }
                }
                goal
            }
            codex_state::ThreadGoalAccountingOutcome::Unchanged(_) => return Ok(()),
        };
        let should_steer_budget_limit =
            matches!(budget_limit_steering, BudgetLimitSteering::Allowed)
                && goal.status == codex_state::ThreadGoalStatus::BudgetLimited
                && !budget_limit_was_already_reported;
        let goal_status = goal.status;
        let goal_id = goal.goal_id.clone();
        if goal_status != codex_state::ThreadGoalStatus::BudgetLimited {
            *state.budget_limit_reported_goal_id.lock().await = None;
        }
        let goal = protocol_goal_from_state(goal);
        handle
            .emit_event_raw(EventMsg::ThreadGoalUpdated(ThreadGoalUpdatedEvent {
                thread_id: handle.thread_id(),
                turn_id: Some(turn_id.to_string()),
                goal: goal.clone(),
            }))
            .await;
        if should_steer_budget_limit {
            let item = budget_limit_steering_item(&goal);
            if handle.inject_response_items(vec![item]).await.is_err() {
                tracing::debug!("skipping budget-limit goal steering because no turn is active");
            }
            *state.budget_limit_reported_goal_id.lock().await = Some(goal_id);
        }
        Ok(())
    }

    pub(super) async fn account_goal_before_external_mutation(
        &self,
        handle: &SessionRuntimeHandle,
    ) -> anyhow::Result<()> {
        if let Some(turn_id) = handle.active_turn_id().await {
            return self
                .account_goal_progress(handle, turn_id.as_str(), BudgetLimitSteering::Suppressed)
                .await;
        }

        let Some(state_db) = handle.state_db_for_persisted_thread().await? else {
            return Ok(());
        };
        self.account_goal_wall_clock_usage(
            handle.thread_id(),
            &state_db,
            codex_state::ThreadGoalAccountingMode::ActiveOnly,
        )
        .await?;
        Ok(())
    }

    pub(super) async fn account_goal_wall_clock_usage(
        &self,
        thread_id: ThreadId,
        state_db: &StateDbHandle,
        mode: codex_state::ThreadGoalAccountingMode,
    ) -> anyhow::Result<Option<ThreadGoal>> {
        let state = self.state(thread_id).await;
        let _accounting_permit = state.accounting_permit().await?;
        let (time_delta_seconds, expected_goal_id) = {
            let accounting = state.accounting.lock().await;
            (
                accounting.wall_clock.time_delta_since_last_accounting(),
                accounting.wall_clock.active_goal_id(),
            )
        };
        if time_delta_seconds == 0 {
            return Ok(None);
        }

        match state_db
            .account_thread_goal_usage(
                thread_id,
                time_delta_seconds,
                /*token_delta*/ 0,
                mode,
                expected_goal_id.as_deref(),
            )
            .await?
        {
            codex_state::ThreadGoalAccountingOutcome::Updated(goal) => {
                state
                    .accounting
                    .lock()
                    .await
                    .wall_clock
                    .mark_accounted(time_delta_seconds);
                Ok(Some(protocol_goal_from_state(goal)))
            }
            codex_state::ThreadGoalAccountingOutcome::Unchanged(goal) => {
                {
                    let mut accounting = state.accounting.lock().await;
                    accounting.wall_clock.reset_baseline();
                    accounting.wall_clock.clear_active_goal();
                }
                Ok(goal.map(protocol_goal_from_state))
            }
        }
    }

    async fn pause_active_goal_for_interrupt(
        &self,
        handle: &SessionRuntimeHandle,
    ) -> anyhow::Result<()> {
        if should_ignore_goal_for_mode(handle.collaboration_mode().await.mode) {
            return Ok(());
        }

        let state = self.state(handle.thread_id()).await;
        let _continuation_guard = state
            .continuation_lock
            .acquire()
            .await
            .context("goal continuation semaphore closed")?;
        let Some(state_db) = handle.state_db_for_persisted_thread().await? else {
            return Ok(());
        };
        self.account_goal_wall_clock_usage(
            handle.thread_id(),
            &state_db,
            codex_state::ThreadGoalAccountingMode::ActiveStatusOnly,
        )
        .await?;
        let Some(goal) = state_db
            .pause_active_thread_goal(handle.thread_id())
            .await?
        else {
            return Ok(());
        };
        let goal = protocol_goal_from_state(goal);
        *state.budget_limit_reported_goal_id.lock().await = None;
        state.accounting.lock().await.wall_clock.clear_active_goal();
        handle
            .emit_event_raw(EventMsg::ThreadGoalUpdated(ThreadGoalUpdatedEvent {
                thread_id: handle.thread_id(),
                turn_id: None,
                goal,
            }))
            .await;
        Ok(())
    }

    async fn activate_paused_goal_after_resume(
        &self,
        handle: &SessionRuntimeHandle,
    ) -> anyhow::Result<bool> {
        if should_ignore_goal_for_mode(handle.collaboration_mode().await.mode) {
            tracing::debug!(
                "skipping paused goal auto-resume while current collaboration mode ignores goals"
            );
            return Ok(false);
        }

        let state = self.state(handle.thread_id()).await;
        let _continuation_guard = state
            .continuation_lock
            .acquire()
            .await
            .context("goal continuation semaphore closed")?;
        let Some(state_db) = handle.state_db_for_persisted_thread().await? else {
            return Ok(false);
        };
        let Some(goal) = state_db.get_thread_goal(handle.thread_id()).await? else {
            *state.budget_limit_reported_goal_id.lock().await = None;
            state.accounting.lock().await.wall_clock.clear_active_goal();
            return Ok(false);
        };
        if goal.status != codex_state::ThreadGoalStatus::Paused {
            let goal_id = goal.goal_id.clone();
            if goal.status == codex_state::ThreadGoalStatus::Active {
                state
                    .accounting
                    .lock()
                    .await
                    .wall_clock
                    .mark_active_goal(goal_id);
            } else {
                state.accounting.lock().await.wall_clock.clear_active_goal();
            }
            return Ok(false);
        }

        let Some(goal) = state_db
            .update_thread_goal(
                handle.thread_id(),
                codex_state::ThreadGoalUpdate {
                    status: Some(codex_state::ThreadGoalStatus::Active),
                    token_budget: None,
                    expected_goal_id: Some(goal.goal_id.clone()),
                },
            )
            .await?
        else {
            *state.budget_limit_reported_goal_id.lock().await = None;
            state.accounting.lock().await.wall_clock.clear_active_goal();
            return Ok(false);
        };
        let goal_id = goal.goal_id.clone();
        let goal = protocol_goal_from_state(goal);
        *state.budget_limit_reported_goal_id.lock().await = None;
        let active_turn_id = handle.active_turn_id().await;
        let current_token_usage = handle.total_token_usage().await.unwrap_or_default();
        self.mark_active_goal_accounting(
            handle.thread_id(),
            goal_id,
            active_turn_id,
            current_token_usage,
        )
        .await;
        handle
            .emit_event_raw(EventMsg::ThreadGoalUpdated(ThreadGoalUpdatedEvent {
                thread_id: handle.thread_id(),
                turn_id: None,
                goal,
            }))
            .await;
        Ok(true)
    }

    async fn maybe_continue_goal_if_idle_runtime(&self, handle: &SessionRuntimeHandle) {
        self.maybe_start_goal_continuation_turn(handle).await;
    }

    async fn maybe_start_goal_continuation_turn(&self, handle: &SessionRuntimeHandle) {
        let state = self.state(handle.thread_id()).await;
        let Ok(_continuation_guard) = state.continuation_lock.acquire().await else {
            tracing::warn!("goal continuation semaphore closed");
            return;
        };
        let Some(candidate) = self.goal_continuation_candidate_if_active(handle).await else {
            return;
        };
        let started = handle
            .try_start_idle_background_turn(candidate.items.clone())
            .await;
        if !started {
            return;
        }

        match handle.state_db_for_persisted_thread().await {
            Ok(Some(state_db)) => match state_db.get_thread_goal(handle.thread_id()).await {
                Ok(Some(goal))
                    if goal.goal_id == candidate.goal_id
                        && goal.status == codex_state::ThreadGoalStatus::Active => {}
                Ok(Some(_)) | Ok(None) => {
                    tracing::debug!(
                        "active goal changed after continuation launch; next idle event will settle state"
                    );
                }
                Err(err) => {
                    tracing::warn!("failed to re-read goal after continuation: {err}");
                }
            },
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("failed to open state db after goal continuation: {err}");
            }
        }
    }

    async fn goal_continuation_candidate_if_active(
        &self,
        handle: &SessionRuntimeHandle,
    ) -> Option<GoalContinuationCandidate> {
        if should_ignore_goal_for_mode(handle.collaboration_mode().await.mode) {
            tracing::debug!("skipping active goal continuation while plan mode is active");
            return None;
        }
        if handle.has_active_turn().await {
            tracing::debug!("skipping active goal continuation because a turn is already active");
            return None;
        }
        if handle.has_queued_response_items_for_next_turn().await {
            tracing::debug!("skipping active goal continuation because queued input exists");
            return None;
        }
        if handle.has_trigger_turn_mailbox_items().await {
            tracing::debug!(
                "skipping active goal continuation because trigger-turn mailbox input is pending"
            );
            return None;
        }
        let state_db = match handle.state_db_for_persisted_thread().await {
            Ok(Some(state_db)) => state_db,
            Ok(None) => {
                tracing::debug!("skipping active goal continuation for ephemeral thread");
                return None;
            }
            Err(err) => {
                tracing::warn!("failed to open state db for goal continuation: {err}");
                return None;
            }
        };
        let goal = match state_db.get_thread_goal(handle.thread_id()).await {
            Ok(Some(goal)) => goal,
            Ok(None) => {
                tracing::debug!("skipping active goal continuation because no goal is set");
                return None;
            }
            Err(err) => {
                tracing::warn!("failed to read goal for continuation: {err}");
                return None;
            }
        };
        if goal.status != codex_state::ThreadGoalStatus::Active {
            tracing::debug!(status = ?goal.status, "skipping inactive goal");
            return None;
        }
        if handle.has_active_turn().await
            || handle.has_queued_response_items_for_next_turn().await
            || handle.has_trigger_turn_mailbox_items().await
        {
            tracing::debug!("skipping active goal continuation because pending work appeared");
            return None;
        }
        let goal_id = goal.goal_id.clone();
        let goal = protocol_goal_from_state(goal);
        Some(GoalContinuationCandidate {
            goal_id,
            items: vec![ResponseInputItem::Message {
                role: "developer".to_string(),
                content: vec![ContentItem::InputText {
                    text: continuation_prompt(&goal),
                }],
                phase: None,
            }],
        })
    }
}
