-- When an item's state (open, snoozed, done) last changed, so devices that
-- each have a copy keep the newest state rather than the newest row (see
-- hub_sync::merge_item_state). Arriving counts as opening at the time the
-- activity happened. Rows from before this get the same guess hub_sync makes
-- for rows from older apps: open since arriving means the activity's time,
-- anything else the last update.
ALTER TABLE items ADD COLUMN state_at TEXT;
UPDATE items SET state_at = CASE
    WHEN state = 'open' AND updated_at = created_at THEN occurred_at
    ELSE updated_at
END;
