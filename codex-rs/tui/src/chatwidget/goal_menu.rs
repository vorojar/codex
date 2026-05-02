//! Goal summary for the bare `/goal` command.

use super::*;
use crate::app_event::AppEvent;
use crate::app_event::ThreadGoalSetMode;
use crate::bottom_pane::GoalSetupView;
use crate::goal_display::format_five_hour_goal_progress;
use crate::goal_display::format_goal_elapsed_seconds;
use crate::status::format_tokens_compact;
use codex_app_server_protocol::ThreadGoalBudgetParams;

impl ChatWidget {
    pub(crate) fn show_goal_summary(&mut self, goal: AppThreadGoal) {
        self.add_plain_history_lines(goal_summary_lines(&goal));
    }

    pub(crate) fn show_goal_setup(&mut self, thread_id: ThreadId) {
        let tx = self.app_event_tx.clone();
        let view = GoalSetupView::new(Box::new(
            move |objective: String, budget: Option<ThreadGoalBudgetParams>| {
                tx.send(AppEvent::SetThreadGoalObjective {
                    thread_id,
                    objective,
                    budget,
                    mode: ThreadGoalSetMode::ConfirmIfExists,
                });
            },
        ));
        self.bottom_pane.show_view(Box::new(view));
    }

    pub(crate) fn on_thread_goal_cleared(&mut self, thread_id: &str) {
        if self
            .thread_id
            .is_some_and(|active_thread_id| active_thread_id.to_string() == thread_id)
        {
            self.current_goal_status = None;
            self.update_collaboration_mode_indicator();
        }
    }
}

fn goal_summary_lines(goal: &AppThreadGoal) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from("Goal".bold()),
        Line::from(vec![
            "Status: ".dim(),
            goal_status_label(goal.status).to_string().into(),
        ]),
        Line::from(vec!["Objective: ".dim(), goal.objective.clone().into()]),
        Line::from(vec![
            "Time used: ".dim(),
            format_goal_elapsed_seconds(goal.time_used_seconds).into(),
        ]),
        Line::from(vec![
            "Tokens used: ".dim(),
            format_tokens_compact(goal.tokens_used).into(),
        ]),
    ];
    if let Some(token_budget) = goal.token_budget {
        lines.push(Line::from(vec![
            "Token budget: ".dim(),
            format_tokens_compact(token_budget).into(),
        ]));
    } else if let Some(progress) = goal
        .budget
        .as_ref()
        .and_then(format_five_hour_goal_progress)
    {
        lines.push(Line::from(vec!["5h limit: ".dim(), progress.into()]));
    }
    let command_hint = match goal.status {
        AppThreadGoalStatus::Active => "Commands: /goal pause, /goal clear",
        AppThreadGoalStatus::Paused => "Commands: /goal resume, /goal clear",
        AppThreadGoalStatus::BudgetLimited | AppThreadGoalStatus::Complete => {
            "Commands: /goal clear"
        }
    };
    lines.push(Line::default());
    lines.push(Line::from(command_hint.dim()));
    lines
}

fn goal_status_label(status: AppThreadGoalStatus) -> &'static str {
    match status {
        AppThreadGoalStatus::Active => "active",
        AppThreadGoalStatus::Paused => "paused",
        AppThreadGoalStatus::BudgetLimited => "limited by budget",
        AppThreadGoalStatus::Complete => "complete",
    }
}
