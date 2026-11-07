-- Chats sync through the hub, so other devices can watch and steer them.

-- This install's id: which device runs a session, and who says hello to a hub.
CREATE TABLE this_device (
    id     INTEGER PRIMARY KEY CHECK (id = 1),
    device TEXT NOT NULL
);
INSERT INTO this_device (id, device) VALUES (1, 'device-' || lower(hex(randomblob(8))));

-- The device that runs the session (has its agent's history and working copy).
ALTER TABLE sessions ADD COLUMN device TEXT;
UPDATE sessions SET device = (SELECT device FROM this_device);

-- Messages get a key that's unique across devices; `id` stays this copy's
-- order and isn't synced.
ALTER TABLE session_messages ADD COLUMN uid TEXT;
UPDATE session_messages SET uid = lower(hex(randomblob(16)));
CREATE UNIQUE INDEX session_messages_uid ON session_messages (uid);
