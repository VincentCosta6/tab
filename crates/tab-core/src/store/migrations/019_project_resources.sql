-- What a project knows besides its repos: websites, documents, email,
-- services (a Cloudflare project, a Stripe account), each with who may
-- read it. Indexing fills `status` and `chunks` later; the Knowledge tab
-- and tab_project list them.
CREATE TABLE project_resources (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- website, file, email, service, link, note
    kind        TEXT NOT NULL,
    title       TEXT NOT NULL,
    -- The link, or for email the label or sender, or a file's hub hash.
    url         TEXT,
    -- What it's for, or for a note its text.
    note        TEXT NOT NULL DEFAULT '',
    -- all, coordinator or vault: who may read it.
    access      TEXT NOT NULL DEFAULT 'all',
    -- Kind-specific choices: crawl depth, schedule, what agents may do.
    options     TEXT NOT NULL DEFAULT '{}',
    -- pending, indexed, live, failed, needs_sign_in
    status      TEXT NOT NULL DEFAULT 'pending',
    chunks      INTEGER NOT NULL DEFAULT 0,
    pinned      INTEGER NOT NULL DEFAULT 0,
    added_by    TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (project_id, kind, url)
);
CREATE INDEX project_resources_project ON project_resources (project_id, kind);
