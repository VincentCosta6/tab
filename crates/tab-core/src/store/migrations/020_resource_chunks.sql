-- What indexing read from a project's resources (and its repos' docs), in
-- pieces small enough to embed and cite. Each device fetches and indexes
-- for itself, so none of this is synced; `project_resources.status` and
-- `chunks` are what other devices see.
CREATE TABLE resource_chunks (
    id           INTEGER PRIMARY KEY,
    -- A project_resources id, or a project_repos id for a repo's docs.
    resource_id  TEXT NOT NULL,
    n            INTEGER NOT NULL,
    project_id   TEXT NOT NULL,
    -- all or coordinator, copied from the resource (search also checks the
    -- resource's current level).
    access       TEXT NOT NULL,
    -- The page title, file name, email subject.
    title        TEXT NOT NULL,
    -- Where it came from, for citations: the page's URL, the file's path in
    -- the repo, the email's link.
    source       TEXT,
    text         TEXT NOT NULL,
    UNIQUE (resource_id, n)
);
CREATE INDEX resource_chunks_project ON resource_chunks (project_id, access);

CREATE VIRTUAL TABLE resource_chunks_fts USING fts5(
    title, text,
    content = 'resource_chunks', content_rowid = 'id', tokenize = 'trigram'
);
CREATE TRIGGER resource_chunks_fts_ai AFTER INSERT ON resource_chunks BEGIN
    INSERT INTO resource_chunks_fts (rowid, title, text) VALUES (new.id, new.title, new.text);
END;
CREATE TRIGGER resource_chunks_fts_ad AFTER DELETE ON resource_chunks BEGIN
    INSERT INTO resource_chunks_fts (resource_chunks_fts, rowid, title, text)
    VALUES ('delete', old.id, old.title, old.text);
END;

-- This device's indexing of each resource and repo.
CREATE TABLE resource_index (
    resource_id   TEXT PRIMARY KEY,
    -- The resource's updated_at when it was last indexed: an edit indexes
    -- it again.
    seen_version  TEXT NOT NULL,
    -- A hash of what was read, so unchanged content isn't chunked again.
    fingerprint   TEXT,
    indexed_at    TEXT,
    -- When to look again (websites and email on their schedule).
    next_at       TEXT,
    error         TEXT,
    -- When an agent's search last returned a piece of it.
    last_used_at  TEXT
);
