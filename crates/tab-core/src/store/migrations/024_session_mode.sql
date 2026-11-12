-- The permission mode a chat runs in, set from tab's default when its first
-- message is sent, like its model. Changing it in one chat leaves the others
-- alone. NULL for chats from before this, which follow the default until
-- their next message.
ALTER TABLE sessions ADD COLUMN permission_mode TEXT;
