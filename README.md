# tab

A modular work queue for engineers, designers and PMs. Connectors bring in everything that needs you (GitHub reviews and CI failures, Slack mentions, email, meeting action items, AI agents waiting on input). tab ranks it deterministically, refines it with your own instructions through Claude Haiku, and gives you one keyboard-driven place to work through it.

Built with [Tauri 2](https://tauri.app) (Rust core) and React.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design and the phased plan.

## What works today

- **Work**: a ranked queue grouped by urgency, with search (SQLite FTS5 trigram), filters by app, a detail pane with rich previews, the item's knowledge-graph entities and related items. Keyboard: `j`/`k` move, `e` done, `s` snooze (then `1`–`4`), `o` open, `/` search.
- **Connections**: connector cards generated from connector metadata, GitHub setup, live status, rate limits per provider, API usage grouped by caller and purpose, and the full audit log of outbound calls.
- **Settings**: your profile, deterministic ranking rules (important people, muted sources, keywords), and the AI layer (API key, model, your instructions).
- **Connectors**: GitHub notifications (real) and Sample data (on by default). Slack, Gmail, Zoom, Jira and Agent sessions are described and shown as coming soon.

## Develop

Prerequisites: Rust (stable), Node 20+, pnpm, and the [Tauri system dependencies](https://tauri.app/start/prerequisites/) for your OS.

```sh
pnpm install
pnpm tauri dev
```

Optional environment variables (the Settings and Connections tabs can set these too):

```sh
export ANTHROPIC_API_KEY=...   # enables the Haiku ranking layer
export GITHUB_TOKEN=...        # enables the GitHub connector (notifications + repo read)
```

UI-only iteration without the desktop shell: `pnpm dev` and open http://localhost:1420. The UI falls back to an in-memory mock backend (`src/lib/mock.ts`).

## Test

```sh
cargo test -p tab-core   # core: store, search, graph, gateway, ranking, engine
pnpm typecheck           # UI
```

Config and data live in the OS app config/data directories (`config.json`, `tab.db`).

## Layout

```
crates/tab-core   connectors, engine, gateway, ranking, SQLite store (no Tauri dependency)
src-tauri         desktop shell: commands + event forwarding
src               React UI: design system (src/design) and modules (src/modules)
docs              architecture and plan
```
