-- Email previews used to be the snippet captured at ingest. Drop them so
-- the full, formatted message is fetched the next time one is opened.
DELETE FROM previews WHERE item_id IN (SELECT id FROM items WHERE source = 'email');
