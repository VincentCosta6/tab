-- Plan limits belong to a harness (Claude Code, Antigravity, Codex, ...),
-- not Claude Code alone. Existing rows came from Claude Code.
CREATE TABLE plan_limits_new (
    agent       TEXT NOT NULL,
    window      TEXT NOT NULL,
    label       TEXT,
    utilization REAL,
    resets_at   TEXT,
    status      TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (agent, window)
);
INSERT INTO plan_limits_new (agent, window, utilization, resets_at, status, updated_at)
SELECT 'claude_code', window, utilization, resets_at, status, updated_at FROM plan_limits;
DROP TABLE plan_limits;
ALTER TABLE plan_limits_new RENAME TO plan_limits;
