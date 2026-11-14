-- Daily workflow rollups: what one version of a workflow's runs added up
-- to on one UTC day (`day` is YYYY-MM-DD), as JSON
-- (`tab_domain::workflow::rollup::DayRollup`). Kept 13 months, so trends
-- outlive the 30-day run log. Synced; every device derives the same body
-- from the same runs.
CREATE TABLE workflow_rollups (
    workflow_id TEXT NOT NULL,
    version     INTEGER NOT NULL,
    day         TEXT NOT NULL,
    body        TEXT NOT NULL,
    PRIMARY KEY (workflow_id, version, day)
);
CREATE INDEX workflow_rollups_by_day ON workflow_rollups (day);
