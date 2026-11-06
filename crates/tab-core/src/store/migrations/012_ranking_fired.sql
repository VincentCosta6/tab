-- The rules that fired for an item's score (JSON array of strings), kept so the
-- UI can show a breakdown: base kind score, keyword and VIP deltas, the AI nudge.
ALTER TABLE items ADD COLUMN fired TEXT;
