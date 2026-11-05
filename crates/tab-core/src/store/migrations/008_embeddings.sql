-- Background embeddings. The vectors themselves are in `doc_vec`, a
-- sqlite-vec table created at runtime once the model's width is known.
CREATE TABLE embed_model (
    id     INTEGER PRIMARY KEY CHECK (id = 1),
    model  TEXT NOT NULL,
    dim    INTEGER NOT NULL
);

-- Which docs (`item:<id>`, `note:<id>`) have a vector, and when it was made.
CREATE TABLE doc_embeddings (
    doc          TEXT PRIMARY KEY,
    embedded_at  TEXT NOT NULL
);

-- Agents search by time window and by project; both are common filters.
CREATE INDEX items_occurred ON items (occurred_at DESC);
DROP INDEX items_project;
CREATE INDEX items_project ON items (project_id, occurred_at DESC);
CREATE INDEX project_notes_project ON project_notes (project_id, updated_at DESC);
