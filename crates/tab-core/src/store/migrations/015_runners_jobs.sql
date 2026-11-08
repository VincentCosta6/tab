-- Work handed from a workflow to a runner (an agent session on one of the
-- person's computers, or a tab cloud runner), and the runners themselves
-- with what each can do. Both sync, so every runner sees every job; the
-- hub's `job:<id>` lease decides which one works it.
CREATE TABLE jobs (
    id           TEXT PRIMARY KEY,
    run_id       TEXT NOT NULL,
    workflow_id  TEXT NOT NULL,
    -- waiting, running, done or failed: the body's state, for queries.
    state        TEXT NOT NULL,
    body         TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX jobs_by_state ON jobs (state, created_at);

CREATE TABLE runners (
    id       TEXT PRIMARY KEY,
    body     TEXT NOT NULL,
    seen_at  TEXT NOT NULL
);

-- Connections set to run in tab cloud: which, and their settings without
-- secrets (those go to the hub's secret store, encrypted). The cloud
-- runner reads this to know what to poll.
CREATE TABLE cloud_connections (
    id          TEXT PRIMARY KEY,
    enabled     INTEGER NOT NULL DEFAULT 1,
    settings    TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
