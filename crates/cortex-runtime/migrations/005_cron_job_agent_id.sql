-- 005_cron_job_agent_id.sql
-- Add target agent_id to cron_jobs and correlated execution run_id to cron_job_runs

ALTER TABLE cron_jobs ADD COLUMN agent_id TEXT;
ALTER TABLE cron_job_runs ADD COLUMN run_id TEXT;

CREATE INDEX IF NOT EXISTS idx_cron_job_runs_run_id ON cron_job_runs(run_id);
