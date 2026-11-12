-- How tasks relate, so the queue can nest and order them:
-- `followup`: src is a follow-up of dst and shows under it while dst is open.
-- `blocks`:   src has to be done before dst can start.
-- Keyed by the link itself, so two devices adding the same one agree.
CREATE TABLE item_links (
    src        TEXT NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    dst        TEXT NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('followup', 'blocks')),
    created_at TEXT NOT NULL,
    PRIMARY KEY (src, dst, kind),
    CHECK (src != dst)
);
CREATE INDEX item_links_dst ON item_links (dst, kind);

-- Follow-ups agents already made: a task an agent created, linked both ways
-- with an older task it came from.
INSERT OR IGNORE INTO item_links (src, dst, kind, created_at)
SELECT c.item_id, c.ref_item_id, 'followup', i.created_at
FROM item_context c
JOIN items i ON i.id = c.item_id
JOIN items p ON p.id = c.ref_item_id
JOIN item_context back ON back.item_id = c.ref_item_id AND back.ref_item_id = c.item_id
WHERE i.source = 'tab' AND json_extract(i.metadata, '$.created_by') = 'agent'
  AND c.added_by = 'agent' AND p.created_at < i.created_at;
