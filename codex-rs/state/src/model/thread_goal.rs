use anyhow::Result;
use anyhow::anyhow;
use chrono::DateTime;
use chrono::Utc;
use codex_protocol::ThreadId;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::epoch_millis_to_datetime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadGoalStatus {
    Active,
    Paused,
    BudgetLimited,
    Complete,
}

impl ThreadGoalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::BudgetLimited => "budget_limited",
            Self::Complete => "complete",
        }
    }

    pub fn is_active(self) -> bool {
        self == Self::Active
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::BudgetLimited | Self::Complete)
    }
}

impl TryFrom<&str> for ThreadGoalStatus {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "budget_limited" => Ok(Self::BudgetLimited),
            "complete" => Ok(Self::Complete),
            other => Err(anyhow!("unknown thread goal status `{other}`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ThreadGoalBudget {
    Tokens {
        token_budget: i64,
    },
    FiveHourLimitPercent {
        limit_id: String,
        percent: f64,
        baseline_used_percent: f64,
        baseline_resets_at: Option<i64>,
        latest_used_percent: f64,
        latest_resets_at: Option<i64>,
    },
}

impl ThreadGoalBudget {
    pub fn token_budget(&self) -> Option<i64> {
        match self {
            Self::Tokens { token_budget } => Some(*token_budget),
            Self::FiveHourLimitPercent { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadGoal {
    pub thread_id: ThreadId,
    pub goal_id: String,
    pub objective: String,
    pub status: ThreadGoalStatus,
    pub budget: Option<ThreadGoalBudget>,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub(crate) struct ThreadGoalRow {
    pub thread_id: String,
    pub goal_id: String,
    pub objective: String,
    pub status: String,
    pub budget_kind: Option<String>,
    pub budget_limit_id: Option<String>,
    pub budget_percent: Option<f64>,
    pub budget_baseline_used_percent: Option<f64>,
    pub budget_baseline_resets_at: Option<i64>,
    pub budget_latest_used_percent: Option<f64>,
    pub budget_latest_resets_at: Option<i64>,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl ThreadGoalRow {
    pub(crate) fn try_from_row(row: &SqliteRow) -> Result<Self> {
        Ok(Self {
            thread_id: row.try_get("thread_id")?,
            goal_id: row.try_get("goal_id")?,
            objective: row.try_get("objective")?,
            status: row.try_get("status")?,
            budget_kind: row.try_get("budget_kind")?,
            budget_limit_id: row.try_get("budget_limit_id")?,
            budget_percent: row.try_get("budget_percent")?,
            budget_baseline_used_percent: row.try_get("budget_baseline_used_percent")?,
            budget_baseline_resets_at: row.try_get("budget_baseline_resets_at")?,
            budget_latest_used_percent: row.try_get("budget_latest_used_percent")?,
            budget_latest_resets_at: row.try_get("budget_latest_resets_at")?,
            token_budget: row.try_get("token_budget")?,
            tokens_used: row.try_get("tokens_used")?,
            time_used_seconds: row.try_get("time_used_seconds")?,
            created_at_ms: row.try_get("created_at_ms")?,
            updated_at_ms: row.try_get("updated_at_ms")?,
        })
    }
}

impl TryFrom<ThreadGoalRow> for ThreadGoal {
    type Error = anyhow::Error;

    fn try_from(row: ThreadGoalRow) -> Result<Self> {
        let budget = thread_goal_budget_from_row(&row)?;
        let token_budget = budget
            .as_ref()
            .and_then(ThreadGoalBudget::token_budget)
            .or(row.token_budget);
        Ok(Self {
            thread_id: ThreadId::try_from(row.thread_id)?,
            goal_id: row.goal_id,
            objective: row.objective,
            status: ThreadGoalStatus::try_from(row.status.as_str())?,
            budget,
            token_budget,
            tokens_used: row.tokens_used,
            time_used_seconds: row.time_used_seconds,
            created_at: epoch_millis_to_datetime(row.created_at_ms)?,
            updated_at: epoch_millis_to_datetime(row.updated_at_ms)?,
        })
    }
}

fn thread_goal_budget_from_row(row: &ThreadGoalRow) -> Result<Option<ThreadGoalBudget>> {
    match row.budget_kind.as_deref() {
        Some("tokens") => {
            let token_budget = row
                .token_budget
                .ok_or_else(|| anyhow!("token goal budget missing token_budget"))?;
            Ok(Some(ThreadGoalBudget::Tokens { token_budget }))
        }
        Some("five_hour_limit_percent") => {
            let limit_id = row
                .budget_limit_id
                .clone()
                .ok_or_else(|| anyhow!("5h goal budget missing budget_limit_id"))?;
            let percent = row
                .budget_percent
                .ok_or_else(|| anyhow!("5h goal budget missing budget_percent"))?;
            let baseline_used_percent = row
                .budget_baseline_used_percent
                .ok_or_else(|| anyhow!("5h goal budget missing budget_baseline_used_percent"))?;
            let latest_used_percent = row
                .budget_latest_used_percent
                .unwrap_or(baseline_used_percent);
            Ok(Some(ThreadGoalBudget::FiveHourLimitPercent {
                limit_id,
                percent,
                baseline_used_percent,
                baseline_resets_at: row.budget_baseline_resets_at,
                latest_used_percent,
                latest_resets_at: row
                    .budget_latest_resets_at
                    .or(row.budget_baseline_resets_at),
            }))
        }
        Some(other) => Err(anyhow!("unknown thread goal budget kind `{other}`")),
        None => Ok(row
            .token_budget
            .map(|token_budget| ThreadGoalBudget::Tokens { token_budget })),
    }
}
