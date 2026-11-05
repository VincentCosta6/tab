-- Projects: a name, color and icon the person picks. Chats started in a
-- project belong to it, and each project is also a node in the graph.
CREATE TABLE projects (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    color       TEXT NOT NULL,
    icon        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

ALTER TABLE items ADD COLUMN project_id TEXT;
CREATE INDEX items_project ON items (project_id);

-- What agents (and the person) learned about a project: durable facts the
-- next chat should start with. Each cites the items it came from.
CREATE TABLE project_notes (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    topic       TEXT NOT NULL COLLATE NOCASE,
    text        TEXT NOT NULL,
    added_by    TEXT NOT NULL,
    session_id  TEXT,
    sources     TEXT NOT NULL DEFAULT '[]',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (project_id, topic)
);
