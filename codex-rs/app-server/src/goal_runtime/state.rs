//! Per-thread in-memory state used by the app-server goal runtime.

use super::prompts::goal_token_delta_for_usage;
use anyhow::Context;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::protocol::TokenUsage;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio::sync::Semaphore;
use tokio::sync::SemaphorePermit;

#[derive(Clone, Copy)]
pub(super) enum BudgetLimitSteering {
    Allowed,
    Suppressed,
}

pub(super) struct GoalRuntimeState {
    pub(super) budget_limit_reported_goal_id: Mutex<Option<String>>,
    pub(super) accounting_lock: Semaphore,
    pub(super) accounting: Mutex<GoalAccountingSnapshot>,
    pub(super) continuation_lock: Semaphore,
}

#[derive(Debug)]
pub(super) struct GoalAccountingSnapshot {
    pub(super) turn: Option<GoalTurnAccountingSnapshot>,
    pub(super) wall_clock: GoalWallClockAccountingSnapshot,
}

#[derive(Debug)]
pub(super) struct GoalTurnAccountingSnapshot {
    pub(super) turn_id: String,
    last_accounted_token_usage: TokenUsage,
    active_goal_id: Option<String>,
}

#[derive(Debug)]
pub(super) struct GoalWallClockAccountingSnapshot {
    last_accounted_at: Instant,
    active_goal_id: Option<String>,
}

pub(super) struct GoalContinuationCandidate {
    pub(super) goal_id: String,
    pub(super) items: Vec<ResponseInputItem>,
}

impl GoalRuntimeState {
    pub(super) fn new() -> Self {
        Self {
            budget_limit_reported_goal_id: Mutex::new(None),
            accounting_lock: Semaphore::new(/*permits*/ 1),
            accounting: Mutex::new(GoalAccountingSnapshot::new()),
            continuation_lock: Semaphore::new(/*permits*/ 1),
        }
    }

    pub(super) async fn accounting_permit(&self) -> anyhow::Result<SemaphorePermit<'_>> {
        self.accounting_lock
            .acquire()
            .await
            .context("goal accounting semaphore closed")
    }
}

impl GoalAccountingSnapshot {
    fn new() -> Self {
        Self {
            turn: None,
            wall_clock: GoalWallClockAccountingSnapshot::new(),
        }
    }
}

impl GoalTurnAccountingSnapshot {
    pub(super) fn new(turn_id: impl Into<String>, token_usage: TokenUsage) -> Self {
        Self {
            turn_id: turn_id.into(),
            last_accounted_token_usage: token_usage,
            active_goal_id: None,
        }
    }

    pub(super) fn mark_active_goal(&mut self, goal_id: impl Into<String>) {
        self.active_goal_id = Some(goal_id.into());
    }

    pub(super) fn active_this_turn(&self) -> bool {
        self.active_goal_id.is_some()
    }

    pub(super) fn active_goal_id(&self) -> Option<String> {
        self.active_goal_id.clone()
    }

    pub(super) fn clear_active_goal(&mut self) {
        self.active_goal_id = None;
    }

    pub(super) fn reset_baseline(&mut self, token_usage: TokenUsage) {
        self.last_accounted_token_usage = token_usage;
    }

    pub(super) fn token_delta_since_last_accounting(&self, current: &TokenUsage) -> i64 {
        let last = &self.last_accounted_token_usage;
        let delta = TokenUsage {
            input_tokens: current.input_tokens.saturating_sub(last.input_tokens),
            cached_input_tokens: current
                .cached_input_tokens
                .saturating_sub(last.cached_input_tokens),
            output_tokens: current.output_tokens.saturating_sub(last.output_tokens),
            reasoning_output_tokens: current
                .reasoning_output_tokens
                .saturating_sub(last.reasoning_output_tokens),
            total_tokens: current.total_tokens.saturating_sub(last.total_tokens),
        };
        goal_token_delta_for_usage(&delta)
    }

    pub(super) fn mark_accounted(&mut self, current: TokenUsage) {
        self.last_accounted_token_usage = current;
    }
}

impl GoalWallClockAccountingSnapshot {
    fn new() -> Self {
        Self {
            last_accounted_at: Instant::now(),
            active_goal_id: None,
        }
    }

    pub(super) fn time_delta_since_last_accounting(&self) -> i64 {
        i64::try_from(self.last_accounted_at.elapsed().as_secs()).unwrap_or(i64::MAX)
    }

    pub(super) fn mark_accounted(&mut self, accounted_seconds: i64) {
        if accounted_seconds <= 0 {
            return;
        }
        let advance = Duration::from_secs(u64::try_from(accounted_seconds).unwrap_or(u64::MAX));
        self.last_accounted_at = self
            .last_accounted_at
            .checked_add(advance)
            .unwrap_or_else(Instant::now);
    }

    pub(super) fn reset_baseline(&mut self) {
        self.last_accounted_at = Instant::now();
    }

    pub(super) fn mark_active_goal(&mut self, goal_id: impl Into<String>) {
        let goal_id = goal_id.into();
        if self.active_goal_id.as_deref() != Some(goal_id.as_str()) {
            self.reset_baseline();
            self.active_goal_id = Some(goal_id);
        }
    }

    pub(super) fn clear_active_goal(&mut self) {
        self.active_goal_id = None;
        self.reset_baseline();
    }

    pub(super) fn active_goal_id(&self) -> Option<String> {
        self.active_goal_id.clone()
    }
}
