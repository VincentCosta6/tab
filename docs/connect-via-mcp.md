# Connections via MCP — design sketch

Status: phase 1 built (sign-in, keychain, the `/mcp/<id>` proxy, Add by URL,
the catalog, per-tool allowed / ask / off for chats). Phase 2 built (import
from Claude Code, local-command servers run by tab, static header keys in the
vault, the cloud eligibility check shown per server). Phase 5 built (Sign in
with Google / Microsoft for email), along with the rest of phase 1's "keychain
for every secret". Phases 3 and 4 are still designs.

What phase 5 leaves out, on purpose:
- It needs a Google "Desktop app" OAuth client and a Microsoft Entra public
  client (redirect `http://127.0.0.1/oauth/callback`) registered for tab.
  Their ids are build variables (`TAB_GOOGLE_CLIENT_ID`,
  `TAB_GOOGLE_CLIENT_SECRET`, `TAB_MICROSOFT_CLIENT_ID`); a build without
  one doesn't offer that provider. Google's `https://mail.google.com/` is a
  restricted scope: until Google verifies the app, only its test users can
  sign in.
- Email signed in this way stays on this computer. tab cloud can't refresh
  it until phase 4 puts sign-ins in the hub.
- One mail sign-in, for the one email connection.

All connection secrets (IMAP passwords, GitHub tokens, the ranker's API key)
are now in the vault ([`crate::secrets`]); `config.json` keeps the rest. The
first launch after the update moves them out of an older `config.json`. The
MCP bearer token stays in `config.json`: it's written into every agent's MCP
config anyway.

What phase 2 leaves out, on purpose:
- The hint in the strict pill when a turn shows blocked servers.
- Cloud eligibility is only shown; nothing runs in the cloud until phase 4.
- Local servers run unsandboxed as tab's children (as Claude Code runs
  them), with tab's own secrets scrubbed from their environment.
- Legacy SSE servers and `headersHelper` scripts can't be imported.

What phase 1 leaves out, on purpose:
- Only the person's own chats get remote tools. Reviews and jobs (anything
  under the review policy) never do, so there's no "In cloud jobs" column yet.
- Sign-ins stay on each computer; nothing goes to the hub.
- Only secrets of MCP connections moved out of `config.json`; the other
  connectors' secrets moved with phase 5.
- Discovery uses Dynamic Client Registration. A Client ID Metadata Document
  at tab.dev comes later (every catalog server supports DCR today).
- GitHub's MCP server isn't in the catalog: it has no self-registration, so
  it needs a registered GitHub app first.

## Goal

Let people connect services by signing in once, with no API keys, tokens or
app passwords. Also let them bring the MCP servers they already use in
Claude Code into tab, so those servers work in strict chats and cloud runs.

## Principle: users see what a connection does, not how it's built

Every connection offers up to two capabilities, and those are the only
things the UI shows:

| Capability | What it means | Built with |
|---|---|---|
| **Fills your queue** | Work shows up in tab: ranked, previewed, and marked done when it's finished upstream | Native connector, or MCP plus a sync recipe |
| **Agent tools** | Agents in chats and jobs can act there as you | MCP server, through tab's proxy |

Rules:
- **One card per service.** GitHub is a single card with both capabilities,
  even though native sync and GitHub's MCP server are separate pieces
  underneath.
- **Group by the existing shelves** (Code, Planning, Communication), not by
  how you sign in. The sign-in method only shows in the button: "Sign in
  with Linear" or "Enter IMAP details".
- **Use the word "MCP" only under Advanced:** "Add a custom MCP server by
  URL" and "Import from Claude Code". The people who use those know the term.
- **Capabilities can light up later.** A service that starts with agent
  tools only gains "Fills your queue" when tab ships a recipe or native
  connector for it. The person doesn't reconnect, and the card doesn't change.

Strategy: use native connectors for the high-volume sources where queue
quality matters most (GitHub, email, later Slack). Use MCP for everything
else, so the long tail of services is one sign-in away.

## What exists today (origin/main)

- `ConnectorFactory` / `Connector` (`crates/tab-core/src/connector.rs`):
  `run` (sync), `preview`, `act`, `on_done`, `is_resolved`.
- `ConnectorConfig { enabled, settings, runs_on }`: connections already
  choose whether they run on this computer or in tab cloud. Cloud secrets
  live in the hub as `connection/<id>/<field>` (`cloud.rs`), and
  `tab-runner` reads them into memory only.
- **Strict mode** (`agents/claude.rs`, `isolation_args`): the person can opt
  in, and it is always on for code they don't own (PR checkouts, cloud
  clones, jobs). It passes `--strict-mcp-config --setting-sources
  project,local` and sets `ENABLE_CLAUDEAI_MCP_SERVERS=false`, so only tab's
  own MCP server loads. `unexpected()` stops the turn if anything else
  loads anyway. Settings → Agents describes it as: "Chats load only tab's
  connections, never your personal MCP servers…"
- The cloud runner gives agents the review policy: no git token, and
  everything runs in the sandbox.

That last point decides the design: **strict chats and every cloud run can
only see tab connections.** The way to get someone's MCP servers there is
to make them tab connections, not to relax strict mode.

## 1. Sign-in (MCP authorization)

tab is an OAuth public client:

1. Call the server with no token. It answers `401` with a
   `WWW-Authenticate: … resource_metadata=…` header.
2. Read Protected Resource Metadata (RFC 9728), then the authorization
   server's metadata (RFC 8414 / OIDC).
3. Register the client without any setup on our side: a Client ID Metadata
   Document (`https://tab.dev/oauth/client.json`) where the server supports
   it, otherwise Dynamic Client Registration (RFC 7591). Keep a static client
   id per provider as a fallback.
4. Authorization code + PKCE (S256), with `resource=<server url>` (RFC 8707),
   in the system browser. The redirect goes to a loopback
   `http://127.0.0.1:<port>/oauth/callback` on tab's existing axum server,
   with an optional `tab://oauth` deep link.
5. Tokens go to the **OS keychain** (`keyring`). Do the planned
   secrets-to-keychain move for all connectors at the same time.
6. Refresh on expiry or a 401, and handle rotated refresh tokens. If refresh
   fails, the status becomes `NeedsSignIn` with a Reconnect button,
   not Failed.

## 2. Agent tools through tab's proxy

Third-party tokens are never written into `mcp.json`. tab's MCP server
gets `POST /mcp/<conn-id>`, which:
- checks tab's chat token and `X-Tab-Session`,
- adds a fresh token, or the server's static headers, and forwards over
  Streamable HTTP (including SSE),
- logs to the gateway audit log and counts against rate budgets.

Local-command (stdio) servers run as a child of tab, and the same route
reaches them over stdio. Agents never start them directly.

`claude.rs` adds one `--mcp-config` entry per connection that's allowed in
the session. Because these are `--mcp-config` servers, they **work in
strict mode**. Extend `unexpected()` to accept the names of tab
connections.

**Permissions:** each tool is set to Allowed, Ask or Off, separately for
chats and for cloud jobs:
- Read-only tools (`annotations.readOnlyHint`): Allowed in both.
- Write tools: Ask in chats, **Off in cloud jobs**. Cloud jobs act on PRs
  and email from other people, so a prompt injection there should never
  write as you by default.

"Use in: all chats / chosen projects" limits how many tools load into each
session.

## 3. Queue sync via recipes

MCP has no standard way to say "what's new for me", so each known server
gets a small recipe:

```rust
struct SyncRecipe {
    tool: &'static str,                     // "list_my_issues"
    args: serde_json::Value,                // {"updatedSince": "{cursor}"}
    map: fn(&Value) -> Vec<IncomingEvent>,
    resolved: Option<fn(&Value) -> bool>,   // feeds is_resolved()
    interval: Duration,
}
```

`McpConnector::run()` calls the tool through the proxy under gateway
budgets, emits events using the remote id as `external_id`, saves a
cursor, and calls `ctx.synced()`. `act()` maps workflow steps to tool calls.
Without a recipe, the connection is agent tools only.

## 4. Import from Claude Code (new)

Claude Code already has the person's servers. Read them, and offer to turn
them into tab connections.

**Sources**
- `~/.claude.json`: user-scope `mcpServers`, plus per-project entries
  under `projects.<path>.mcpServers`.
- `.mcp.json` in the repos tab knows about (project scope).
- claude.ai connectors: tab can't read their configuration. List them
  from the `mcp_servers` names in a non-strict session's init line (prefix
  `claude.ai `), and match them to catalog entries by name.

**How each kind imports**

| Kind | Import | Where it can run |
|---|---|---|
| Remote HTTP with OAuth | URL only. **Sign in again in tab**: Claude Code's tokens are its own, so don't read them | This computer and cloud |
| Remote HTTP with static headers or keys | URL plus headers. Secrets go to the keychain | This computer. Cloud only if the person allows it (uploads the key to the hub) |
| Local command, portable (`npx`/`uvx` package, no local paths) | command, args, env | This computer. Cloud if the runner image has the runtime |
| Local command that touches local paths, localhost or the LAN | command, args, env | **This computer only** |
| claude.ai connector that matches a catalog entry | Becomes that tab connection. Sign in once | Both |
| claude.ai connector with no match | Can't import. It stays in non-strict chats | — |

**Cloud eligibility check** (shown as "✓ This computer ✓ Cloud" per row):
the server can't use `localhost`, `127.*`, `*.local` or private IP
addresses; a command can't use an absolute path or a path under the
home folder; and secrets need explicit permission.

**Discovery:** a banner in Connections ("You have 5 MCP servers set up in
Claude Code; strict chats and cloud runs can't see them"). Also a hint in
the strict pill in chats when a turn shows the person has servers that
strict mode blocked.

Importing copies; nothing in Claude Code changes. Re-running the import
shows what's new or changed. Don't auto-sync both ways: the two copies
would drift, and it would be unclear which one wins.

## 5. tab cloud

- Uses the existing `runs_on`. "Where it runs: this computer and tab cloud"
  versus "only on my computers".
- Cloud plus OAuth: the **refresh token** is stored as a hub secret
  (`connection/<id>/oauth`), encrypted at rest and readable only by the
  runner. The runner refreshes it. Its access tokens stay in memory and
  reach agents only through the proxy, the same way as on desktop.
- Cloud jobs use only the connections that run in the cloud, with the
  cloud permission column.
- **When a cloud job needs something it doesn't have**, the run reports
  which connection or tool was missing. The item shows a card with
  one-click fixes: "Allow in cloud", "Ask me each time" for a blocked
  write, or "Run this job on my computer" for servers that only work
  locally. That's the prompt that gets people to add their MCP servers
  to cloud runs, at the moment it matters, not during setup.
- Sync dedupes on `external_id`, so a computer and the runner both polling
  is harmless. Let the hub's leases pick one.

## What this doesn't solve

- **Email.** IMAP doesn't use MCP. Gmail and Outlook use native OAuth
  (XOAUTH2, `crate::mail_oauth`), reusing the client from step 1 with
  registered Google and Microsoft apps instead of self-registration.
- **GitHub queue.** It stays native. GitHub's MCP server only adds the agent tools.

## Phases

1. Keychain for every secret. OAuth client. Proxy route. Add by URL.
   Strict mode accepts tab connections. Two permission columns.
2. Import from Claude Code: the banner and the import dialog.
3. Catalog with capability chips. Sync recipes for Linear, Notion and Sentry.
4. Cloud: OAuth refresh tokens in the hub, the cloud eligibility check, the
   "missing in cloud" card.
5. Native OAuth for Gmail and Outlook IMAP.

## Open questions

- Should tools load in every chat or only in chosen projects? Default to
  every chat, and switch once someone has more than about 3 connections?
- Where should the CIMD document live: `tab.dev`, or the marketing site on
  Cloudflare Pages?
- Which runtimes go in the runner image for imported local-command servers:
  Node and Python? Anything else should be this computer only.
- Should agents be allowed to propose sync recipes for custom servers?
