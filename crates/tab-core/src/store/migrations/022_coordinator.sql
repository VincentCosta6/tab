-- A project's coordinator watches it and compacts its own chat (docs/projects-hub.md, phase 3).

-- Wake on queue changes and check hourly (1), or only when asked (0).
ALTER TABLE projects ADD COLUMN coordinator_watch INTEGER NOT NULL DEFAULT 1;
-- Weekday morning brief at this local time (HH:MM), '' for none.
ALTER TABLE projects ADD COLUMN coordinator_brief TEXT NOT NULL DEFAULT '08:00';
-- Compact the coordinator's chat when a turn ends with more context than
-- this, in tokens; 0 never.
ALTER TABLE projects ADD COLUMN compact_at INTEGER NOT NULL DEFAULT 200000;

-- Each stretch of a chat before it was compacted: the agent session it
-- ran in, and what it carried forward. The messages stay in
-- session_messages, up to the `compacted` divider that names the segment.
CREATE TABLE session_segments (
    id                TEXT PRIMARY KEY,
    session_id        TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    project_id        TEXT,
    -- The agent's own session id for this stretch (no longer resumed).
    agent_session_id  TEXT,
    -- The carry-forward summary (markdown), which the person can edit; the
    -- next stretch starts with it in its brief.
    summary           TEXT NOT NULL,
    -- Context used when it was compacted, in tokens.
    tokens            INTEGER NOT NULL DEFAULT 0,
    messages          INTEGER NOT NULL DEFAULT 0,
    -- auto or manual.
    reason            TEXT NOT NULL DEFAULT 'auto',
    started_at        TEXT NOT NULL,
    ended_at          TEXT NOT NULL,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);
CREATE INDEX session_segments_session ON session_segments (session_id, ended_at);

-- What this device's coordinator monitor last looked at, per project.
-- Only the device that runs a coordinator wakes it, so this isn't synced.
CREATE TABLE coordinator_watch (
    project_id     TEXT PRIMARY KEY,
    -- Queue and thread changes up to here have been told to the coordinator.
    seen_at        TEXT NOT NULL,
    checked_at     TEXT,
    -- The local date (YYYY-MM-DD) of the last morning brief.
    briefed_on     TEXT
);
