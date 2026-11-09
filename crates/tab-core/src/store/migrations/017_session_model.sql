-- The model a chat runs on, set from tab's default when its first message
-- is sent. Changing it in one chat leaves the others alone. NULL for chats
-- from before this, which follow the default until their next message; ''
-- for the harness's own default.
ALTER TABLE sessions ADD COLUMN model TEXT;
