# tab architecture

tab is a local-first desktop app that gathers everything that needs a human (review requests, CI failures, mentions, meeting action items, agents waiting on you), ranks it, and gives you one place to act on it. Everything comes in through **connectors**, and everything a connector contributes is **data** the app renders with a shared design system.

This document describes what exists today and the design for what comes next. Sections marked **(built)** are in the code; **(designed)** sections are the agreed shape for upcoming phases.

## Principles

1. **Local first.** One SQLite file holds the queue, the knowledge graph, the HTTP cache and the audit log. A server comes later for sync and webhooks; the data model is already sync-ready (UUID ids, `updated_at` everywhere, append-only audit).
2. **Metadata driven.** Connectors describe themselves (settings form, kinds, icons, tones) and their content (`Block`s). Adding a connector should need no UI code unless it brings a whole new surface, like code review.
3. **Deterministic core, AI on top.** Rules always produce the same score for the same item. The AI layer is optional, bounded, and cached by its inputs, so it is repeatable too.
4. **Every outbound call goes through the gateway.** One place for caching, rate limits, retries and audit. Nothing calls `reqwest` directly.
5. **One event stream.** Every state change is an `EngineEvent`. The UI, automations and the MCP server all subscribe to the same stream.
6. **Same capability, three ways in.** Anything you can configure in the UI you can configure over MCP (talk to the app) or by editing JSON (build it yourself).

## Layout (built)

One repo holds the app and the hub. The hub is the part of tab that owns work state (records, the knowledge graph, leases, the change log every device syncs from). The same hub code runs inside the desktop app on local SQLite, and later in the cloud inside a Cloudflare Durable Object, one per person or team. Only the storage backend and a thin host differ.

```
crates/tab-domain/      Pure logic. No I/O, no clock, no threads; builds for wasm32.
  model.rs              Domain types: IncomingEvent, QueueItem, Block, EntityRef, Ranking, descriptors, Change
  rules.rs              Deterministic ranking rules
  lease.rs              Lease rules: grant / renew / refuse, fencing tokens
  ops.rs                The change vocabulary (Put, Delete, Link, Unlink), validation, search text
  workflow/             Workflow definitions, validation, templates, run records
  links.rs              Recognizing links agents create or mention
crates/tab-protocol/    The shared API layer. Protobuf (proto/tab/hub/v1/hub.proto), no gRPC; builds for wasm32.
crates/tab-store/       HubStore trait + backends (in-memory reference, SQLite), sea-query statements,
                        and the conformance suite every backend must pass. BlobStore (files) with its
                        own suite: in-memory and local-folder backends.
crates/tab-hub/         The hub: answers protocol requests on any HubStore; the live channel (watch and
                        steer); file cleanup; migration between stores.
                        Builds for wasm32 so the Worker can host it.
crates/tab-core/        The desktop engine. No Tauri dependency, fully unit tested. Re-exports tab-domain
                        under its old paths (tab_core::model, tab_core::links, ...).
  connector.rs          Connector + ConnectorFactory traits, ConnectorContext
  connectors/           github.rs, demo.rs, email.rs, ...
  remote_mcp/           Remote MCP servers the person connects (Linear, Notion, a URL):
                        OAuth sign-in (oauth.rs), sign-ins kept out of config (vault.rs)
  mcp.rs                tab's MCP server for agents, plus /mcp/<id> (remote servers,
                        signed in by tab) and /oauth/callback
  engine.rs             Wires connectors → store → ranker → events
  gateway.rs            Cache, rate limits, audit for every API call
  ranker/               ai.rs (Haiku layer) over tab-domain's rules
  store/                SQLite: items, graph, http cache, audit; migrations/
  workflow/runtime.rs   Runs workflows off the engine's event stream
  config.rs             TabConfig (profile, ranking, connectors map)
src-tauri/              Desktop shell: Tauri commands + event forwarding
src/                    React UI
  design/               Design system: tokens.css, components, icons, blocks, markdown
  modules/              registry.ts + built-in modules (work, connections, settings)
  lib/                  api.ts (Tauri or browser mock), types.ts (mirrors model.rs),
                        protocol/ (hub messages generated from hub.proto by protobuf-es)
buf.yaml, buf.gen.yaml  Protobuf lint/breaking rules and TypeScript generation (`pnpm proto`)
scripts/storage-tests.sh  The full storage suite, outside CI (see Storage tests)
```

Coming next (designed): `apps/mobile/`, `cloud/worker/` (workers-rs host for tab-hub on Durable Objects), `crates/tab-hub-org/` (org features).

**Where things run today.** The desktop app runs its own hub (`tab_core::local_hub::LocalHub`): a `tab-hub` on SQLite in `hub.db` next to `tab.db`. The hub's change log is the source of truth; `tab.db` is this device's copy, kept because it holds the indexes, full-text search and vectors every screen reads. The engine keeps its SQL, and `store::hub_sync` keeps the copy and the hub in step:

- **Out, as each change commits.** Triggers on every synced table (`store::hub_sync::SYNCED`: projects, items, notes, context, entities, graph edges, activity, workflows and runs) record each changed key in `hub_outbox`, in the same transaction. A SQLite commit hook wakes the sync loop, which sends the current rows to the hub as `Put`/`Delete` ops in one `Apply` (skipping rows the hub already has unchanged). A record is `kind = <table>`, `id = <JSON array of the primary key>`, body = the row as JSON.
- **The hub decides.** If the hub refuses a change (validation, size limits), that row is put back to the hub's version locally. The rest of the batch still goes through.
- **In, from the log.** The app then reads the hub's log with `GetChanges` from where this copy last got to (`hub_sync.pulled_to`) and writes the hub's rows into `tab.db` with the triggers off, so nothing echoes. Rows changed here that the hub hasn't seen yet keep the local change. Rows already identical are skipped, and the UI is told about the rest. Other devices' changes arrive this way, and a fresh or deleted `tab.db` is rebuilt from the hub.
- **Twins.** Two devices can ingest the same thing (one GitHub notification) under different ids. When a hub row collides with a local row on a natural key, the one later in the log wins. Every device replays the same order, so they keep the same one.
- **First meeting.** An empty hub gets everything in `tab.db`. A `tab.db` meeting a hub that already has data takes the hub's rows, then sends only what the hub lacked. If `hub.db` is replaced by one that's behind this copy, they're introduced again.
- **Other devices.** The hub listens on `wss://127.0.0.1:47850/hub` (binary Protobuf frames; the next free port if taken), and their changes wake the same sync loop. See *Transport* below.
- **Status.** Settings → Hub shows where the hub runs, its address, the app process's CPU and memory, the size of the log and store, changes still waiting to sync, and each connected device with how far behind it is.

The sync speaks only the hub protocol (`Apply`, `GetChanges`), so pointing the app at a cloud hub replaces the in-process call with the WebSocket and nothing else. Not synced, on purpose: caches (HTTP, AI, previews, embeddings), the API audit log and usage counters.

**Chats sync too** (`sessions`, `session_messages`), so other devices can watch and steer them. Each session names the device that runs it (`sessions.device`, this install's id from `this_device`); only that device runs its turns. Messages sync by a `uid` (their local `id` only orders this copy). Only what the person reads in a chat travels: replies, their own messages, prompts, notes and errors. Tool output stays on the device that ran it, and a tool call syncs as its one-line summary without its input, except the step list and questions for the person, whose input is what they read. A message's text is cut at 64 KB in the hub.

### Transport (built)

Even on localhost, the hub only speaks **TLS 1.3**: no plain HTTP, no TLS 1.2 (refused with a `protocol_version` alert).

- **Certificate.** No public CA signs certificates for `127.0.0.1` or LAN addresses, so each install makes its own (ECDSA P-256, in `hub-tls/` next to `hub.db`, key readable only by the user). Devices don't trust it through a CA: they **pin** its fingerprint (`sha256/<hex>`, shown on the Hub page), learned at pairing along with the token (`tab_core::hub_tls::pinned_client`). Nobody else can mint a certificate a paired device accepts.
- **TCP** carries HTTP/2 and HTTP/1.1 (ALPN). The sync WebSocket works over either: an HTTP/1.1 upgrade, or HTTP/2 extended CONNECT (RFC 8441).
- **QUIC** on the same port (UDP) carries **HTTP/3** for files, advertised with `Alt-Svc: h3=":<port>"`, so clients that speak it switch. No 0-RTT, since early data can be replayed.
- **Auth** is `Authorization: Bearer <token>`, or the WebSocket subprotocol `tab.token.<token>` for browsers (they can't set headers on a WebSocket). A token in the URL is ignored, since URLs end up in logs. Tokens compare in constant time.
- **Not yet over QUIC: the sync stream.** WebSockets over HTTP/3 (RFC 9220) isn't supported by the Rust HTTP/3 stack (`h3`) or, as far as we know, by Cloudflare yet; WebTransport is the QUIC-native replacement to move to once the cloud side supports it. Its frames are already compact Protobuf.
- **In the cloud (designed)**, Cloudflare terminates TLS and HTTP/3 at the edge; the zone's minimum TLS version is to be set to 1.3 so the cloud hub matches.

### Files (built)

Attachments live in the hub's file store, beside the records: `tab_store::blob::BlobStore`, content-addressed by SHA-256 (`sha256:<hex>`), so the same file attached twice is stored once. Backends: in memory, and a local folder (`hub-files/` next to `hub.db`, sharded by the first two hex digits, written via a temp file and rename). An R2 backend comes with the cloud hub.

- **Records refer to files by hash**, anywhere in their body (a message's `meta.files`). `HubStore::blob_refs` lists every hash a current record mentions, and `tab_hub::blobs::collect` deletes files nothing refers to once they're a day old (the grace period covers an upload whose record hasn't arrived yet). The desktop runs it hourly. The hub owns cleanup; each device's `attachments/` folder is a cache.
- **Bytes go over HTTP, not the sync socket**: `PUT /files` returns `{hash, size}`, `GET /files/<hash>` returns the bytes (immutable, so cacheable forever), over HTTP/3 or HTTP/2. In the cloud these become signed R2 URLs.
- **On the desktop** a pasted file goes into the hub and is copied into `attachments/<hash hex>/<name>`, so the path itself names the file. A path that isn't there (swept from the cache, or from a chat another device runs) is fetched from the hub when shown.

### Watch and steer (built)

A session runs on one device: it has the agent's own history and the working copy. Other devices watch and steer it through the hub's **live channel** (`tab_hub::live`), which carries what doesn't belong in the change log:

```
 runner (desktop)                     hub                       watcher (phone, laptop)
   agent streams ──Publish(session)──► relays to watchers ──Live──► shows the reply as it streams
                  ◄──────Watched────── when watchers change ◄─Watch── opens the chat
   steer / approve ◄─────Steered────── routes to the runner ◄─Steer── types a message, answers a prompt
```

- **Stored messages and status** reach watchers through sync, like any record. **Streamed reply text and approval prompts** are published live and never stored (nothing else goes live: no tool calls, no tool progress, and long strings in a prompt's tool input are cut at 2,000 characters); a watcher joining mid-turn gets a snapshot (the reply so far, open prompts) because the runner is told when watchers change.
- **Steering** is a `SteerCommand` (`message` with text and file hashes, `approval`, `interrupt`, `stop`). The runner fetches attached files from the hub, then the message steers the running turn or starts the next one. As in Claude Code's terminal, the agent reads a steering message at its next step, after the running tool ends. While a tool holds it, the chat offers **Send now** (`interrupt`, Esc in the terminal): tab sends Claude Code's interrupt, the tool is stopped, and the turn carries on with the queued message. The hub routes by the session record's `device`; if that device isn't connected, steering is refused with a message saying so. Only the runner may publish for a session.
- **On the desktop**, the app watches the sessions other devices run while they're mid-turn, shows their live events in the chat, and sends what the person types or answers there to the runner.

Next: "Continue on this device" (hand a session to another device: code over a git branch, the transcript and agent history as an encrypted file in the file store, a `session:<id>` lease to fence the old runner), then end-to-end encryption of message bodies and files with routing metadata left readable.

### Hub protocol (built)

Clients and hubs exchange Protobuf messages from `crates/tab-protocol/proto/tab/hub/v1/hub.proto`: a `ClientEnvelope` or `ServerEnvelope` per binary WebSocket frame (or passed in-process on the desktop), each carrying `protocol_version`, `request_id` and a `oneof` body. Rust types come from prost (compiled with protox, so no `protoc` install), TypeScript types from protobuf-es. Record bodies travel as JSON bytes so connectors can describe items without protocol changes.

We use Protobuf messages but **not gRPC**: browsers and the webview can only reach gRPC through a gRPC-Web proxy, Workers can't host a gRPC server (tonic needs tokio and raw HTTP/2), and the hub's channel is a long-lived WebSocket on a Durable Object. Protobuf over Rust-only formats like postcard because the protocol crosses Rust and TypeScript and old and new app versions.

Compatibility: fields and messages are only added, removed ones are `reserved`, `buf lint` runs in CI and `buf breaking` will once the proto is on `main`. A hub answers every version from `MIN_PROTOCOL_VERSION` to `PROTOCOL_VERSION`; unknown fields are skipped and unknown requests get an `UNSUPPORTED` error rather than being dropped.

### Storage layer (built)

`tab-store`'s `HubStore` trait is shaped around tab's operations (apply a batch, changes since a cursor, get, claim/release/expire leases, search, graph neighbors), not around SQL. The rules that decide what an operation means (lease outcomes, op validation, search matching) are in tab-domain, so backends only store and fetch. The trait is synchronous: rusqlite and the Durable Object SQL API both are, and it keeps the hub building for Wasm.

SQL backends share their tables and queries through `tab_store::sql`, built with sea-query, which renders for SQLite and Postgres. That's the "ORM" seam: no Rust ORM can drive the Durable Object's SQLite (only reachable as `storage.sql` inside the Worker), but a query builder works everywhere. Each backend runs the rendered SQL on its own driver (rusqlite, `storage.sql`, sqlx for Postgres later). Full-text search is the per-database part (FTS5 trigrams in SQLite, `pg_trgm` in Postgres).

The desktop and the cloud hub are both SQLite, so they share one store, `tab_store::sqlite_on::SqliteOn<C>`, written against a small `SqliteConn` interface (run a statement, read rows, run a closure in a transaction, size). The desktop plugs in rusqlite (`tab_store::sqlite::SqliteStore`); the hub Durable Object in tab-infra plugs in `ctx.storage.sql` with `transactionSync`. A test connection that behaves like the Durable Object's (integers back as floats, no `BEGIN`/`PRAGMA` from statements) runs the whole conformance suite and the property tests, so the two can't drift.

**Migration between local and cloud** (`tab_hub::migrate::copy`, or the `Import` request over the wire) replays one store's change log into an empty one. `Import` takes the log in pages; the first must start at seq 1 on an empty hub and each later page must continue right after the head on the same connection, so it can't rewrite a hub in use. Each change keeps its author, time and sequence number, so devices synced to the old hub carry on against the new one. Leases aren't copied. Tests move data SQLite → in-memory → SQLite and compare everything.

### Storage tests (built)

**Rule: every behavior of the storage layer has a test, and every backend passes all of them.** That's what lets us add or swap a backend (Durable Object SQLite, Postgres) with confidence.

- **Conformance suite** (`crates/tab-store/src/conformance.rs`): one function per behavior (80 today: log ordering and paging, validation and all-or-nothing batches, record round-trips, graph edges and ordering, search matching and FTS edge cases, lease grant/renew/refuse/expire/release and fencing tokens, file references). `conformance_tests!` turns each into its own `#[test]` for a backend; it runs against the in-memory store, SQLite in memory, and SQLite on disk. A test fails if a check isn't registered in the macro.
- **File store suite** (`crates/tab-store/src/blob_conformance.rs`, `blob_conformance_tests!`): the same rules for `BlobStore` backends (dedupe, first-stored time, size limit, malformed hashes, paging), run against the in-memory and folder stores.
- **Property tests** (`tests/properties.rs`, proptest): random workloads run on two backends at once must agree on everything observable (the folder file store against the in-memory one too); replaying any log rebuilds the same state; a lease key never has two live holders and tokens only go up.
- **Volume and timing** (`tests/perf.rs`, ignored by default): 100k items plus edges, paging the log, gets, searches, neighbors and 10k lease claims, each with a time budget. It already caught one problem: without WAL every lease claim waited on an fsync.
- **New behavior lands with its check. A storage bug gets a failing check before the fix.** A new backend runs `conformance_tests!` and joins the property tests.

These are too slow to run on every CI build, so they run with `scripts/storage-tests.sh` (add `--perf` for volume, `--cov` for coverage with a 95% line target) whenever tab-store changes, and on a cadence. CI only builds the hub crates for Wasm, lints the proto, and runs the fast domain, protocol and hub tests.

## Data flow (built)

```
 Connector.run()                                    UI (Work tab)
   │ IncomingEvent {kind, title, body, blocks,          ▲
   │                entities, metadata}               tab://event
   ▼                                                    │
 Engine ingest ─► Store.upsert ──► Change::Ingested ────┤
   │                 │ dedupe by (source, external_id)  │
   │                 │ link entities into the graph     │
   ▼                 ▼                                   │
 Ranker: rules ─► AI layer (cached) ─► Store.set_ranking ─► Change::Ranked
                                                        │
 User: done / snooze / reopen ─► Store.set_state ───────┤ Change::Done …
                     └─► Connector.on_done (e.g. mark GitHub thread read)
```

`Change` values (`ingested`, `updated`, `ranked`, `snoozed`, `woke`, `done`, `reopened`) are the trigger vocabulary for automations.

## Connectors (built)

A connector type is a `ConnectorFactory`:

- `descriptor()` returns id, name, icon, description, the **kinds** it emits (label, icon, tone) and its **settings fields** (text, secret, number, toggle). The Connections tab renders the card and settings form from this alone.
- `build(settings, gateway)` returns a running `Connector`, or an error the UI shows.

A `Connector` instance:

- `run(ctx)` polls or streams and calls `ctx.emit(IncomingEvent)`. It reports health with `ctx.synced()` / `ctx.set_status(...)` (`running`, `rate_limited`, `error`).
- `preview(item)` returns richer `Block`s for the detail pane, fetched through the gateway with a cache TTL, so opening an item is usually free.
- `on_done(item)` lets the source react (GitHub marks the notification thread read).

`IncomingEvent` fields that matter for extensibility:

| Field | Purpose |
|---|---|
| `kind` | Dotted, `app.thing` (`github.review_requested`). The prefix picks UI surfaces and default rules. |
| `blocks` | Rich content known at ingest (quotes, fields, stats, badges, links, code, choices). |
| `entities` | What it's about, with canonical keys (`github:acme/api#12`, `person github:dana`). Feeds the graph. |
| `metadata` | Free-form payload for plugin surfaces (PR number, repo, API URL). |

**Adding a connector**: write a factory + connector in `crates/tab-core/src/connectors/`, add it to `registry()`, use only `Call`s through the gateway, emit canonical entity keys. No UI change unless you add a module.

**Remote MCP connections (built).** Services with a remote MCP server (Linear, Notion, Sentry, Vercel, Stripe, Atlassian, or any URL) connect with one browser sign-in: tab finds the authorization server (RFC 9728 / 8414), registers itself (RFC 7591), signs in with PKCE back to `/oauth/callback` on its MCP server, and keeps the tokens out of `config.json` (DPAPI on Windows, the keychain on macOS, a private file elsewhere). Agents in chats the person drives get each server as `--mcp-config` entry `http://127.0.0.1:<port>/mcp/<id>`: tab adds the token, refreshes it, hides tools set to off and asks the person before tools set to ask. Because they come in through tab's own config, they work in strict chats. They give agents tools; they don't fill the queue yet. See `docs/connect-via-mcp.md`.

**Third-party connectors (designed).** Out-of-process connectors speaking a small JSON-RPC protocol over stdio (descriptor, run → event notifications, preview, on_done). This keeps the core safe from plugin crashes and lets connectors be written in any language. The protocol is deliberately close to MCP so an MCP server can be wrapped as a connector.

## Ranking (built)

Two layers, both reproducible.

1. **Rules** (`ranker/rules.rs`): base score per kind (exact or `prefix.*`), keyword deltas, VIP actors, mentions of your handles, and muted contexts. Pure function: no clock, no I/O. Editable in Settings and serialized in config.
2. **AI layer** (`ranker/ai.rs`): Claude Haiku (`claude-haiku-5-5`, `effort: low`, structured JSON output) reads the item, your profile, **your custom instructions**, and the rules result, and returns an adjustment clamped to ±25 plus a one-line reason. The verdict is cached in `ai_cache` under a SHA-256 of model, prompt version, instructions, profile, rules result and item content. Same inputs, same answer, no API call. Muted items skip it. If the call fails, the rules score stands.

Urgency is a pure function of the final score (80+ now, 60+ today, 35+ this week). The list order is `score DESC, occurred_at DESC, id`, a total order. Changing rules or instructions re-ranks open items; unchanged inputs come from cache.

## Gateway: cache, rate limits, audit (built)

Every call is a `Call { provider, connector, purpose, method, url, cache_ttl }`. "Who" and "why" are mandatory, so the audit log can answer "what is spending my GitHub quota".

- **Cache**: GET responses stored in `http_cache` with ETag / Last-Modified. Freshness is decided by the *caller's* TTL: previews accept 5 minutes, the notifications poll always revalidates. Revalidation sends `If-None-Match`; GitHub does not charge a `304` against the limit.
- **Rate limits**: parsed from `x-ratelimit-*` (GitHub and most REST APIs), `anthropic-ratelimit-*`, and `retry-after`, tracked per provider and resource (`core`, `search`, `graphql`). After a `429` or exhausted limit the gateway refuses further calls *locally* until reset and serves stale cache if it has any. Network errors also fall back to stale cache (offline mode).
- **Concurrency**: at most 4 in-flight calls per provider.
- **Audit**: one row per attempt in `api_audit`: provider, connector, purpose, method, redacted URL, outcome (`network`, `cache_hit`, `revalidated`, `stale`, `rate_limited`, `error`), status, duration, and the limit state at the time. Secrets in query strings are masked. 30-day retention. The Connections tab shows limits, usage grouped by caller and purpose, and the raw log.

**Budgets (built)**: every `ConnectorDescriptor` declares `rate_limits` (`RateLimitSpec { provider, resource, requests, per_seconds, burst }`), and the engine registers them with the gateway's `Limiter` for every connector, installed or not. The limiter keeps a sliding window of requests that actually went over the network and computes *headroom* as the lower of local (declared budget minus usage) and remote (provider-reported remaining over limit).
- Calls carry a priority via a task-local: anything inside `ratelimit::background(...)` is background work; everything else is interactive. Connectors don't change.
- Interactive calls go out unless the declared budget is fully spent.
- Background calls are deferred (`Outcome::Deferred`, served stale when cached) when headroom is under 30%, or for 30 seconds after a burst (more than `burst` requests in 10 seconds).
- The Connections → API usage tab shows live pressure per bucket: usage, headroom, recent burst count, cool-down, deferred count.

**Preview cache (built)**: `previews` holds each item's rendered blocks. `EngineHandle::preview` returns whatever is cached immediately, however old, and queues a background refresh if it is past the connector's `preview_ttl()`. Only a never-seen item waits on the network. A refresher task runs every 45 seconds, takes the most important open items with missing, dirty (item changed upstream) or stale previews, and refreshes at most 12 per pass at background priority, skipping a connector for the rest of the pass once it is deferred. Results arrive in the UI as `PreviewChanged` events. In the UI, React Query runs with `staleTime` and `gcTime` at infinity and is persisted to localStorage; engine events patch exactly the affected queries, so nothing polls.

**Next (designed)**:
- *CLI tools*: a `CommandProvider` that runs `gh`, `jira`, etc. through the same audit and concurrency limits, with output caching keyed by argv.
- *Local token buckets* for providers that don't send limit headers (Slack tiers are documented, not reported).

## Storage (built)

One SQLite database (`tab.db` in the app data dir), WAL mode, `PRAGMA user_version` migrations in `store/migrations/`.

| Table | Notes |
|---|---|
| `items` | Queue. Unique `(source, external_id)`. Indexes for queue order, source, kind, actor, context, unranked and snoozed. |
| `items_fts` | FTS5 **trigram** index over title, body, actor, context, kind; kept in sync by triggers. Substring search ("checko" finds "#incident-checkout"), bm25 ranked. Queries under 3 chars fall back to `LIKE`. |
| `entities`, `entities_fts` | Graph nodes, unique `(kind, key)`, trigram searchable. |
| `item_entities` | Item ↔ entity with a relation (`from`, `in`, `about`, `mentions`). |
| `edges` | Entity ↔ entity, weighted. `co_occurs` today; typed relations later. |
| `http_cache`, `ai_cache` | Gateway and ranker caches. |
| `api_audit` | Append-only audit log, indexed by time, provider, connector, outcome. |

Scaling notes: writes are tiny and serialized behind one connection; when reads contend we add a read-only connection pool (WAL allows concurrent readers). Audit and cache are pruned on a timer. Large blobs (diffs, transcripts) will go to files referenced from SQLite, not into rows.

## Knowledge graph

**Built**: every item links to its entities; entities on the same item get a weighted `co_occurs` edge. The detail pane shows the item's entities and **Related** items (most shared entities first). `search_entities` and `neighbors` exist in the store.

**Designed**:
- **Graph view** module: a force-directed canvas (WebGL for large graphs) centered on a selected entity or item. Click a node to open a side panel with its items, rich links and neighbors; double-click to re-center and traverse. Filters by entity kind and time.
- **Typed relations** from connectors (`person reviews pull_request`, `pull_request touches repo`) and from agents over MCP (`link_context`), on top of co-occurrence.
- **Agent context**: MCP tools let agent sessions search the graph for relevant items and link what they produce (PRs, docs, decisions) back into it.

## Modules and the design system (built)

The UI is a set of **modules** (`src/modules/registry.ts`). A module contributes tabs today; next it can contribute item surfaces (by kind prefix), settings panes, automation actions and custom block renderers. The core app is itself three modules: Work, Connections and Settings. GitHub's review workspace is a module that adds the Reviews tab and a review surface for PR items.

The design system (`src/design/`) is the only source of styling:
- `tokens.css`: color, type, spacing, radius tokens with light and dark themes. Six semantic **tones** (neutral, accent, info, success, warning, danger) are the only way connectors influence color.
- `components.tsx`: Button, Badge, SourceIcon, ScorePill, Card, Section, PageHeader, EmptyState, inputs, Toggle, Tabs, Meter, StatusDot.
- `blocks.tsx`: one renderer per `Block` variant.
- `icons.tsx`: the icon set connectors reference by name.

Rule of thumb: a new screen should be composable from these; if it isn't, add the missing component to the design system first.

## Workflows (built)

"When something happens in tab, do these steps." Built in the Workflows tab (a visual builder), by the builder's assistant, over MCP, or as JSON.

**Actions are connector metadata.** Next to `kinds` (what a connector emits), every `ConnectorDescriptor` lists `actions`: `ActionInfo { id, label, description, icon, effect, inputs, outputs, spends, for_kinds }`. `effect` is `read`, `write` or `destructive`; `inputs`/`outputs` are typed fields (`text`, `long_text`, `number`, `bool`, `select`, `list`) with defaults like `{{item.metadata.repo}}`. `Connector::act(action, input)` runs one, through the gateway like every other call. Built in: GitHub (`pr.get`, `issue.comment`, `issue.add_labels`, `pr.request_reviewers`, `issue.create`, `pr.merge`, `issue.close`), email (`message.mark_read`, `message.flag`, `message.move`) and tab (`task.create`, `item.snooze`, `item.done`, `item.reopen`, `item.set_project`, run by the engine itself). Adding an action needs no UI work: the builder's forms come from the descriptor.

**A workflow is data** (`workflow/mod.rs`): one trigger (`item` change from chosen connections and kinds with conditions, `signal` from another workflow, `schedule`, or `manual`) and ordered steps: `action`, `filter` (stop unless conditions hold), `set` (compute values), `emit` (signal other workflows) and `command` (run a program in the sandbox; see Sandbox). Each step can have `when` conditions. Templates (`{{item.title}}`, `{{steps.pr.additions}}`, `{{signal.repo}}`) let steps feed each other; a value that is exactly one template keeps its type. There's no model call anywhere in a run, so runs are deterministic.

**Runtime** (`workflow/runtime.rs`) is one more subscriber to the engine's event stream. Stored in `workflows` (JSON definition) with every save kept in `workflow_versions`; runs in `workflow_runs` with each step's inputs, outputs, status and timing, for 30 days.

**Guardrails.**
- *Loops.* Each run carries the chain of workflows that caused it (tasks a run creates carry `workflow_chain` in metadata, and items a run changes remember the chain for 10 minutes). A workflow never runs inside its own chain, and chains stop at 3. Saving also refuses a set of enabled workflows that could start each other in a circle (a task-creating workflow on every new tab item, two workflows signalling each other).
- *Dedup.* The same trigger (change, item, item version; or schedule slot) runs a workflow once, enforced by a unique index.
- *Rate.* Per-workflow runs per hour (default 30) and per-item cool-down, 200 runs an hour across all workflows, at most 10 actions per run, a 60 s step timeout. Connector actions run at background priority, so the gateway defers them when a provider budget is tight; the run is marked *held back* rather than spending what interactive use needs. Refused runs are written down at most once an hour per workflow.
- *Consent.* Agents can draft, save and dry-run workflows but every agent save is off and can't allow destructive steps or commands. The person turns a workflow on (which re-validates it), and destructive actions and command steps stay blocked until the person allows them on that workflow (`allow_destructive`, `allow_commands`).

**Dry runs** execute reads for real and show what each write would do with its rendered inputs.

**Model families.** An agent step pins a harness and model, or names a family (`tab_domain::families`): a named, ordered list of harness and model pairs. tab ships `fast`, `balanced` and `deep`, seeded from each harness's `families` in `harnesses.json`; the person edits them and adds their own in Settings → Agents, kept in the synced `model_families` table (a saved family replaces the built-in with its id). The run copies the family's candidates onto the job (`JobSpec.family`, `.candidates`); a runner may take it when it has any candidate's harness, runs the first it has, and moves on to the next in the same attempt only when the harness's error reads as a model reason (`families::model_trouble`: model unavailable, rate limited, plan used up). What ran and how many times it moved on are on `JobResult.usage`.
**On queue items.** Each item's runs add up to one workflow status beside the agent's on its queue row and header: waiting for you (a step needs your OK, or the agent a run waits on asked you something), running, waiting on an agent, failed, held back, done (`tab_domain::workflow::item_status`, pure; `workflow_activity` reads it for the whole queue in one query). Only the newest run of each workflow on an item counts once it ends; done shows for an hour, failed and held back for a day, a blocked run for a week. The item lists its runs with links to the run, its agent's chat, and the chat or run that started it.

**Approve and carry on.** A run blocked on a destructive or command step keeps its values (`resume` with no job). The person can approve that one step for that one run, with the exact inputs shown, from the item or the Runs panel (`workflow_run_answer`, a Tauri command only: no MCP tool can); the run carries on from that step. Declining ends it. The workflow's `allow_destructive` and `allow_commands` don't change.

**Agents start workflows.** `tab_run_workflow` runs a workflow the person turned on (never one that's off), through every guardrail; `tab_get_workflow_run` reads a run. The run records the agent chat that started it (`started_by`). An agent a workflow's job started passes that run's chain on, so a workflow can't loop through an agent.

**Building with AI.** The builder has an assistant chat (session key `workflows`) whose agent uses MCP tools `tab_list_actions`, `tab_list_workflows`, `tab_save_workflow` and `tab_test_workflow`; the open workflow is sent with each message, and what the agent saves appears on the canvas right away.

**Fix and optimize.** `workflow::suggest` (pure) reads a workflow's stats and suggests, by fixed rules with a minimum sample each: a cheaper family for an agent step that finishes on its own 95%+ over 20+ runs (or a cheaper model already seen, for a pinned one), the next family up for one failing 25%+ or finishing alone under 60% over 10+ runs, more time for a step whose p95 is within 80% of its limit (10+ runs), a higher hourly limit after 3+ hours held back by it, a wider placement when the middle queue wait is 5+ min and over half the step's time, and a note when an approval was granted 10+ times and never declined. Each carries its evidence and, where there is one, the changed workflow, which the Insights panel loads as an unsaved draft; none ever allows destructive steps or commands. A failure group or suggestion can be handed to the builder's assistant as a prepared message (the person sends it), and a failed run can be run again (`workflow_rerun`; the new run's `rerun_of` links back). Agents read the same numbers with `tab_workflow_stats` and `tab_workflow_failures`. A run the person declined at an approval (`Run::declined`: stopped with its last step blocked) is neither a success nor a failure in the stats.
