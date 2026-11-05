-- Context attached to a queue item. The item itself is always the primary
-- context; these rows add more. `section` is 'context' (used by the agent)
-- or 'found' (suggested by an agent or the graph, waiting to be promoted).

CREATE TABLE item_context (
    id          TEXT PRIMARY KEY,
    item_id     TEXT NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    section     TEXT NOT NULL CHECK (section IN ('context', 'found')),
    pinned      INTEGER NOT NULL DEFAULT 0,
    ref_item_id TEXT REFERENCES items (id) ON DELETE CASCADE,
    url         TEXT,
    title       TEXT NOT NULL,
    summary     TEXT,
    added_by    TEXT NOT NULL,           -- user | agent | graph
    created_at  TEXT NOT NULL,
    UNIQUE (item_id, ref_item_id),
    UNIQUE (item_id, url)
);
CREATE INDEX item_context_item ON item_context (item_id, section, pinned DESC, created_at);

-- Agent sessions. One task (item) can have several; the newest is reused.
CREATE TABLE sessions (
    id               TEXT PRIMARY KEY,
    item_id          TEXT REFERENCES items (id) ON DELETE SET NULL,
    agent            TEXT NOT NULL,      -- claude_code | opencode
    agent_session_id TEXT,               -- the agent's own id, for resume
    title            TEXT NOT NULL,
    status           TEXT NOT NULL,      -- idle | working | needs_input | done | failed
    cwd              TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);
CREATE INDEX sessions_item    ON sessions (item_id, updated_at DESC);
CREATE INDEX sessions_updated ON sessions (updated_at DESC);

CREATE TABLE session_messages (
    id         INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    role       TEXT NOT NULL,            -- user | assistant | tool | system
    kind       TEXT NOT NULL,            -- text | tool | status | error
    content    TEXT NOT NULL,
    meta       TEXT NOT NULL DEFAULT 'null',
    created_at TEXT NOT NULL
);
CREATE INDEX session_messages_session ON session_messages (session_id, id);
