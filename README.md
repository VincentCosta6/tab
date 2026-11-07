# tab

**TAB Annihilates Backlogs.**

A modular work queue for engineers, designers and PMs. Connectors bring in everything that needs you (GitHub reviews and CI failures, Slack mentions, email, meeting action items, AI agents waiting on input). tab ranks it deterministically, refines it with your own instructions through Claude Haiku, and gives you one keyboard-driven place to work through it.

Built with [Tauri 2](https://tauri.app) (Rust core) and React.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design and the phased plan.

## What works today

- **Work**: a ranked queue grouped by urgency, with search (SQLite FTS5 trigram), filters by app, and real product marks (Slack, GitHub, Gmail, Zoom, Jira). Hover a row to snooze or finish it; the actions overlay the right edge so nothing shifts. Keyboard: `j`/`k` move, `e` done, `s` snooze (then `1`–`4`), `o` open, `r` review a PR, `n` new chat, `/` search.
- **Chat on any item**: opening an item shows a chat with your agent. The context bar above it rolls up everything around the item: the item itself with its cached rich preview, pinned and attached **Context** (sent with every message), and **Found** (things the agent filed through MCP, plus knowledge-graph neighbors) that you promote with one click. The first message starts a session for that item; later messages resume it. Tool approvals appear in the chat.
- **New chat**: "work on the checkout incident" becomes a tab task with its own session. The agent can find the ticket or PR in your queue and attach it as context.
- **Agents**: Claude Code (driven through its official CLI with your own login) or OpenCode (tab starts `opencode serve` and follows its event stream). Streaming text, tool calls and results, cost, stop, and approvals routed to you.
- **tab's own connection**: tasks and scheduled tasks with priority (low → urgent) and due times, created by you or by agents.
- **MCP server**: local, token-protected: `tab_search`, `tab_get_item`, `tab_add_found`, `tab_attach_context`, `tab_create_task`, and `approve` (Claude Code's permission prompt tool).
- **Connections**: Installed and Marketplace tabs, a detail page per connection (settings, kinds of work, declared rate budget), and API usage: live budgets per bucket, provider-reported limits, usage by caller and purpose, and the full audit log.
- **Rate budgets for every connector**: connectors declare their provider's limits; the gateway tracks a sliding window of real usage and bursts, keeps interactive calls flowing, and defers background work when headroom drops below 30% or after a burst.
- **Cached rich previews**: previews live in SQLite and in a persisted React Query cache, so rendering never waits on an API. A background refresher updates stale previews only when budgets allow.
- **GitHub module**: a Reviews tab and a built-in VS Code (Monaco) side-by-side diff with line comments posted to the PR. Sample PRs work without a token.
- **Onboarding**: first launch walks through picking and checking an agent, then connections.
- **Settings**: profile, deterministic ranking rules, the AI layer, agent, and MCP details.

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

### Working on tab from inside tab

`pnpm tauri dev` hot-reloads the UI and rebuilds the backend on every save, so an agent chat that edits the checkout tab is running from can blank the window or restart the app mid-turn. Dev builds tell agents which checkout that is and to use a worktree instead. To do it by hand:

```sh
git worktree add ../tab-<topic> -b <branch> origin/main
```

For day-to-day use, run a built copy (`pnpm tauri build`, then install the bundle from `target/release/bundle`) and keep `pnpm tauri dev` for testing changes. Both use the same config and `tab.db`, so quit one before starting the other.

## Test

```sh
cargo test -p tab-core   # core: store, search, graph, gateway, ranking, engine
cargo test -p tab-domain -p tab-protocol -p tab-hub   # shared logic, protocol, hub
scripts/storage-tests.sh # full storage suite (conformance, property tests); --perf, --cov
pnpm typecheck           # UI
pnpm proto               # regenerate TypeScript from the .proto after changing it
```

Config and data live in the OS app config/data directories (`config.json`, `tab.db`).

## Layout

```
crates/tab-domain    pure logic shared by every place a hub runs (builds for Wasm)
crates/tab-protocol  hub messages: Protobuf, no gRPC (builds for Wasm)
crates/tab-store     hub storage trait, SQLite and in-memory backends, conformance suite
crates/tab-hub       the hub: answers clients on any store; local <-> cloud migration
crates/tab-core      desktop engine: connectors, gateway, ranking, SQLite store (no Tauri dependency)
src-tauri         desktop shell: commands + event forwarding
src               React UI: design system (src/design) and modules (src/modules)
docs              architecture and plan
```

## License

[FSL-1.1-MIT](LICENSE.md), the Functional Source License. You can read, run, modify and redistribute tab for anything except offering a competing commercial product or service. Each version becomes MIT two years after it's released.
