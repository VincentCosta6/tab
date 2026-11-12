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

## Item timelines (built)

`item_activity` is an append-only log per item: arrived (and from where, or which workflow created it), updated, snoozed, woke, done (by you or a workflow), reopened, finished upstream (a PR merged or closed), moved to a project, agent session started / waiting on you / failed, and workflow runs. Reading a timeline also pulls in what happened to items linked into the item's context (a PR the agent raised shows its merge), so a task reads as: created, Claude Code session started, PR linked, PR merged, workflow ran. The Timeline tab on every item shows it, and `tab_get_item` gives it to agents.

## MCP server (built: core tools; designed: the rest)

Built: Streamable HTTP (JSON responses) on `127.0.0.1:47821` (next free port if taken), bearer token generated on first launch and shown in Settings. Tools: `tab_search`, `tab_get_item`, `tab_add_found`, `tab_attach_context`, `tab_create_task`, and `approve` (Claude Code's `--permission-prompt-tool`). Sessions started by tab send `X-Tab-Session`, so "add to Found" lands on the right task without an id.

Designed: a local MCP server (Streamable HTTP on localhost with a per-install token, plus stdio for CLI agents) so any agent (Claude, ChatGPT, OpenCode) can use and configure tab.

- **Read**: `queue_list`, `queue_search`, `item_get` (with preview blocks), `graph_search`, `graph_neighbors`, `item_related`.
- **Act**: `item_done`, `item_snooze`, `link_context` (attach an artifact or note to items/entities), `report_status` (for agent sessions).
- **Configure**: `connectors_list`, `connector_configure`, `rules_get` / `rules_update`, `instructions_set`, `automations_list` / `automation_upsert` / `automation_test`.
- **Resources**: `tab://item/{id}`, `tab://entity/{kind}/{key}`.

Configuration tools write the same config and tables the UI does, so "set up an automation that snoozes Dependabot PRs until Friday" in chat produces exactly what the builder would.

## Agent sessions (built: Claude Code, OpenCode, Codex and Pi)

Built (`tab-core/src/agents`): one session per item, reused on every message; transcripts in `sessions` / `session_messages`; events `SessionChanged`, `SessionMessage`, `SessionDelta` (streaming text), `ApprovalRequested` / `ApprovalResolved`.
- *Claude Code*: one `claude -p --output-format stream-json --include-partial-messages` process per turn, `--resume` with the stored session id, `--append-system-prompt-file` with the item and its Context, `--mcp-config` pointing at tab, and `--permission-prompt-tool mcp__tab__approve` so approvals come to the chat. Runs the user's own Claude Code install and login. `bypassPermissions` is never offered.
- *OpenCode*: tab starts `opencode serve` on a free localhost port (or uses a configured URL), injects its MCP server through `OPENCODE_CONFIG_CONTENT`, creates a session per task, sends `prompt_async` with the context as `system`, follows `GET /event` (SSE) for text deltas, tool parts and `permission.asked`, and answers permissions with `POST /permission/{id}/reply`.
- *Codex* (`agents/codex.rs`): one `codex exec --json` process per turn (`resume <thread id>` after the first), the context as `developer_instructions`, tab's MCP server through `-c mcp_servers.tab.*` with its token in `TAB_MCP_TOKEN`. `exec` can't route approvals, so it runs with `approval_policy="never"`: inside tab's sandbox where that's enforced (`--sandbox danger-full-access`, since Codex's own sandbox can't nest in it), else in Codex's `workspace-write` sandbox. Strict adds `--ignore-user-config --ignore-rules`, and a call to any MCP server but `tab` stops the turn.
- *Pi* (`agents/pi.rs`): one `pi --mode json` process per turn; each chat is the Pi session `--session-id <chat id>`, the context goes in `--append-system-prompt <file>`. Pi has no approvals and reads MCP servers only from its own config, so it doesn't get tab's tools yet. Strict adds `--no-extensions --no-skills --no-prompt-templates --no-approve`.
- Codex and Pi share `agents/per_turn.rs`: messages sent mid-turn wait and run as the next turn, since neither CLI takes input once started.
- Which harnesses exist is data (`agents/harnesses.json`). Onboarding and Settings probe all of them at once and list the installed ones first; the rest sit under a search, in file order (Claude Code, then OpenCode).
- Windows: executables are resolved with `which` (so npm `.cmd` shims work) and started without a console window.

Still designed: a PTY terminal view for arbitrary CLI agents (below).

Agent-agnostic by design. The only hard requirement is that the agent's terminal can be streamed into tab and answered from tab.

- **Runner**: spawn any CLI agent (`claude`, `codex`, `opencode`, `aider`, …) in a pseudo-terminal (`portable-pty`) owned by the Rust core. Output streams to the UI over a Tauri channel into an `xterm.js` terminal; keystrokes go back to the PTY. Sessions survive window reloads and can be detached.
- **Status**: `working`, `needs_input`, `needs_review`, `monitoring`, `done`, `failed`. Adapters improve on a generic detector:
  - *Generic*: prompt and idle heuristics on the PTY stream.
  - *MCP*: the agent calls `report_status` on tab's MCP server (works for any MCP-capable agent).
  - *Native*: Claude Code hooks, OpenCode's server/event API, the Claude Agent SDK.
- `needs_input` and `needs_review` become queue items (kinds `agent.needs_input`, `agent.needs_review`) ranked like everything else; the detail pane embeds the live terminal, and `Choices` blocks become one-click answers.
- Transcripts are stored as files and linked into the graph (agent_session entity ↔ repo, PR, items it touched).

## GitHub review workspace (built)

Opening a PR (Reviews tab, or Review on a PR item) boots an editor-style workspace around it.

- **Boot.** `workspace.rs` clones the repository once per computer (blobless, `workspaces/<owner>/<name>/repo` under the app data dir), fetches `refs/pull/<n>/head` (forks work) and checks the head out into a worktree of its own (`pr-<n>`). git's `--progress` output streams to the UI as `tab://workspace` events. The token reaches git through `GIT_CONFIG_*` environment variables per command, never `.git/config`. The diff base is the merge base, like github.com. Without git (or if the clone fails) you can review through the API instead.
- **Layout.** Header with PR state and what blocks merging (CI, conflicts, open threads, review decision, behind base, ready to merge / ready for review), an Actions menu (view on GitHub, open in VS Code, show folder, copy branch / checkout command, update branch, draft / ready, merge three ways, close / reopen) and Submit review. An activity bar switches between Explorer (the whole repo, changed files marked), Search (`git grep` over the checkout, case / word / regex / include globs), Files changed (viewed checkboxes and progress, auto-read toggle) and Review threads. Editor tabs hold Changes (every diff stacked, mounted lazily), Overview, and files opened from the explorer, search or go to definition.
- **Diffs.** Monaco diff editors, split or unified, unchanged regions collapsed. Click or drag in the gutter (or select lines and press Ctrl Shift M) to comment on a line or range, on either side. Threads render inline under their lines: view zones reserve the space and real React cards sit in an overlay, so inputs behave normally.
- **Comments.** A markdown composer with a toolbar, preview, keyboard shortcuts and **Suggest** (a `suggestion` block prefilled with the selected lines, rendered as a change). "Add single comment" posts now; "Start a review" adds to the viewer's pending review on GitHub, so drafts follow you across machines. Replies, edits, deletes, resolve / unresolve. Submit review sends Comment, Approve or Request changes with a summary; Discard deletes the pending review.
- **Viewed files** sync with GitHub's own viewed state (`markFileAsViewed`), including "changed since last view". With auto-read on, a file is marked viewed once its end scrolls past the top of the view.
- **Language features.** Every TS/JS/JSON file in the checkout is loaded as a Monaco model at `file:///<path>`, so hover, go to definition (opens a tab), references and peek work across the repository. Go, SQL, Rust and Python get a language server (`lsp.rs`: gopls, sqls, rust-analyzer, pyright) started in the PR's worktree over stdio; `lsp.ts` opens head-side models with it and maps hover, definition, type definition, implementation, references, highlights and diagnostics into Monaco. tab doesn't ship the servers: it finds them on PATH and the usual install dirs (`~/go/bin`, `~/.cargo/bin`, Homebrew, npm), and the Language servers dialog (status bar) installs a missing one with the language's own tool (`go install`, `rustup`, `npm`, `brew`), or links to that tool when it's missing too. Other languages get highlighting and "Search the repository for this".
- **AI.** Each PR has one agent chat (Ask AI in the header opens it beside the editor). The session is keyed `review:<repo>#<n>` (`sessions.key`, migration 006), runs in the PR's worktree, and carries a `brief` about the PR that's added to the agent's instructions each turn. The brief tells the agent to link code as `path#L12`, and those links open in tab's editor or in VS Code (toggle in the panel; Ctrl/Cmd click does the other). The new-comment composer adds **Ask AI**, which sends the selected lines with your question (or "What does this code do?"), and **Add to fix list**. Review threads also have a Fix list button. The fix list is kept per PR on this computer and shown in a banner under the header, where **Run fix list** sends every queued fix, with its lines and code, to the agent in one message. The agent edits the checkout and doesn't commit unless asked.
- **Data.** One GraphQL query reads the PR, checks rollup, review threads (with resolved / outdated state), the pending review and viewed states; REST adds rename info. Every call goes through the gateway and is audited. Actions apply optimistically, run one at a time, and refetch the PR. Sample PRs simulate the boot and keep edits local.

## Sandbox (built)

Everything tab starts on someone's behalf goes through `sandbox::Sandbox::command` with a `Policy` (`sandbox/mod.rs`): what's read-only (`protect`, `/` for everything), what's writable inside that, what's unreadable (`hide`), and the network (`Any`, `None`, or a domain list). It's on by default and set in Settings → Sandbox.

| Who | Policy | Writes | Hidden | Network |
|---|---|---|---|---|
| A person's chat (Claude Code, OpenCode) | `chat_policy` | everywhere except tab's data and config, a dev build's own checkout (its `.git` stays writable for `git worktree add`), shell startup files, `~/.ssh` | cloud credentials, browser profiles, tab's own secrets: `config.json`, `account.json` (the tab cloud sign-in), `tab.db`, `hub.db` and `hub-tls/` | any |
| A chat about a PR, in its checkout; a workflow's agent job; every chat on a cloud runner | `review_policy` | as a person's chat, but the whole work folder is read-only except the checkout | the above plus git, SSH, npm and cargo credentials | any |
| Language servers on a PR checkout | `isolated_policy` | the checkout, build caches, temp | as review | registries in `lsp_domains` |
| Workflow `command` steps | `isolated_policy` | the step's folder (a fresh one per run by default), caches, temp | as review | the step's `network` list (empty is none) |

**Enforcement.** macOS uses Seatbelt (`sandbox-exec` with a generated profile, `seatbelt.rs`); Linux uses bubblewrap (`bwrap.rs`), detected by actually running it. Only `sandbox-exec`'s "already sandboxed" exit (71) counts as running inside another sandbox (`Inherited`, which Settings shows as not enforced); any other failure is no sandbox at all. A domain list starts a small CONNECT proxy for that process (`proxy.rs`) and points `HTTPS_PROXY` and friends at it; on macOS the profile also blocks every other outbound connection. On Linux the process gets its own network namespace, and reaches the proxy through a Unix socket bound into the sandbox and a bridge in front of the program (`tab __tab_bridge`, `bridge.rs`); without the bridge it gets no network rather than all of it. Under bubblewrap every process also gets its own PID namespace and `/proc`, and on macOS the profile denies reading other processes' arguments and environment (`kern.procargs`), so nothing reads tab's own environment; release builds of tab and the cloud runner are also non-dumpable (`harden_process`). bubblewrap can only cover paths that exist, so hidden credential files that are fine to find empty (`.git-credentials`, `.netrc`, `account.json`) are made empty, readable by the user only, before a process starts. Claude Code's `Bash` runs without asking only where the network limit holds too. Windows has no file or network enforcement; there, and everywhere, two layers still apply:
- *Environment* (`env.rs`): variables that look secret (`*_TOKEN`, `*_KEY`, `AWS_*`, ...) are dropped. A person's chat keeps model keys (and `GH_TOKEN` only when tab isn't brokering GitHub for it); review chats keep model keys; language servers and commands keep nothing.
- *Process trees* (`tree.rs`): a process group on Unix, a job object on Windows, so stopping a chat or a step ends everything it started.

**The cloud runner** (`crates/tab-runner`) runs agents on prompts that may carry anyone's PR description or email, next to the hub's secrets, so it is stricter: it installs a sandbox that refuses to start anything unenforced (`require_enforcement`), hides its data folder, and keeps the hub's secrets in memory. Harness keys reach each agent through `Sandbox::set_spawn_env` (filtered by trust like the rest of the environment), never the runner's own environment; `TAB_RUNNER_TOKEN` is removed from it as soon as it's read. Every chat there gets the review policy, so no GitHub token reaches an agent: the runner clones (GitHub `owner/name` only) and pushes the branch the agent committed to itself, with `git/token` handed to that one git command through a credential helper that answers only github.com. It says hello as `runner-<hub id>` and starts from an empty data folder each time. Agent steps mark text from the item or trigger as data in the prompt (`<data from="item.body">`), and refuse a `repo` templated from it.

OpenCode runs one server per chat (`agents/opencode.rs`, at most four, the least recently used idle one stops first), started in the chat's folder under that chat's policy, shims and token; a server the person runs themselves is shared. Claude Code also gets the policy as its own permission rules (`--settings`: deny `Edit` on protected paths and `Read` on hidden ones; where the sandbox is enforced, allow `Bash`, and elsewhere `gh` and `git`), which holds on Windows too. It never changes the person's permission mode: `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB` is left out because it turns auto mode into default. rust-analyzer's build scripts and proc macros, which run the PR's code, stay on only where the network is enforced (macOS).

**Harnesses are data** (`agents/harnesses.json`, read by `agents/catalog.rs`): each entry has the harness's name, icon, command, capabilities (strict mode, permission modes, attaching to a person's server, spend caps), its models (a fixed list, or a command that lists them plus the ones shown first), and how to install and update it. Onboarding, Settings, the chat's model picker and the strict pill all read it, so a new harness is an entry plus its adapter. Each harness keeps its own model (`AgentConfig::models`), since a chat stays on the harness it started on; the titler can run on any installed harness. Onboarding installs a missing harness in one click (npm when it's on PATH, else its own installer) and keeps looking for it, including in its installer's folder when the app's PATH predates the install.

**Strict mode** (`AgentConfig::strict`, on by default, offered in onboarding): chats load only tab's MCP server, never the person's own agent setup. Claude Code gets `--strict-mcp-config --setting-sources project,local` (no user MCP servers, claude.ai connectors, user hooks, plugins, allow rules or `~/.claude/CLAUDE.md`; the repo's own config still loads) plus `ENABLE_CLAUDEAI_MCP_SERVERS=false`, and its init line is checked: any MCP server but `tab`, or an account-synced plugin, stops the turn. OpenCode gets every MCP server from the person's global config turned off in `OPENCODE_CONFIG_CONTENT`, `OPENCODE_DISABLE_CLAUDE_CODE`, and a `GET /mcp` check before the first turn; a server the person runs themselves can't be held to it, so the chat shows "Not strict". Chats on code nobody here wrote are always strict and load no settings at all (`--setting-sources ""`), since a pull request's `.claude/settings.json` hooks would otherwise run as soon as the chat starts. Each adapter must both isolate and report what loaded to count as strict.

**CLIs through connections** (`sandbox/broker.rs`, `tab_domain::cli`). A connection lists the command-line tools it signs in on its descriptor (`clis`): GitHub declares `gh` and `git`. Each `CliInfo` says how the token reaches the tool and has rules that sort commands into local, read, write, destructive or blocked (`gh pr merge` is destructive; `gh auth` and gh extensions are blocked; `gh api` goes by HTTP method). When the sandbox is on, every agent chat gets:
- *Shims* in its own folder, first on PATH (and again through `CLAUDE_ENV_FILE`, since Claude Code rebuilds PATH from the person's shell setup): `gh` runs `tab __tab_cli gh …`, which posts to `/cli` on tab's local server. tab asks the person before destructive commands (and before any write in a review chat), runs the real `gh` in the chat's own sandbox with the connection's token in that process only, writes it to the API audit log, and sends `CliChanged` for the chat's activity. Reads aren't cached.
- *Pages changed*: each CLI's metadata names the page that shows each resource its rules touch (`CliInfo::pages`, like `pull` → `https://github.com/{repo}/pull/{number}`; a push touches the repo's page). After a write, tab calls `Engine::pages_changed(connector, pages)`: the connector's `ConnectorFactory::forget_pages` drops what it cached about those pages (GitHub's two-minute link summaries), queue items at them get fresh previews, and the app hears `PagesChanged`, invalidating `["link-preview", page]` plus whatever the connection lists in `pageQueries` (`src/lib/query.tsx`). A connector that notices a change itself (a webhook, a poll) calls `ConnectorContext::changed(pages)` for the same refresh. A new connection gets live previews by filling in `pages` and, if it caches, `forget_pages`.
- *A git endpoint*: the chat's git config (`GIT_CONFIG_*`) rewrites `https://github.com/`, `git@github.com:` and `ssh://git@github.com/` to `/git/<chat>/github/…`, which forwards to GitHub with the token. git runs in the chat as usual, hooks and all, and never holds the token. A push's ref updates are read first: a delete, or an update the checkout says isn't a fast-forward, asks the person; a no comes back as git's own `! [remote rejected]`.
- *A credential helper* (`git-credential-tab`) in place of the person's: other hosts get credentials from the person's own helpers through tab, GitHub gets none.
- *What it can't do*: start the real `gh` or git's credential helpers (blocked in the profile), read gh's config or git's credential files, or see a GitHub token in its environment. Settings → Sandbox can also keep SSH keys out of chats.
- *Its own token*: `mcp::chat_token` derives one per chat from tab's token, and `/cli` and `/git` accept only the token of the chat they name. Each chat's folder is hidden from every other chat.
On Windows nothing stops an agent from starting `gh.exe` directly; the shims and git config still cover what agents do by default.

**Containers.** A `command` step with an `image` runs in Docker or Podman (`container.rs`): only its folder mounted at `/work`, no capabilities, `no-new-privileges`, the person's uid, and `--network none` unless it lists hosts. That's the strong option on Windows and what a self-hosted server image would use for agents and commands.

## Security

- Secrets live in `config.json` for now and are masked in the UI; moving them to the OS keychain is phase 2.
- Item text is passed to models as data with explicit instructions not to follow it; the user's own instructions are the only trusted prompt input.
- The MCP server binds to localhost and requires a token; configuration tools can be disabled.
- Agents, language servers and workflow commands run in the sandbox (see Sandbox): they can't change tab or read its secrets, and code from pull requests can only change its own checkout.
- Automations and agent actions that change external systems are audited and can require confirmation.

## tab cloud accounts (built: setup; designed: the rest)

Signing in (Settings → Hub, or the Hub step of onboarding) is Supabase OAuth with Apple or Google (`tab_core::account`). Picking tab cloud then sets up the person's **private hub**: their own instance of the hub Durable Object, with its own SQLite and its own data key. The cloud side is in the private `tab-infra` repo (edge Worker, hub Durable Object, accounts in D1); the app only calls its edge Worker (`tab_core::cloud`, `TAB_CLOUD_URL`, default `https://api.tabqueue.com`):

- `POST /v1/hub` creates the account and its one 14-day trial and sets up the hub. Idempotent; it retries a failed setup. `tab_core::cloud::set_up_hub` calls it and waits while another request finishes the setup.
- `GET /v1/hub` reports `NONE`, `PROVISIONING`, `READY` or `FAILED`, with the region and plan.
- `GET /v1/hubs` lists every hub the person belongs to: their personal one first, then team hubs, each with their role. `GET /v1/hubs/{hub_id}` reads one. A hub id in a path is only a request: the Worker checks the person's membership and role, and a hub they don't belong to is a 404 like one that doesn't exist.

The API is defined once, in `crates/tab-protocol/proto/tab/cloud/v1/account.proto`. Bodies are binary Protobuf (`application/x-protobuf`), errors are an `Error` with an `ErrorCode`. tab-infra depends on `tab-protocol` by git rev and builds its responses from the same Rust types; the app's `cloud_*` Tauri commands pass the bytes through and the UI decodes them with the generated TypeScript (`src/lib/protocol/cloud.ts`). The same additive-only rule as `hub.proto` applies, and tab-infra bumps its rev to pick up a change.

Onboarding waits on this before Continue, with Try again on failure and See plans when the trial has ended. The access token is refreshed before each call when it's within a minute of expiring.

**Syncing with the cloud hub.** The hub Durable Object runs `tab_hub::Hub` on its own SQLite (the same `SqliteOn` store as the desktop), behind `GET /v1/hubs/{hub_id}/sync`, a WebSocket speaking hub.proto with the access token in `Authorization`. The edge checks membership and forwards the caller's role; the hub checks every request against it (guests read, members write, admins and owners may `Import`). `tab_core::cloud_link::CloudLink` is the app's end: one socket, reconnecting with a fresh token, request/reply calls that block (sync runs on a blocking thread), and pushes that wake sync.

**Moving** (Settings → Hub → Move to tab cloud, `LocalHub::move_to_cloud`): flush `tab.db` into `hub.db`, then `Import` hub.db's whole log into the empty cloud hub, page by page, keeping sequence numbers, so `tab.db`'s sync cursor stays valid. From then on `tab.db` syncs with the cloud hub instead of `hub.db`, which keeps its copy from the move. A cloud hub that already has work (another computer moved first) is joined instead: `tab.db` takes its rows and adds what it lacked. `hub.cloud_hub` in config remembers the move; at launch sync waits until the cloud hub connects rather than writing to `hub.db`, whose numbering no longer matches. Moving back, device pairing and live sessions through the cloud are next.

**Dev builds** (`pnpm dev:tab`) never use production: they read `~/.config/tab-dev/cloud.env`, which tab-infra's `scripts/dev.sh` writes when it starts a local tab cloud (Supabase in Docker plus both Workers under `wrangler dev` on `127.0.0.1:8787`; tab-infra's `docs/DEV.md`). Without it sign-in is off; `TAB_DEV_CLOUD=prod` points a dev build at production on purpose.

## Server (later)

Sync across devices, team-shared automations and graph, and a webhook receiver so GitHub and Slack push instead of being polled (which also removes most rate-limit pressure). The local app stays the source of truth for a single user and works offline. The pieces it builds on (tab-domain, tab-protocol, tab-store, tab-hub) are in the repo; see Layout.

## Phased plan

| Phase | Scope |
|---|---|
| **0. Foundation** (this PR) | Core crate, connector framework, GitHub + sample connectors, deterministic ranking + Haiku layer, gateway with cache/limits/audit, SQLite with FTS5 trigram and graph tables, design system, Work / Connections / Settings. |
| **1. Agents + MCP** | MCP server (read, act, configure). Agent session runner with PTY streaming, xterm.js, status adapters (generic, MCP, Claude Code hooks, OpenCode). Agent items in the queue. |
| **2. Graph + review** | Graph view module. GitHub review module with Monaco diffs and comments. Secrets in keychain. |
| **3. Workflows** (built) | Connector actions, deterministic runtime on the event stream with loop/dedup/rate guards, visual builder, AI assistant over MCP, item timelines. Next: agent steps, approval cards for destructive steps, webhook triggers. |
| **4. More connectors** | Slack, Gmail, Zoom summaries, Jira/Linear; out-of-process connector protocol; gateway budgets and CLI provider. |
| **5. Server** | Sync, webhooks, team features. |
