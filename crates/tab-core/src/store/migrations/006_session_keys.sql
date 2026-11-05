-- Sessions that belong to something other than a queue item (a PR being
-- reviewed): found by `key`, with `brief` added to the agent's instructions.
ALTER TABLE sessions ADD COLUMN key TEXT;
ALTER TABLE sessions ADD COLUMN brief TEXT;
CREATE INDEX sessions_key ON sessions (key, updated_at DESC);
