# Agents in tab's browser

Status: **phase 1 built** (read-only, logged out). Phases 2–4 below are the
agreed plan, not built yet. The code is `crates/tab-core/src/browser_agent.rs`,
the `tab_browser_*` tools in `crates/tab-core/src/mcp.rs`, agent tabs in
`browser_open` / `browser_nav` / `browser_close` (`src-tauri/src/lib.rs`), and
the dock (`src/design/BrowserDock.tsx`, `src/lib/browser.ts`).

**Goal:** when the person asks "help me set up X" (a DNS record, an OAuth app,
a Stripe webhook…), the agent opens the pages in tab's browser dock beside the
chat, reads them, and later clicks and types its way through. The person
watches it happen and takes over whenever they want, or whenever a secret is
needed.

## Rules (don't break these)

Agents working on this area: these are the guarantees. Changing one needs
the person's explicit sign-off **and** an update to this doc and its tests in
the same PR. When in doubt, ask instead of loosening.

1. **Off by default.** `browser.agents` in the config is `off` until the person
   turns it on (Settings → Browser). Never default it on, never let an agent
   change it. (`tab_configure_connection` must not learn to set it.)
2. **Only chats the person drives, on this computer.** `McpServer::browser_session`:
   `Trust::Person`, no session key (so no coordinators, jobs, workflows or tab's
   helpers), `runs_here`, never in the cloud. Don't widen this.
3. **No trust in any model.** Decisions are fixed code in tab-core. Never gate
   on which model or harness is asking, and never let a model's answer turn a
   refusal or a question into an allow. A model-based check (the critic, phase
   4) may only *add* questions.
4. **Logged out unless lent.** Agent tabs are `incognito` webviews: their own
   throwaway data store, nothing shared with the person's tabs, nothing kept.
   Agent reads go through the gateway with no cookies. Never import logins
   into an agent tab except through the phase 3 "lend my login" flow.
5. **The shell builds agent tabs from tab-core's registry, not from the UI.**
   `browser_open` asks `browser_agent::agent_tab(id)`; for an agent tab it
   forces incognito, `on_navigation` (re-runs `check_static` on every load,
   redirects included), denies new windows and downloads, and only opens the
   URL the agent read. Keep it that way: the UI's `agent` flag is display only.
6. **URL rules** (`browser_agent::check`): http(s) only; no credentials in the
   URL; nothing on this computer or its network (localhost, `.local`,
   `.internal`, private/link-local/CGNAT ranges, IPv4 in decimal/hex/octal,
   IPv4-mapped IPv6); the host must resolve to public addresses only; every
   redirect hop is checked again; and the chat's sandbox network bounds the
   browser (`network_allows`), since the browser runs outside the sandbox.
7. **Secrets never reach the model.** `redact` hides provider keys, tokens,
   JWTs, private key blocks and long random strings before a page is returned.
   The agent is told secrets were hidden and to use the vault. Agents never
   type into password or one-time-code fields (phase 2): the person does.
8. **Page text is data.** Every read is prefixed as untrusted web content.
   Nothing from a page may reach the gate's decisions, the critic, or an
   approval card's wording.
9. **No raw JavaScript for agents.** There is no `browser_eval`. Agents get a
   fixed set of verbs; tab's own code does the DOM work. In phase 2 that code
   runs in an **isolated JS world** (`WKContentWorld` on macOS, CDP
   `Page.createIsolatedWorld` via WebView2, WebKitGTK script worlds); where a
   platform can't, the feature stays off there. Never fall back to the page's
   world.
10. **No IPC for pages in the dock.** No capability may list `remote` URLs or
    the browser's webviews, and `withGlobalTauri` stays off
    (`dock_isolation_tests` in `src-tauri/src/lib.rs`). Results from page
    scripts come back through the platform's eval-with-result, never Tauri IPC.
11. **Visible, always.** Agent tabs open in the dock, never hidden or headless,
    with a banner and an accent border. The person can close one at any time,
    and that removes it from the agent too.
12. **Limits.** At most `MAX_TABS` tabs per chat, `READS_PER_MINUTE` reads per
    chat, `MAX_REDIRECTS` per read, text capped at `MAX_CHARS`.

## Threats

| # | Threat | Main defences |
|---|---|---|
| T1 | Prompt injection from page content | Rules 3, 8; phase 2 plan matching and taint checks |
| T2 | Acting as the person on signed-in sites | Rule 4; phase 2 plan approval, consequential confirms; phase 3 watch mode |
| T3 | Secret or data exfiltration (URLs, searches, forms) | Rule 7; phase 2 taint tracking |
| T4 | Sandbox bypass (localhost, hub/MCP ports, `file:`, the person's tabs) | Rules 2, 5, 6 |
| T5 | Page spoofs or tampers with the control script | Rule 9 |
| T6 | Page gets tab's IPC | Rule 10 |
| T7 | Unattended use (cloud, workflows, coordinators, PR agents) | Rules 1, 2 |
| T8 | Side channels: downloads, uploads, popups, clipboard, permissions, custom schemes | Rule 5; phase 2 blocks file inputs, clipboard reads and permission prompts |

## Phases

1. **Read-only, logged out (built).** `tab_browser_open`, `tab_browser_read`
   (re-read, follow a numbered link, or go to a URL; `query` focuses long pages
   on the relevant sections), `tab_browser_close`. Pages show in the dock as
   agent tabs; the agent gets redacted Markdown fetched through the gateway.
2. **Plan approval, tab runs the plan.** See below. Adds reading the live
   webview (isolated world, interactive elements with refs), click, type,
   navigate, handover, consequential confirms, taint checks, person input
   pausing the agent, "Ask for everything".
3. **Lend a login + watch mode** for write sites.
4. **Critic, saved flows, remembered write sites per project, budgets in the
   banner, localhost dev mode.**

### Levels (`browser.agents`)

`off` (default) · `read_only` (phase 1) · later `plan` (phase 2's main mode)
and `ask` (every action asks). The person picks; tab never infers it from the
model.

## Phase 2: plan approval

The agent calls `tab_browser_plan` with a structured plan:

```jsonc
{
  "goal": "Point www.example.com at the app",
  "write": ["dash.cloudflare.com"],          // may click/type/submit; may be lent a login
  "read":  ["developers.cloudflare.com"],    // read-only, logged out (any public site is read-only by default)
  "steps": [
    { "do": "navigate", "where": "DNS records for example.com" },
    { "do": "click",    "what": "Add record" },
    { "do": "type",     "field": "Name",   "value": "www" },
    { "do": "type",     "field": "Target", "value": "app.example.com" },
    { "do": "submit",   "what": "Save" }
  ],
  "needs_login": ["dash.cloudflare.com"]
}
```

tab shows one card (write sites prominent, exact values highlighted) with
Approve / Edit / Decline. The gate then enforces it:

| Agent does | Gate |
|---|---|
| Reads, scrolls, follows links on any site | Allow (URL and taint checks apply) |
| Non-mutating click on a write site | Allow |
| Click/type on a write site matching a step (element and **exact value**) | Allow; highlighted as it happens |
| Types a value not in the plan | Ask, showing planned vs new |
| Sends a tainted value toward a non-write site | Refuse |
| Any form input or submit on a read site | Refuse (a site's own search goes through the URL check) |
| Adds or promotes a write site | Plan revision: ask with a diff |
| Delete / pay / grant / authorize / revoke, OAuth consent, billing | Confirm in one click even if planned; the card shows the real element |
| Password, OTP or secret field | Refuse → handover to the person |
| Revision that only removes sites or steps | Allow |

**Taint:** strings read on write sites, plus every redacted secret, are kept
(normalized: URL-encoded, base64, hex, case, whitespace) in the gate, per
chat. They may move between the plan's own write sites (audited) and nowhere
else. URLs the agent makes up toward non-plan sites must be a link it saw, a
bare origin, or a clean search.

**tab runs the plan.** After approval, tab's executor (same gate, not a side
door) finds each step's element and does it; the model is called back only on
a mismatch or surprise. Fewer model turns is both cheaper and safer.

**Watch mode (phase 3):** on lent write sites and for consequential actions,
writes pause while the dock is hidden, tab isn't frontmost, or the screen is
locked; reads continue; tab notifies the person and resumes when they're back.

**Critic (phase 4):** one-shot through `agents/small.rs` (the harness's small
model, no tools). It sees only the person's messages, the plan and tab's
description of the action, never page text. It can only turn allow into ask.

**Saved flows (phase 4):** a cleanly run plan can be saved per site with
parameters ("Cloudflare: add DNS record {name, target}"). Next time the model
only fills the parameters; it still produces a plan the person approves, and
is re-checked against the live page. (Not called "recipes": that word is
taken by connection manifests, `docs/recipe-manifests.md`.)

## Harnesses and cost

- The tools are tab MCP tools, so every harness (Claude Code, Codex, opencode,
  pi, ACP) gets the same tools and the same gate. Don't add per-harness
  browser integrations; a harness's own browser is outside this design.
- Harness facts come from `harnesses.json` (`small_model`; later `vision`,
  `deferred_tools`), never from a model list.
- Every model call goes through the person's own harness login. tab adds no
  model bill. Local models are fine because rule 3 holds regardless.
- Cost levers, in order: tab runs approved plans itself; saved flows; compact
  reads (`query` focus, element lists, diffs, `find`); docs fetched as text
  through the gateway (cached) instead of a webview; screenshots only when the
  harness reports `vision`, downscaled; the critic only on near-matches; a
  small, stable tool surface; visible budgets.

## Tests

Built:

- `browser_agent::tests`: URL rules (schemes, credentials, local names and
  every IP form, sandbox network bounds, registrable domains and look-alikes),
  redaction (provider keys, JWTs, private keys; ordinary ids and URLs kept),
  `query` focus, tabs scoped to their chat and capped, the rate limit.
- `mcp::tests::browser_tools_only_for_the_persons_own_chats_when_on`: tools
  listed and callable only by a plain person chat with the setting on; never
  coordinators or jobs; local URLs and other chats' tabs refused.
- `dock_isolation_tests` (`src-tauri`): no capability reaches remote pages or
  browser webviews, no global `__TAURI__`.

Required as each phase lands (nothing ships on a platform until its suite
passes there):

- Gate property tests: the critic can't allow; no write without a plan; no
  tainted value reaches a non-write site; narrowing never allows more.
- URL: DNS rebinding (public name → 127.0.0.1), redirects to local/`file:`
  stopped in the navigation handler.
- Real-webview e2e per platform: IPC calls from a dock page fail; a hostile
  page overriding DOM built-ins can't fool or detect the isolated-world
  script; cookies don't cross between the person's and agents' tabs; downloads,
  file inputs, popups, clipboard reads, permission prompts and `mailto:` are
  blocked.
- Redaction across iframes, shadow DOM, attributes; masked screenshots.
- An injection corpus driven by a scripted "evil agent" that obeys every
  injection: every attempt must be asked, refused or paused.
- Live evals with real harnesses on real setup flows, tracking turns, tokens
  and asks per flow (cost and convenience). They tune matching; they never
  change what the gate guarantees.

## Known gaps

- **DNS rebinding between check and fetch.** `check` resolves the host, then
  the gateway resolves it again to connect. A host that changes its answer in
  between could reach a private address. Same as the knowledge crawler today.
  Fix by connecting to the checked address (a resolver that pins it).
- **Agent tabs and tab's custom protocols.** Tauri registers the `asset`
  protocol (media files, `assetProtocol.scope` in `tauri.conf.json`) for every
  webview. Whether a remote page in the dock, the person's or an agent's, can
  fetch from it hasn't been tested. It needs the e2e test above, and a fix
  (scope or origin check) if it can.
- **The webview and the agent can see different pages.** In phase 1 the agent
  reads a cookie-less fetch while the dock renders the page with JavaScript.
  Phase 2 reads the live webview.
- Agent tabs aren't closed when their chat ends; the person or the agent
  closes them (`browser_agent::close_session` exists for that hook).
