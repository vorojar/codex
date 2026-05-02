//! Model-facing goal tool handling for the app-server runtime extension.

use super::GoalRuntime;
use super::prompts::completion_budget_report;
use super::prompts::protocol_goal_from_state;
use super::prompts::state_goal_status_from_protocol;
use super::prompts::validate_goal_budget;
use super::state::BudgetLimitSteering;
use codex_core::SessionRuntimeHandle;
use codex_core::SessionToolError;
use codex_core::SessionToolInvocation;
use codex_core::SessionToolOutput;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadGoal;
use codex_protocol::protocol::ThreadGoalStatus;
use codex_protocol::protocol::ThreadGoalUpdatedEvent;
use codex_protocol::protocol::validate_thread_goal_objective as validate_goal_objective;
use codex_tools::CREATE_GOAL_TOOL_NAME;
use codex_tools::GET_GOAL_TOOL_NAME;
use codex_tools::UPDATE_GOAL_TOOL_NAME;
use serde::Deserialize;
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct CreateGoalArgs {
    objective: String,
    token_budget: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct UpdateGoalArgs {
    status: ThreadGoalStatus,
}

struct SetGoalRequest {
    objective: Option<String>,
    status: Option<ThreadGoalStatus>,
    token_budget: Option<Option<i64>>,
}

struct CreateGoalRequest {
    objective: String,
    token_budget: Option<i64>,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GoalToolResponse {
    pub(super) goal: Option<ThreadGoal>,
    pub(super) remaining_tokens: Option<i64>,
    pub(super) completion_budget_report: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) enum CompletionBudgetReport {
    Include,
    Omit,
}

impl GoalRuntime {
    async fn get_goal(&self, handle: &SessionRuntimeHandle) -> anyhow::Result<Option<ThreadGoal>> {
        let state_db = self.require_state_db(handle).await?;
        state_db
            .get_thread_goal(handle.thread_id())
            .await
            .map(|goal| goal.map(protocol_goal_from_state))
    }

    async fn create_goal(
        &self,
        handle: &SessionRuntimeHandle,
        turn_id: String,
        request: CreateGoalRequest,
    ) -> anyhow::Result<ThreadGoal> {
        let CreateGoalRequest {
            objective,
            token_budget,
        } = request;
        validate_goal_budget(token_budget)?;
        let objective = objective.trim();
        validate_goal_objective(objective).map_err(anyhow::Error::msg)?;

        let state_db = self.require_state_db(handle).await?;
        self.account_goal_wall_clock_usage(
            handle.thread_id(),
            &state_db,
            codex_state::ThreadGoalAccountingMode::ActiveOnly,
        )
        .await?;
        let goal = state_db
            .insert_thread_goal(
                handle.thread_id(),
                objective,
                codex_state::ThreadGoalStatus::Active,
                token_budget,
            )
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "cannot create a new goal because thread {} already has a goal",
                    handle.thread_id()
                )
            })?;

        let goal_id = goal.goal_id.clone();
        let goal = protocol_goal_from_state(goal);
        let state = self.state(handle.thread_id()).await;
        *state.budget_limit_reported_goal_id.lock().await = None;

        let current_token_usage = handle.total_token_usage().await.unwrap_or_default();
        self.mark_active_goal_accounting(
            handle.thread_id(),
            goal_id,
            Some(turn_id.clone()),
            current_token_usage,
        )
        .await;

        handle
            .emit_event_raw(EventMsg::ThreadGoalUpdated(ThreadGoalUpdatedEvent {
                thread_id: handle.thread_id(),
                turn_id: Some(turn_id),
                goal: goal.clone(),
            }))
            .await;
        Ok(goal)
    }

    async fn set_goal(
        &self,
        handle: &SessionRuntimeHandle,
        turn_id: String,
        request: SetGoalRequest,
    ) -> anyhow::Result<ThreadGoal> {
        let SetGoalRequest {
            objective,
            status,
            token_budget,
        } = request;
        validate_goal_budget(token_budget.flatten())?;
        let state_db = self.require_state_db(handle).await?;
        let objective = objective.map(|objective| objective.trim().to_string());
        if let Some(objective) = objective.as_deref()
            && let Err(err) = validate_goal_objective(objective)
        {
            anyhow::bail!("{err}");
        }

        self.account_goal_wall_clock_usage(
            handle.thread_id(),
            &state_db,
            codex_state::ThreadGoalAccountingMode::ActiveOnly,
        )
        .await?;
        let mut replacing_goal = objective.is_some();
        let previous_status;
        let goal = if let Some(objective) = objective.as_deref() {
            let existing_goal = state_db.get_thread_goal(handle.thread_id()).await?;
            previous_status = existing_goal.as_ref().map(|goal| goal.status);
            let same_nonterminal_goal = existing_goal.as_ref().is_some_and(|goal| {
                goal.objective == objective
                    && goal.status != codex_state::ThreadGoalStatus::Complete
            });
            if same_nonterminal_goal {
                replacing_goal = false;
                state_db
                    .update_thread_goal(
                        handle.thread_id(),
                        codex_state::ThreadGoalUpdate {
                            status: status
                                .map(state_goal_status_from_protocol)
                                .or(Some(codex_state::ThreadGoalStatus::Active)),
                            token_budget,
                            expected_goal_id: existing_goal
                                .as_ref()
                                .map(|goal| goal.goal_id.clone()),
                        },
                    )
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "cannot update goal for thread {}: no goal exists",
                            handle.thread_id()
                        )
                    })?
            } else {
                state_db
                    .replace_thread_goal(
                        handle.thread_id(),
                        objective,
                        status
                            .map(state_goal_status_from_protocol)
                            .unwrap_or(codex_state::ThreadGoalStatus::Active),
                        token_budget.flatten(),
                    )
                    .await?
            }
        } else {
            let existing_goal = state_db.get_thread_goal(handle.thread_id()).await?;
            previous_status = existing_goal.as_ref().map(|goal| goal.status);
            let expected_goal_id = existing_goal.map(|goal| goal.goal_id);
            state_db
                .update_thread_goal(
                    handle.thread_id(),
                    codex_state::ThreadGoalUpdate {
                        status: status.map(state_goal_status_from_protocol),
                        token_budget,
                        expected_goal_id,
                    },
                )
                .await?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "cannot update goal for thread {}: no goal exists",
                        handle.thread_id()
                    )
                })?
        };

        let goal_status = goal.status;
        let goal_id = goal.goal_id.clone();
        let goal = protocol_goal_from_state(goal);
        let state = self.state(handle.thread_id()).await;
        *state.budget_limit_reported_goal_id.lock().await = None;
        let newly_active_goal = goal_status == codex_state::ThreadGoalStatus::Active
            && (replacing_goal
                || previous_status
                    .is_some_and(|status| status != codex_state::ThreadGoalStatus::Active));
        if newly_active_goal {
            let current_token_usage = handle.total_token_usage().await.unwrap_or_default();
            self.mark_active_goal_accounting(
                handle.thread_id(),
                goal_id,
                Some(turn_id.clone()),
                current_token_usage,
            )
            .await;
        } else if goal_status != codex_state::ThreadGoalStatus::Active {
            self.clear_active_goal_accounting(handle.thread_id(), turn_id.as_str())
                .await;
        }
        handle
            .emit_event_raw(EventMsg::ThreadGoalUpdated(ThreadGoalUpdatedEvent {
                thread_id: handle.thread_id(),
                turn_id: Some(turn_id),
                goal: goal.clone(),
            }))
            .await;
        Ok(goal)
    }

    pub(super) async fn handle_tool(
        &self,
        handle: SessionRuntimeHandle,
        invocation: SessionToolInvocation,
    ) -> Result<SessionToolOutput, SessionToolError> {
        match invocation.tool_name.name.as_str() {
            GET_GOAL_TOOL_NAME => {
                let goal = self
                    .get_goal(&handle)
                    .await
                    .map_err(|err| SessionToolError::RespondToModel(format_goal_error(err)))?;
                self.goal_response(goal, CompletionBudgetReport::Omit)
            }
            CREATE_GOAL_TOOL_NAME => {
                let args: CreateGoalArgs = parse_arguments(&invocation.arguments)?;
                let goal = self
                    .create_goal(
                        &handle,
                        invocation.turn_id,
                        CreateGoalRequest {
                            objective: args.objective,
                            token_budget: args.token_budget,
                        },
                    )
                    .await
                    .map_err(|err| {
                        if err
                            .chain()
                            .any(|cause| cause.to_string().contains("already has a goal"))
                        {
                            SessionToolError::RespondToModel(
                                "cannot create a new goal because this thread already has a goal; use update_goal only when the existing goal is complete"
                                    .to_string(),
                            )
                        } else {
                            SessionToolError::RespondToModel(format_goal_error(err))
                        }
                    })?;
                self.goal_response(Some(goal), CompletionBudgetReport::Omit)
            }
            UPDATE_GOAL_TOOL_NAME => {
                let args: UpdateGoalArgs = parse_arguments(&invocation.arguments)?;
                validate_update_goal_status(args.status)?;
                self.account_goal_progress(
                    &handle,
                    invocation.turn_id.as_str(),
                    BudgetLimitSteering::Suppressed,
                )
                .await
                .map_err(|err| SessionToolError::RespondToModel(format_goal_error(err)))?;
                let goal = self
                    .set_goal(
                        &handle,
                        invocation.turn_id,
                        SetGoalRequest {
                            objective: None,
                            status: Some(ThreadGoalStatus::Complete),
                            token_budget: None,
                        },
                    )
                    .await
                    .map_err(|err| SessionToolError::RespondToModel(format_goal_error(err)))?;
                self.goal_response(Some(goal), CompletionBudgetReport::Include)
            }
            other => Err(SessionToolError::Fatal(format!(
                "goal runtime received unsupported tool: {other}"
            ))),
        }
    }

    fn goal_response(
        &self,
        goal: Option<ThreadGoal>,
        completion_budget_report: CompletionBudgetReport,
    ) -> Result<SessionToolOutput, SessionToolError> {
        let response =
            serde_json::to_string_pretty(&GoalToolResponse::new(goal, completion_budget_report))
                .map_err(|err| SessionToolError::Fatal(err.to_string()))?;
        Ok(SessionToolOutput::from_text(response, Some(true)))
    }
}

impl GoalToolResponse {
    pub(super) fn new(goal: Option<ThreadGoal>, report_mode: CompletionBudgetReport) -> Self {
        let remaining_tokens = goal.as_ref().and_then(|goal| {
            goal.token_budget
                .map(|budget| (budget - goal.tokens_used).max(0))
        });
        let completion_budget_report = match report_mode {
            CompletionBudgetReport::Include => goal
                .as_ref()
                .filter(|goal| goal.status == ThreadGoalStatus::Complete)
                .and_then(completion_budget_report),
            CompletionBudgetReport::Omit => None,
        };
        Self {
            goal,
            remaining_tokens,
            completion_budget_report,
        }
    }
}

fn parse_arguments<T: for<'de> Deserialize<'de>>(arguments: &str) -> Result<T, SessionToolError> {
    serde_json::from_str(arguments)
        .map_err(|err| SessionToolError::RespondToModel(format!("invalid goal arguments: {err}")))
}

pub(super) fn validate_update_goal_status(
    status: ThreadGoalStatus,
) -> Result<(), SessionToolError> {
    if status == ThreadGoalStatus::Complete {
        return Ok(());
    }
    Err(SessionToolError::RespondToModel(
        "update_goal can only mark the existing goal complete; pause, resume, and budget-limited status changes are controlled by the user or system"
            .to_string(),
    ))
}

fn format_goal_error(err: anyhow::Error) -> String {
    let mut message = err.to_string();
    for cause in err.chain().skip(1) {
        let _ = write!(message, ": {cause}");
    }
    message
}
