use super::GoalRuntime;
use super::prompts::budget_limit_prompt;
use super::prompts::continuation_prompt;
use super::prompts::goal_token_delta_for_usage;
use super::prompts::should_ignore_goal_for_mode;
use super::tools::CompletionBudgetReport;
use super::tools::GoalToolResponse;
use super::tools::validate_update_goal_status;
use codex_core::SessionRuntimeExtension;
use codex_core::SessionToolError;
use codex_core::SessionToolSpecContext;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ModeKind;
use codex_protocol::protocol::ThreadGoal;
use codex_protocol::protocol::ThreadGoalStatus;
use codex_protocol::protocol::TokenUsage;
use codex_tools::CREATE_GOAL_TOOL_NAME;
use codex_tools::GET_GOAL_TOOL_NAME;
use codex_tools::ToolSpec;
use codex_tools::UPDATE_GOAL_TOOL_NAME;
use pretty_assertions::assert_eq;

#[test]
fn goal_runtime_exposes_goal_tools_for_persisted_threads_only() {
    let runtime = GoalRuntime::new();
    let persisted_specs = runtime.tool_specs(SessionToolSpecContext {
        mode: ModeKind::Default,
        ephemeral: false,
    });
    let ephemeral_specs = runtime.tool_specs(SessionToolSpecContext {
        mode: ModeKind::Default,
        ephemeral: true,
    });

    let names = persisted_specs
        .iter()
        .map(ToolSpec::name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            GET_GOAL_TOOL_NAME,
            CREATE_GOAL_TOOL_NAME,
            UPDATE_GOAL_TOOL_NAME,
        ],
    );
    assert!(ephemeral_specs.is_empty());
}

#[test]
fn update_goal_status_policy_only_accepts_complete() {
    assert!(validate_update_goal_status(ThreadGoalStatus::Complete).is_ok());

    let err = validate_update_goal_status(ThreadGoalStatus::Paused)
        .expect_err("paused should not be accepted from update_goal");

    let SessionToolError::RespondToModel(message) = err else {
        panic!("expected model-facing update_goal rejection");
    };
    assert_eq!(
        message,
        "update_goal can only mark the existing goal complete; pause, resume, and budget-limited status changes are controlled by the user or system",
    );
}

#[test]
fn completed_budgeted_goal_response_reports_final_usage() {
    let goal = ThreadGoal {
        thread_id: ThreadId::new(),
        objective: "Keep optimizing".to_string(),
        status: ThreadGoalStatus::Complete,
        token_budget: Some(10_000),
        tokens_used: 3_250,
        time_used_seconds: 75,
        created_at: 1,
        updated_at: 2,
    };

    let response = GoalToolResponse::new(Some(goal.clone()), CompletionBudgetReport::Include);

    assert_eq!(
        response,
        GoalToolResponse {
            goal: Some(goal),
            remaining_tokens: Some(6_750),
            completion_budget_report: Some(
                "Goal achieved. Report final budget usage to the user: tokens used: 3250 of 10000; time used: 75 seconds."
                    .to_string()
            ),
        }
    );
}

#[test]
fn completed_unbudgeted_goal_response_omits_budget_report() {
    let goal = ThreadGoal {
        thread_id: ThreadId::new(),
        objective: "Write a poem".to_string(),
        status: ThreadGoalStatus::Complete,
        token_budget: None,
        tokens_used: 250,
        time_used_seconds: 0,
        created_at: 1,
        updated_at: 2,
    };

    let response = GoalToolResponse::new(Some(goal.clone()), CompletionBudgetReport::Include);

    assert_eq!(
        response,
        GoalToolResponse {
            goal: Some(goal),
            remaining_tokens: None,
            completion_budget_report: None,
        }
    );
}

#[test]
fn goal_continuation_is_ignored_only_in_plan_mode() {
    assert!(should_ignore_goal_for_mode(ModeKind::Plan));
    assert!(!should_ignore_goal_for_mode(ModeKind::Default));
    assert!(!should_ignore_goal_for_mode(ModeKind::PairProgramming));
    assert!(!should_ignore_goal_for_mode(ModeKind::Execute));
}

#[test]
fn goal_usage_ignores_cached_input_tokens() {
    let usage = TokenUsage {
        input_tokens: 10,
        cached_input_tokens: 7,
        output_tokens: 4,
        reasoning_output_tokens: 3,
        total_tokens: 17,
    };

    assert_eq!(goal_token_delta_for_usage(&usage), 7);
}

#[test]
fn prompts_escape_goal_objective() {
    let goal = ThreadGoal {
        thread_id: ThreadId::new(),
        objective: "ship <fast> & safe".to_string(),
        status: ThreadGoalStatus::Active,
        token_budget: Some(100),
        tokens_used: 10,
        time_used_seconds: 20,
        created_at: 1,
        updated_at: 2,
    };

    let continuation = continuation_prompt(&goal);
    let budget_limit = budget_limit_prompt(&goal);

    assert!(continuation.contains("ship &lt;fast&gt; &amp; safe"));
    assert!(budget_limit.contains("ship &lt;fast&gt; &amp; safe"));
    assert!(!continuation.contains("ship <fast> & safe"));
    assert!(!budget_limit.contains("ship <fast> & safe"));
}
