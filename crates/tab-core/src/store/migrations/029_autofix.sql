-- Links in a task's Context the chat's bar shows (a PR), whether the person
-- dismissed one, and autofix's state for it: when it last saw activity
-- (backoff starts from there), when to look next, the commit it last saw
-- and which problems it already handed the agent (JSON array of ids).
-- This device's only: the device that runs the chat does the fixing.
CREATE TABLE autofix (
    item_id    TEXT NOT NULL,
    url        TEXT NOT NULL,
    enabled    INTEGER NOT NULL DEFAULT 0,
    dismissed  INTEGER NOT NULL DEFAULT 0,
    active_at  TEXT NOT NULL,
    next_at    TEXT,
    checked_at TEXT,
    head       TEXT,
    handled    TEXT NOT NULL DEFAULT '[]',
    last       TEXT,
    PRIMARY KEY (item_id, url)
);
CREATE INDEX autofix_due ON autofix (next_at) WHERE enabled = 1 AND dismissed = 0;
