-- Rich preview blocks per item. Rendering reads from here; the engine's
-- refresher updates entries in the background when rate limits allow.
CREATE TABLE previews (
    item_id    TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
    blocks     TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    -- Set when the item changed upstream; the refresher picks these first.
    dirty      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX previews_fetched ON previews (dirty DESC, fetched_at);
