ALTER TABLE thread_goals ADD COLUMN budget_kind TEXT CHECK(budget_kind IN ('tokens', 'five_hour_limit_percent'));
ALTER TABLE thread_goals ADD COLUMN budget_limit_id TEXT;
ALTER TABLE thread_goals ADD COLUMN budget_percent REAL;
ALTER TABLE thread_goals ADD COLUMN budget_baseline_used_percent REAL;
ALTER TABLE thread_goals ADD COLUMN budget_baseline_resets_at INTEGER;
ALTER TABLE thread_goals ADD COLUMN budget_latest_used_percent REAL;
ALTER TABLE thread_goals ADD COLUMN budget_latest_resets_at INTEGER;

UPDATE thread_goals
SET budget_kind = 'tokens'
WHERE token_budget IS NOT NULL;
