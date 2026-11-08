-- Branches a chat created on a git host, and the commit it last pushed to
-- each. A chat may force-push its own branch without asking, as long as
-- nobody else has pushed to it since.
CREATE TABLE session_branches (
    session_id  TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    -- owner/name on the host.
    repo        TEXT NOT NULL COLLATE NOCASE,
    -- The full ref, e.g. refs/heads/my-branch.
    ref         TEXT NOT NULL,
    head        TEXT NOT NULL,
    PRIMARY KEY (session_id, repo, ref)
);
