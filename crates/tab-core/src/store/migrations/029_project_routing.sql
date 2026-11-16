-- Routing items to projects at ingest (crate::store::routing).
--
-- Who decided an item's project: 'auto' when tab routed it, 'person' when
-- the person (or an agent for them) set or cleared it. NULL is nobody yet;
-- an item with a project and no `project_by` was filed by hand before
-- routing existed. tab only routes items nobody has decided.
ALTER TABLE items ADD COLUMN project_by TEXT;
-- Why tab put it there, e.g. "Sentry project tab-desktop".
ALTER TABLE items ADD COLUMN routed_by TEXT;
-- A project tab isn't sure enough to file it in: shown as a chip to accept.
ALTER TABLE items ADD COLUMN suggested_project_id TEXT;
ALTER TABLE items ADD COLUMN suggested_by TEXT;

-- Containers (a repo, a Sentry project, a Linear team) tied to a project.
-- `always` routes the container's items there, `never` stops tab doing so
-- (an undone route, a dismissed proposal), and `proposed` waits for the
-- person. The repos in project_repos count as `always` too.
CREATE TABLE project_anchors (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- The container's graph entity.
    kind        TEXT NOT NULL,
    key         TEXT NOT NULL,
    label       TEXT NOT NULL,
    state       TEXT NOT NULL,
    -- How it came about: 'person', 'learned', 'scan', 'match' or 'undo'.
    source      TEXT NOT NULL,
    -- For a proposal, what it's based on ("SENTRY_PROJECT in .env").
    reason      TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (project_id, kind, key)
);
CREATE INDEX project_anchors_by_key ON project_anchors (kind, key);

-- What scanning a project's checkout found (service ids, app names), kept
-- so they still match containers that show up later. Not synced: each
-- device scans its own checkouts.
CREATE TABLE repo_hints (
    project_id  TEXT NOT NULL,
    service     TEXT NOT NULL,
    value       TEXT NOT NULL,
    -- Where it was found, relative to the checkout.
    file        TEXT NOT NULL,
    PRIMARY KEY (project_id, service, value)
);
