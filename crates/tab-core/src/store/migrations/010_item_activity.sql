-- What happened to each item, in order: it arrived, was snoozed, an agent
-- session started on it, a workflow ran or changed it, it finished upstream.
-- Append-only, so an item's history can be read back as a timeline.
CREATE TABLE item_activity (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id   TEXT NOT NULL,
    at        TEXT NOT NULL,
    kind      TEXT NOT NULL,
    title     TEXT NOT NULL,
    by        TEXT,
    detail    TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX item_activity_by_item ON item_activity (item_id, at);
