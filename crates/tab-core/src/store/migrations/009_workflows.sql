-- Workflows: a trigger and steps, stored as JSON. Every save keeps the
-- previous definition, so a change an agent made can be looked at and undone.
CREATE TABLE workflows (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 0,
    definition  TEXT NOT NULL,
    version     INTEGER NOT NULL,
    created_by  TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE workflow_versions (
    workflow_id TEXT NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    version     INTEGER NOT NULL,
    definition  TEXT NOT NULL,
    saved_by    TEXT NOT NULL,
    saved_at    TEXT NOT NULL,
    PRIMARY KEY (workflow_id, version)
);

-- One row per run with each step's inputs and outputs. `trigger_key` makes
-- the same trigger run a workflow once.
CREATE TABLE workflow_runs (
    id            TEXT PRIMARY KEY,
    workflow_id   TEXT NOT NULL,
    version       INTEGER NOT NULL,
    status        TEXT NOT NULL,
    dry_run       INTEGER NOT NULL DEFAULT 0,
    trigger_key   TEXT,
    item_id       TEXT,
    body          TEXT NOT NULL,
    started_at    TEXT NOT NULL,
    finished_at   TEXT
);
CREATE INDEX workflow_runs_by_workflow ON workflow_runs (workflow_id, started_at);
CREATE INDEX workflow_runs_by_time ON workflow_runs (started_at);
CREATE UNIQUE INDEX workflow_runs_dedup ON workflow_runs (workflow_id, trigger_key) WHERE trigger_key IS NOT NULL;
