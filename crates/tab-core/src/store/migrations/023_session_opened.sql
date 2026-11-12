-- A task can have several chats (/clear starts one, /resume goes back to
-- one). The one it shows is the one opened last: when it was created, or
-- picked again with /resume. NULL on rows from older copies, which count
-- from created_at.
ALTER TABLE sessions ADD COLUMN opened_at TEXT;
UPDATE sessions SET opened_at = updated_at;
CREATE INDEX sessions_item_opened ON sessions (item_id, opened_at DESC);
