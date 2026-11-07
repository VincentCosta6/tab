-- What a project is, beyond its name: what it does and the rules to work by,
-- and the repos its work happens in. Agents read these with tab_project and
-- keep them current; the person edits them on the Projects tab.
ALTER TABLE projects ADD COLUMN about TEXT NOT NULL DEFAULT '';
ALTER TABLE projects ADD COLUMN rules TEXT NOT NULL DEFAULT '';

CREATE TABLE project_repos (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- owner/name, or a URL for repos that aren't on GitHub.
    repo        TEXT NOT NULL COLLATE NOCASE,
    -- Where it's checked out, if known.
    path        TEXT,
    -- What the repo is for in this project.
    note        TEXT NOT NULL DEFAULT '',
    added_by    TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (project_id, repo)
);
