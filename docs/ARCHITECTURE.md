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

```
crates/tab-core/        UI-agnostic core. No Tauri dependency, fully unit tested.
  model.rs              Domain types: IncomingEvent, QueueItem, Block, EntityRef, Ranking, descriptors
  connector.rs          Connector + ConnectorFactory traits, ConnectorContext
  connectors/           github.rs, demo.rs, planned.rs (Slack, Gmail, Zoom, Jira, Agents described)
  engine.rs             Wires connectors → store → ranker → events
  gateway.rs            Cache, rate limits, audit for every API call
  ranker/               rules.rs (deterministic), ai.rs (Haiku layer)
  store/                SQLite: items, graph, http cache, audit; migrations/
  config.rs             TabConfig (profile, ranking, connectors map)
src-tauri/              Desktop shell: Tauri commands + event forwarding
src/                    React UI
  design/               Design system: tokens.css, components, icons, blocks, markdown
  modules/              registry.ts + built-in modules (work, connections, settings)
  lib/                  api.ts (Tauri or browser mock), types.ts (mirrors model.rs)
```

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

**Next (designed)**:
- *Budgets*: per-connector share of a provider's limit, and priority classes (interactive preview > background poll > backfill). Background work yields when remaining quota drops below a reserve.
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

The UI is a set of **modules** (`src/modules/registry.ts`). A module contributes tabs today; next it can contribute item surfaces (by kind prefix), settings panes, automation actions and custom block renderers. The core app is itself three modules: Work, Connections and Settings. GitHub's review tool will be a module that adds a "GitHub" tab and a review surface for `github.review_requested` items.

The design system (`src/design/`) is the only source of styling:
- `tokens.css`: color, type, spacing, radius tokens with light and dark themes. Six semantic **tones** (neutral, accent, info, success, warning, danger) are the only way connectors influence color.
- `components.tsx`: Button, Badge, SourceIcon, ScorePill, Card, Section, PageHeader, EmptyState, inputs, Toggle, Tabs, Meter, StatusDot.
- `blocks.tsx`: one renderer per `Block` variant.
- `icons.tsx`: the icon set connectors reference by name.

Rule of thumb: a new screen should be composable from these; if it isn't, add the missing component to the design system first.

## Automations (designed)

"When something happens in tab, do something." Configurable in a visual builder, over MCP, or as JSON.

```
Trigger            →  Conditions               →  Actions
item ingested         kind = github.ci_failed      deterministic: snooze, mark done, set score,
item ranked           score ≥ 80                     webhook, run command, connector action
item snoozed          actor in [...]                 (comment on PR, re-run CI, reply in Slack)
item done             context matches /…/        agent: start an agent session with a prompt
agent status changed  time window                    template and the item as context
schedule (cron)
```

- Stored in `automations` (versioned JSON graph of nodes and edges) with runs in `automation_runs` (inputs, each step's output, duration, error). Runs are audited like API calls; actions that call APIs go through the gateway.
- Triggers are `EngineEvent`s, so the runtime is just another subscriber.
- The **visual builder** is a node canvas (trigger → condition → action) generated from action descriptors, the same metadata pattern connectors use. Connectors can contribute actions.
- Safety: dry-run mode, per-automation rate limits, and confirmation for destructive actions until the automation is marked trusted.

## MCP server (designed)

A local MCP server (Streamable HTTP on localhost with a per-install token, plus stdio for CLI agents) so any agent (Claude, ChatGPT, OpenCode) can use and configure tab.

- **Read**: `queue_list`, `queue_search`, `item_get` (with preview blocks), `graph_search`, `graph_neighbors`, `item_related`.
- **Act**: `item_done`, `item_snooze`, `link_context` (attach an artifact or note to items/entities), `report_status` (for agent sessions).
- **Configure**: `connectors_list`, `connector_configure`, `rules_get` / `rules_update`, `instructions_set`, `automations_list` / `automation_upsert` / `automation_test`.
- **Resources**: `tab://item/{id}`, `tab://entity/{kind}/{key}`.

Configuration tools write the same config and tables the UI does, so "set up an automation that snoozes Dependabot PRs until Friday" in chat produces exactly what the builder would.

## Agent sessions (designed)

Agent-agnostic by design. The only hard requirement is that the agent's terminal can be streamed into tab and answered from tab.

- **Runner**: spawn any CLI agent (`claude`, `codex`, `opencode`, `aider`, …) in a pseudo-terminal (`portable-pty`) owned by the Rust core. Output streams to the UI over a Tauri channel into an `xterm.js` terminal; keystrokes go back to the PTY. Sessions survive window reloads and can be detached.
- **Status**: `working`, `needs_input`, `needs_review`, `monitoring`, `done`, `failed`. Adapters improve on a generic detector:
  - *Generic*: prompt and idle heuristics on the PTY stream.
  - *MCP*: the agent calls `report_status` on tab's MCP server (works for any MCP-capable agent).
  - *Native*: Claude Code hooks, OpenCode's server/event API, the Claude Agent SDK.
- `needs_input` and `needs_review` become queue items (kinds `agent.needs_input`, `agent.needs_review`) ranked like everything else; the detail pane embeds the live terminal, and `Choices` blocks become one-click answers.
- Transcripts are stored as files and linked into the graph (agent_session entity ↔ repo, PR, items it touched).

## GitHub review tool (designed)

A module that adds a review surface for PR items and a GitHub tab.

- **Monaco** (the VS Code editor) diff view with a file tree, syntax highlighting for common languages out of the box, and language intelligence later via LSP sidecars.
- Inline comments, suggestions and a pending review submitted in one batch. All reads cached through the gateway (PR files keyed by head SHA, so they never go stale); writes audited.
- Uses the same tokens and components as the rest of the app; Monaco gets a theme generated from `tokens.css`.

## Security

- Secrets live in `config.json` for now and are masked in the UI; moving them to the OS keychain is phase 2.
- Item text is passed to models as data with explicit instructions not to follow it; the user's own instructions are the only trusted prompt input.
- The MCP server binds to localhost and requires a token; configuration tools can be disabled.
- Automations and agent actions that change external systems are audited and can require confirmation.

## Server (later)

Sync across devices, team-shared automations and graph, and a webhook receiver so GitHub and Slack push instead of being polled (which also removes most rate-limit pressure). The local app stays the source of truth for a single user and works offline.

## Phased plan

| Phase | Scope |
|---|---|
| **0. Foundation** (this PR) | Core crate, connector framework, GitHub + sample connectors, deterministic ranking + Haiku layer, gateway with cache/limits/audit, SQLite with FTS5 trigram and graph tables, design system, Work / Connections / Settings. |
| **1. Agents + MCP** | MCP server (read, act, configure). Agent session runner with PTY streaming, xterm.js, status adapters (generic, MCP, Claude Code hooks, OpenCode). Agent items in the queue. |
| **2. Graph + review** | Graph view module. GitHub review module with Monaco diffs and comments. Secrets in keychain. |
| **3. Automations** | Runtime on the event stream, deterministic and agent actions, visual builder, MCP configuration. |
| **4. More connectors** | Slack, Gmail, Zoom summaries, Jira/Linear; out-of-process connector protocol; gateway budgets and CLI provider. |
| **5. Server** | Sync, webhooks, team features. |
