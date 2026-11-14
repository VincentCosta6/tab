-- Model families the person saved: their own, and edited copies of the
-- built-in ones (`fast`, `balanced`, `deep`), which replace those. The
-- body is the family as JSON. Synced, so every runner resolves a family
-- the same way.
CREATE TABLE model_families (
    id          TEXT PRIMARY KEY,
    body        TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
