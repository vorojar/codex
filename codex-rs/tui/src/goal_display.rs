use crate::status::format_tokens_compact;
use codex_app_server_protocol::ThreadGoal;
use codex_app_server_protocol::ThreadGoalBudget;
use codex_app_server_protocol::ThreadGoalStatus;

pub(crate) fn format_goal_elapsed_seconds(seconds: i64) -> String {
    let seconds = seconds.max(0) as u64;
    if seconds < 60 {
        return format!("{seconds}s");
    }

    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }

    let hours = minutes / 60;
    let remaining_minutes = minutes % 60;
    if hours >= 24 {
        let days = hours / 24;
        let remaining_hours = hours % 24;
        return format!("{days}d {remaining_hours}h {remaining_minutes}m");
    }

    if remaining_minutes == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {remaining_minutes}m")
    }
}

pub(crate) fn goal_status_label(status: ThreadGoalStatus) -> &'static str {
    match status {
        ThreadGoalStatus::Active => "active",
        ThreadGoalStatus::Paused => "paused",
        ThreadGoalStatus::BudgetLimited => "limited by budget",
        ThreadGoalStatus::Complete => "complete",
    }
}

pub(crate) fn goal_usage_summary(goal: &ThreadGoal) -> String {
    let mut parts = vec![format!("Objective: {}", goal.objective)];
    if goal.time_used_seconds > 0 {
        parts.push(format!(
            "Time: {}.",
            format_goal_elapsed_seconds(goal.time_used_seconds)
        ));
    }
    if let Some(token_budget) = goal.token_budget {
        parts.push(format!(
            "Tokens: {}/{}.",
            format_tokens_compact(goal.tokens_used),
            format_tokens_compact(token_budget)
        ));
    } else if let Some(ThreadGoalBudget::FiveHourLimitPercent {
        percent,
        baseline_used_percent,
        latest_used_percent,
        baseline_resets_at,
        ..
    }) = &goal.budget
    {
        parts.push(format!(
            "5h limit: {} / {}.",
            format_percent_points(latest_used_percent - baseline_used_percent),
            format_percent_points(*percent)
        ));
        if let Some(resets_at) = baseline_resets_at {
            parts.push(format!(
                "Started at {}; limit at {}; resets {}.",
                format_percent(*baseline_used_percent),
                format_percent(baseline_used_percent + percent),
                format_unix_date(*resets_at)
            ));
        }
    }
    parts.join(" ")
}

pub(crate) fn format_five_hour_goal_progress(budget: &ThreadGoalBudget) -> Option<String> {
    match budget {
        ThreadGoalBudget::FiveHourLimitPercent {
            percent,
            baseline_used_percent,
            latest_used_percent,
            ..
        } => Some(format!(
            "{} / {}",
            format_percent_points(latest_used_percent - baseline_used_percent),
            format_percent_points(*percent)
        )),
        ThreadGoalBudget::Tokens { .. } => None,
    }
}

fn format_percent_points(value: f64) -> String {
    format!("+{}pp", format_percent_number(value.max(0.0)))
}

fn format_percent(value: f64) -> String {
    format!("{}%", format_percent_number(value))
}

fn format_percent_number(value: f64) -> String {
    if (value.fract()).abs() < 0.05 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn format_unix_date(value: i64) -> String {
    chrono::DateTime::from_timestamp(value, 0)
        .map(|date| date.format("%b %-d, %Y").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::ThreadGoal;
    use codex_app_server_protocol::ThreadGoalStatus;
    use pretty_assertions::assert_eq;

    #[test]
    fn format_goal_elapsed_seconds_is_compact() {
        assert_eq!(format_goal_elapsed_seconds(/*seconds*/ 0), "0s");
        assert_eq!(format_goal_elapsed_seconds(/*seconds*/ 59), "59s");
        assert_eq!(format_goal_elapsed_seconds(/*seconds*/ 60), "1m");
        assert_eq!(format_goal_elapsed_seconds(30 * 60), "30m");
        assert_eq!(format_goal_elapsed_seconds(90 * 60), "1h 30m");
        assert_eq!(format_goal_elapsed_seconds(2 * 60 * 60), "2h");
        let just_before_one_day = 24 * 60 * 60 - 1;
        assert_eq!(format_goal_elapsed_seconds(just_before_one_day), "23h 59m");

        let one_day = 24 * 60 * 60;
        assert_eq!(format_goal_elapsed_seconds(one_day), "1d 0h 0m");

        let almost_three_days = 2 * 24 * 60 * 60 + 23 * 60 * 60 + 42 * 60;
        assert_eq!(format_goal_elapsed_seconds(almost_three_days), "2d 23h 42m");
    }

    fn test_thread_goal(token_budget: Option<i64>, tokens_used: i64) -> ThreadGoal {
        ThreadGoal {
            thread_id: "thread-1".to_string(),
            objective: "Complete the task described in ../gameboy-long-running-prompt5.txt"
                .to_string(),
            status: ThreadGoalStatus::BudgetLimited,
            budget: token_budget.map(|token_budget| ThreadGoalBudget::Tokens { token_budget }),
            token_budget,
            tokens_used,
            time_used_seconds: 120,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn goal_usage_summary_formats_time_and_budgeted_tokens() {
        assert_eq!(
            goal_usage_summary(&test_thread_goal(
                /*token_budget*/ Some(50_000),
                /*tokens_used*/ 63_876,
            )),
            "Objective: Complete the task described in ../gameboy-long-running-prompt5.txt Time: 2m. Tokens: 63.9K/50K."
        );
    }
}
