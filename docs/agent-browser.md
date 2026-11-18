# Agents in tab's browser

Status: **built, all phases**. Live pages (reading the real tab, clicking,
typing, screenshots, lending a login) need an isolated script world, which
tab has on **macOS** today. On Windows and Linux agents get the read-only,
logged-out text reads only, as rule 9 requires, until the WebView2 (CDP
`Page.createIsolatedWorld`) and WebKitGTK routes are built.

Code:

| Where | What |
|---|---|
| `crates/tab-core/src/browser_agent/mod.rs` | URL rules, dev-server ports, redaction, the agent-tab registry, limits, text reads |
| `browser_agent/plan.rs` | The plan: validation, label matching, narrowing, diffs |
| `browser_agent/gate.rs` | **The decision for every action**, with property tests |
| `browser_agent/taint.rs` | What was read on write sites, so it can't leave |
| `browser_agent/state.rs` | Per chat: approved plan, taint, lent logins, pauses, action count; watch mode |
| `browser_agent/page.rs`, `page.js` | tab's page script (run in the isolated world) and its ops |
| `browser_agent/critic.rs` | The second check (escalate only) |
| `browser_agent/flows.rs` | Saved flows |
| `browser_agent/tools.rs` | The `tab_browser_*` MCP tools, cards, tab's plan executor |
| `src-tauri/src/agent_browser.rs` | macOS: the isolated world, screenshots, lending cookies into an agent tab |
| `src-tauri/src/lib.rs` | Agent tabs in `browser_open`/`browser_nav`/`browser_close`; `browser_agent_watch`/`_resume`/`_stop`; `dock_isolation_tests` |
| `src/design/BrowserDock.tsx`, `src/lib/browser.ts` | Agent tabs, the banner (progress, Hand back, Stop), watch reporting |
| `src/modules/work/Chat.tsx` | The plan card and action cards |
| `src/modules/settings/pages/Browser.tsx` | Level, second check, dev-server ports |

**Goal:** when the person asks "help me set up X" (a DNS record, an OAuth app,
a Stripe webhook…), the agent researches, proposes a plan the person approves
once, and tab carries it out in a tab beside the chat while the person
watches, stepping in for anything risky or secret.

## Rules (don't break these)

Agents working on this area: these are the guarantees. Changing one needs
the person's explicit sign-off **and** an update to this doc and its tests in
the same PR. When in doubt, ask instead of loosening.

1. **Off by default.** `browser.agents` is `off` until the person picks a level
   in Settings → Browser. Never default it on; no agent tool may change it.
2. **Only chats the person drives, on this computer.** `McpServer::browser_session`:
   `Trust::Person`, no session key (no coordinators, jobs, workflows or
   helpers), `runs_here`, never in the cloud. Don't widen this.
3. **No trust in any model.** Every decision is `gate::decide`, plain code.
   Never gate on which model or harness asks. A model (the critic) may only
   turn "allow" into "ask"; an error or timeout from it also means "ask".
4. **Logged out unless lent.** Agent tabs are `incognito` webviews: their own
   throwaway store, nothing shared with the person's tabs. Text reads go
   through the gateway with no cookies. A login reaches an agent tab only when
   the person picks "Approve and lend my login" on a plan, only for that plan's
   write sites, copied into **that tab's** store (`agent_browser::lend`), and
   the tab then stays on the write sites (`navigation_allowed`) except while
   the person does a handover.
5. **The shell builds agent tabs from tab-core's registry, not from the UI.**
   For an agent tab `browser_open` forces incognito, runs `navigation_allowed`
   on every load (redirects included), denies new windows and downloads, and
   only opens the URL the agent read. The UI's `agent` flag is display only.
6. **URL rules** (`check`, `check_static`): http(s) only; no credentials in the
   URL; nothing on this computer or its network (every IP form, `.local`,
   `.internal`, dotless names) except dev-server ports the person listed, never
   tab's own MCP port; public DNS only; every redirect re-checked; and the chat's
   sandbox network bounds the browser.
7. **Plans bind values.** Under plan approval, a write (type/select) goes
   through only if its field matches a step **and its value equals the step's
   exactly**; a submit or mutating click only if it matches a click/submit step.
   Anything else asks. No plan: reading and harmless clicks only. Narrowing a
   plan needs no approval; anything new shows a diff and asks.
8. **Consequential actions always confirm**, planned or not (delete, pay,
   grant, authorize, revoke, send, invite, publish, deploy…, and submits on
   billing/checkout/oauth/consent paths). The card shows a screenshot with the
   real element outlined. Page text on a card is quoted and labelled as the
   page's words, never used as tab's own wording.
9. **No raw JavaScript for agents.** Agents pick from `page::Op`; tab's
   `page.js` runs them in an **isolated world** (`WKContentWorld` "tab-agent"):
   the page can't see, call or fake it, and element refs live in that world,
   never in the DOM. Ops go in as JSON data, never interpolated as code. Where
   a platform has no isolated world, live pages stay off there.
10. **No IPC for pages in the dock.** No capability lists `remote` URLs or
    browser webviews, `withGlobalTauri` stays off (`dock_isolation_tests`).
    Page results come back through WebKit's eval-with-result, never Tauri IPC.
11. **Secrets never reach the model, and agents never type them.** `redact`
    runs on all page text and labels; `page.js` blanks secret-field values;
    secret fields (password, one-time code, card number/CVC, names like
    token/api key) are refused with a handover, in Rust and again in the page
    script. Screenshots cover secret fields.
12. **What's read on write sites stays there** (taint). Strings read on write
    sites and every hidden secret may go to the plan's write sites only; any
    URL, search or typed value bound elsewhere that carries one (as is,
    URL-encoded twice, base64, hex, any case, separators) is refused. Values
    the person approved in a plan are theirs to send.
13. **The person's input wins.** A trusted click or key in an agent tab after
    the agent's last action pauses it there until the person presses Hand
    back. A handover pauses it until then too. Stop ends the chat's browsing:
    plan, taint, lent logins, tabs.
14. **Watch mode.** With a lent login, writes, and anything consequential,
    wait until the person is looking at that tab (the dock's active tab, tab
    in front, page visible).
15. **Visible, always.** Agent tabs open in the dock with a banner and accent
    border; never hidden or headless.
16. **Limits.** `MAX_TABS` per chat, `READS_PER_MINUTE`, `MAX_ACTIONS` per chat
    (shown in the banner), `MAX_REDIRECTS`, `MAX_CHARS`, plan size limits.
17. **tab's executor is not a side door.** `tab_browser_run` takes each step
    through the same `act` path as the agent's calls (gate, cards, watch
    mode), and only acts on one clear element; ambiguity stops it.

## Threats

| # | Threat | Main defences (rules) |
|---|---|---|
| T1 | Prompt injection from page content | 3, 7, 8, 12, critic sees no page text |
| T2 | Acting as the person on signed-in sites | 4, 7, 8, 13, 14 |
| T3 | Secret or data exfiltration | 11, 12 |
| T4 | Sandbox bypass (localhost, tab's ports, `file:`, the person's tabs) | 2, 5, 6 |
| T5 | Page spoofs or tampers with the control script | 9 |
| T6 | Page gets tab's IPC | 10 |
| T7 | Unattended use (cloud, workflows, coordinators, PR agents) | 1, 2 |
| T8 | Side channels: downloads, popups, custom schemes | 5 |

## Levels (`browser.agents`)

| Level | Agents get |
|---|---|
| `off` (default) | Nothing |
| `read_only` | `tab_browser_open/read/close`: read pages, logged out; no acting |
| `plan` | Plus `plan`, `run`, `click`, `type`, `select`, `handover`, `flows`, `screenshot` (if the harness takes images): the main mode |
| `ask` | The same tools; every write and every action off the plan asks |

`browser.agent_critic_off` turns the second check off; `browser.agent_dev_ports`
lists dev-server ports agents may open.

## How it works

1. **Research.** The agent opens and reads pages. No plan is needed to read.
   On macOS a read is the live tab: text plus interactive elements by ref
   (`e12 button "Save" (submits)`), redacted. Elsewhere it's a cookie-less fetch.
2. **Plan.** `tab_browser_plan` with `goal`, `write`, `read`, `steps` (each
   with exact values) and `needs_login`, or a saved `flow` with `params`. The
   card shows write sites, each step with its value, the critic's note if it
   has one, and Approve / Approve and lend my login / Decline.
3. **Run.** `tab_browser_run` carries out the remaining steps itself,
   stopping at the first that doesn't clearly match the page; or the agent acts
   step by step. Matching steps go through; off-plan actions get a card;
   consequential ones get a confirm card; secret fields get a handover.
4. **Watch.** The banner shows the goal, steps done, actions used, a lent
   login, and Hand back / Stop. If the person isn't watching when a watched
   action comes up, the dock brings the tab forward and the agent waits.
5. **Save.** `tab_browser_flows` with `save` keeps the approved plan as a
   flow, with the values that change between uses as parameters. Next time
   the agent plans with the flow and only fills in the parameters; the person
   still approves the plan. (Not "recipes": that word means connection
   manifests, `docs/recipe-manifests.md`.)

## Harnesses and cost

- The tools are tab MCP tools, so every harness (Claude Code, Codex, opencode,
  pi, ACP) gets the same tools and the same gate. No per-harness browser
  integrations; a harness's own browser is outside this design.
- Harness facts come from `harnesses.json`: `small_model` (the critic), and
  `capabilities.vision` (unset means yes; `false` hides the screenshot tool).
  Never a model list.
- Every model call goes through the person's own harness login; tab adds no
  model bill. Local models are fine because rule 3 holds regardless.
- Cost: tab runs approved plans itself (one model turn instead of one per
  click); saved flows skip research; reads are compact (`query` focus, element
  lists); text reads go through the gateway's cache; screenshots only on
  request; the critic runs once per plan and on near-matches only, with a
  short prompt and no page text.

## Tests

- `browser_agent::gate::tests`: every verdict path, plus property tests: no
  write without a plan, secrets never typed, taint never reaches another site,
  unplanned values ask, narrowing never widens.
- `browser_agent::{plan,taint,flows,state,page,tools}::tests`, and `browser_agent::tests`
  (URL rules, dev ports, redaction, focus, tabs, limits).
- `mcp::tests::an_approved_plan_runs_and_nothing_else_does`: the whole tool
  path against a fake page (no network): no acting without a plan, the plan
  card, tab running exactly the planned steps with no cards, an off-plan value
  asking and being declined, a secret field handed over, read data refused on
  its way out.
- `mcp::tests::browser_tools_only_for_the_persons_own_chats_when_on`.
- `dock_isolation_tests` (`src-tauri`).
- `page.js` was exercised in a real browser engine (Playwright Chromium)
  during development: snapshot (incl. shadow DOM), secret detection and
  blanking, type/select/click, trusted-input detection, masks, stale refs. It
  isn't in CI yet.

Still to add (needs a real WKWebView, so a macOS e2e harness): IPC calls from
a dock page fail; a hostile page overriding DOM built-ins can't fool or detect
the isolated world; cookies don't cross between the person's and agents'
tabs; downloads and popups blocked; screenshots masked; the injection corpus
with a scripted agent that obeys every injection.

## Known gaps

- **Live pages are macOS only.** Windows (WebView2: CDP isolated world) and
  Linux (WebKitGTK script worlds) need their own `agent_browser` code.
- **DNS rebinding between check and fetch** for text reads (same as the
  knowledge crawler); fix by pinning the checked address.
- **Tauri's `asset` protocol** is registered for every webview; whether a dock
  page can fetch from it is untested. Needs the e2e test, and a fix if so.
- **Masks and outlines are DOM nodes**: a hostile page could remove a mask
  before a screenshot. Text reads (redacted in Rust) are the safe path;
  screenshots are a convenience.
- **Synthetic clicks** (`isTrusted: false`) are ignored by a few sites; the
  agent then hands the step to the person.
- **Watch mode** knows the dock, window focus and page visibility, not the OS
  screen lock directly, and has no OS notification yet: the dock brings the
  waiting tab forward.
- **Flows** are kept in `browser-flows.json` in tab's data folder, unencrypted.
  Values not turned into parameters stay in them.
- Agent tabs aren't closed when their chat ends (Stop does it; so does the person
  closing them).
