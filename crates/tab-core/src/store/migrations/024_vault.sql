-- The end-to-end encrypted vault (docs/projects-hub.md, "Vault design";
-- crate::e2e). Everything here syncs through the hub, which only ever sees
-- public keys, wrapped keys, MACs and ciphertext.

-- Each device that may open the vault, and the recovery key (id
-- 'recovery'). A device asks to join with its public key (pending); an
-- unlocked device approves it and vouches for its key with a MAC under the
-- vault key, so a key the hub swapped in is never wrapped for.
CREATE TABLE vault_devices (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    -- X25519, base64.
    public_key   TEXT NOT NULL,
    -- pending, active or removed.
    state        TEXT NOT NULL DEFAULT 'pending',
    -- {"key_id", "mac"}: HMAC under that vault key over the id and key.
    vouch        TEXT,
    approved_by  TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

-- Generations of the vault key. A new one is made when a device is
-- removed; the one before vouches for it, and it carries the one before
-- sealed under itself, so older records still open.
CREATE TABLE vault_generations (
    id           TEXT PRIMARY KEY,
    prev_id      TEXT,
    -- HMAC under the previous key over this id and this key's hash.
    endorsement  TEXT,
    -- The previous key, sealed under this one (JSON).
    prev_sealed  TEXT,
    -- HMAC under this key, to tell a wrong key from a right one.
    check_mac    TEXT NOT NULL,
    created_by   TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

-- The vault key, wrapped once per device (and for the recovery key), per
-- generation.
CREATE TABLE vault_keys (
    id           TEXT PRIMARY KEY,
    key_id       TEXT NOT NULL,
    device_id    TEXT NOT NULL,
    -- {"ephemeral", "nonce", "ciphertext"}.
    wrapped      TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX vault_keys_device ON vault_keys (device_id);

-- Every read of the vault and every change to who can open it. Append
-- only: rows are never changed or removed, here or from the hub. What was
-- read (field names, why) is sealed under the vault key.
CREATE TABLE vault_audit (
    id           TEXT PRIMARY KEY,
    at           TEXT NOT NULL,
    device_id    TEXT NOT NULL,
    session_id   TEXT,
    project_id   TEXT,
    resource_id  TEXT,
    -- create, unlock, search, get, reveal, request, approve, deny, add,
    -- edit, delete, device_ask, device_approve, device_remove, rotate,
    -- recover, export.
    action       TEXT NOT NULL,
    -- person, coordinator or thread.
    actor        TEXT NOT NULL,
    -- Sealed JSON (field names, why), or NULL.
    detail       TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX vault_audit_at ON vault_audit (at);
CREATE TRIGGER vault_audit_no_update BEFORE UPDATE ON vault_audit BEGIN SELECT RAISE(IGNORE); END;
CREATE TRIGGER vault_audit_no_delete BEFORE DELETE ON vault_audit BEGIN SELECT RAISE(IGNORE); END;

-- Vault key generations this device has opened, by fingerprint. Not
-- synced: it's how this device tells the hub didn't hand it a key of its
-- own (a newer generation must chain back to one of these).
CREATE TABLE vault_trust (
    key_id       TEXT PRIMARY KEY,
    fingerprint  TEXT NOT NULL,
    seen_at      TEXT NOT NULL
);
