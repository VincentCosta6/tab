-- What agents and the ranker spent, one row per turn or ranking call.
-- On a Claude subscription the cost is Claude Code's API-equivalent figure.
CREATE TABLE agent_usage (
    id            INTEGER PRIMARY KEY,
    at            TEXT NOT NULL,
    -- 'agent' (a chat turn) or 'ranker' (reading the queue)
    purpose       TEXT NOT NULL,
    session_id    TEXT,
    agent         TEXT NOT NULL,
    model         TEXT,
    -- 'subscription', 'api' or 'unknown'
    billing       TEXT NOT NULL,
    cost_usd      REAL NOT NULL DEFAULT 0,
    input_tokens  INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX agent_usage_at ON agent_usage (at);

-- The Claude plan's usage windows (five_hour, seven_day, ...), as Claude Code
-- last reported them.
CREATE TABLE plan_limits (
    window      TEXT PRIMARY KEY,
    utilization REAL,
    resets_at   TEXT,
    status      TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
