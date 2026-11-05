-- Queue -----------------------------------------------------------------

CREATE TABLE items (
    id            TEXT PRIMARY KEY,
    source        TEXT NOT NULL,
    external_id   TEXT NOT NULL,
    kind          TEXT NOT NULL,
    title         TEXT NOT NULL,
    body          TEXT,
    url           TEXT,
    actor         TEXT,
    context       TEXT,
    occurred_at   TEXT NOT NULL,
    blocks        TEXT NOT NULL DEFAULT '[]',
    entities      TEXT NOT NULL DEFAULT '[]',
    metadata      TEXT NOT NULL DEFAULT 'null',
    fingerprint   TEXT NOT NULL,
    state         TEXT NOT NULL DEFAULT 'open',
    snoozed_until TEXT,
    score         INTEGER,
    base_score    INTEGER,
    ai_adjustment INTEGER,
    urgency       TEXT,
    reason        TEXT,
    ranked_by     TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE (source, external_id)
);
CREATE INDEX items_queue    ON items (state, score DESC, occurred_at DESC, id);
CREATE INDEX items_source   ON items (source, occurred_at DESC);
CREATE INDEX items_kind     ON items (kind, occurred_at DESC);
CREATE INDEX items_actor    ON items (actor);
CREATE INDEX items_context  ON items (context);
CREATE INDEX items_unranked ON items (state) WHERE score IS NULL;
CREATE INDEX items_snoozed  ON items (snoozed_until) WHERE state = 'snoozed';

CREATE VIRTUAL TABLE items_fts USING fts5(
    title, body, actor, context, kind,
    content = 'items', content_rowid = 'rowid', tokenize = 'trigram'
);
CREATE TRIGGER items_fts_ai AFTER INSERT ON items BEGIN
    INSERT INTO items_fts (rowid, title, body, actor, context, kind)
    VALUES (new.rowid, new.title, new.body, new.actor, new.context, new.kind);
END;
CREATE TRIGGER items_fts_ad AFTER DELETE ON items BEGIN
    INSERT INTO items_fts (items_fts, rowid, title, body, actor, context, kind)
    VALUES ('delete', old.rowid, old.title, old.body, old.actor, old.context, old.kind);
END;
CREATE TRIGGER items_fts_au AFTER UPDATE OF title, body, actor, context, kind ON items BEGIN
    INSERT INTO items_fts (items_fts, rowid, title, body, actor, context, kind)
    VALUES ('delete', old.rowid, old.title, old.body, old.actor, old.context, old.kind);
    INSERT INTO items_fts (rowid, title, body, actor, context, kind)
    VALUES (new.rowid, new.title, new.body, new.actor, new.context, new.kind);
END;

-- Knowledge graph ---------------------------------------------------------
-- Nodes are entities (people, repos, channels, PRs, docs) plus items.
-- item_entities links items to what they're about; edges links entities
-- to each other (co-occurrence today, explicit relations later).

CREATE TABLE entities (
    id         INTEGER PRIMARY KEY,
    kind       TEXT NOT NULL,
    key        TEXT NOT NULL,
    label      TEXT NOT NULL,
    data       TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (kind, key)
);
CREATE INDEX entities_kind_label ON entities (kind, label);

CREATE VIRTUAL TABLE entities_fts USING fts5(
    label, key, content = 'entities', content_rowid = 'id', tokenize = 'trigram'
);
CREATE TRIGGER entities_fts_ai AFTER INSERT ON entities BEGIN
    INSERT INTO entities_fts (rowid, label, key) VALUES (new.id, new.label, new.key);
END;
CREATE TRIGGER entities_fts_ad AFTER DELETE ON entities BEGIN
    INSERT INTO entities_fts (entities_fts, rowid, label, key) VALUES ('delete', old.id, old.label, old.key);
END;
CREATE TRIGGER entities_fts_au AFTER UPDATE OF label, key ON entities BEGIN
    INSERT INTO entities_fts (entities_fts, rowid, label, key) VALUES ('delete', old.id, old.label, old.key);
    INSERT INTO entities_fts (rowid, label, key) VALUES (new.id, new.label, new.key);
END;

CREATE TABLE item_entities (
    item_id   TEXT    NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    entity_id INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    rel       TEXT    NOT NULL,
    PRIMARY KEY (item_id, entity_id, rel)
);
CREATE INDEX item_entities_entity ON item_entities (entity_id, item_id);

CREATE TABLE edges (
    src        INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    dst        INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    rel        TEXT    NOT NULL,
    weight     REAL    NOT NULL DEFAULT 1,
    updated_at TEXT    NOT NULL,
    PRIMARY KEY (src, dst, rel)
);
CREATE INDEX edges_dst ON edges (dst, rel);

-- Gateway: HTTP cache, AI cache, audit log -------------------------------

CREATE TABLE http_cache (
    key           TEXT PRIMARY KEY,
    provider      TEXT NOT NULL,
    url           TEXT NOT NULL,
    status        INTEGER NOT NULL,
    body          TEXT NOT NULL,
    etag          TEXT,
    last_modified TEXT,
    fetched_at    TEXT NOT NULL,
    expires_at    TEXT NOT NULL
);
CREATE INDEX http_cache_provider ON http_cache (provider, fetched_at DESC);
CREATE INDEX http_cache_expires  ON http_cache (expires_at);

CREATE TABLE ai_cache (
    key        TEXT PRIMARY KEY,
    model      TEXT NOT NULL,
    value      TEXT NOT NULL,
    created_at TEXT NOT NULL
);

-- One row per outbound call attempt, including ones answered from cache or
-- refused locally because of a rate limit.
CREATE TABLE api_audit (
    id           INTEGER PRIMARY KEY,
    ts           TEXT    NOT NULL,
    provider     TEXT    NOT NULL,
    connector    TEXT    NOT NULL,
    purpose      TEXT    NOT NULL,
    method       TEXT    NOT NULL,
    url          TEXT    NOT NULL,
    outcome      TEXT    NOT NULL, -- network | cache_hit | revalidated | stale | rate_limited | error
    status       INTEGER,
    duration_ms  INTEGER NOT NULL DEFAULT 0,
    rl_resource  TEXT,
    rl_limit     INTEGER,
    rl_remaining INTEGER,
    rl_reset     TEXT,
    error        TEXT
);
CREATE INDEX api_audit_ts        ON api_audit (ts DESC);
CREATE INDEX api_audit_provider  ON api_audit (provider, ts DESC);
CREATE INDEX api_audit_connector ON api_audit (connector, ts DESC);
CREATE INDEX api_audit_outcome   ON api_audit (outcome, ts DESC);
