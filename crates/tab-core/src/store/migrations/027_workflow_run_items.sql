-- Workflow status on queue items: an item's runs, runs still going, and the
-- jobs of a run, without scanning every run.
CREATE INDEX workflow_runs_by_item ON workflow_runs (item_id, workflow_id, started_at) WHERE item_id IS NOT NULL;
CREATE INDEX workflow_runs_by_status ON workflow_runs (status, started_at);
CREATE INDEX jobs_by_run ON jobs (run_id);
