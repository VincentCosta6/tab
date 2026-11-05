-- Graph nodes the person deleted. Connectors keep reporting the same people,
-- repos and PRs, so without this a deleted node would come back on the next
-- sync; link_item skips anything listed here.
CREATE TABLE entity_blocks (
    kind       TEXT NOT NULL,
    key        TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (kind, key)
);
