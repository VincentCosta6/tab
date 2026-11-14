# Queue recipes as manifests — design sketch

Status: design. Nothing here is built yet, except the first step: the UI
already reads everything about a connection's queue (description, choices,
search placeholder) from its recipe, not from connector names.

## Why

tab should be able to take a new connector without a release: a manifest
someone writes for their own tool, and later one installed from a
marketplace. Today, filling the queue from an MCP connection
(`remote_mcp::sync`) is plug and play for the plumbing and hardcoded for
the rest:

| Piece | Today | Plug and play? |
|---|---|---|
| Running a recipe: budgets, audit log, dedupe, resolving what finished | `McpConnector` | Yes, generic |
| Choices and description shown in the UI | `Recipe::queue_choices`, `Recipe::description`, passed through `CatalogEntry` | Yes, generic |
| Which recipes exist | `sync::recipe()`, a `match` on `"linear" \| "sentry" \| "notion"` | No |
| What a recipe does | Rust: `impl Recipe for Sentry`, … | No |
| The catalog (name, URL, color) | `remote_mcp::catalog()`, a Rust `vec![]` | No |

`docs/connect-via-mcp.md` §3 sketched a declarative recipe and then built
a Rust trait instead. Its reasons still apply, and this design has to answer
them:

1. **More than one call per look.** Sentry looks up your organizations,
   then searches each one, and now runs two searches per organization.
2. **Paging and "changed since".** Linear pages with a cursor and asks for
   what changed since the last look.
3. **Answers that aren't JSON.** Sentry's server sometimes answers in
   markdown only, and tab parses that.

## Two tiers

1. **Manifests (data, no code).** A manifest describes calls to the
   connection's own MCP tools and how their JSON answers map to queue
   items. This covers most services: one generic engine runs every
   manifest, and Linear, Sentry and Notion become manifests shipped with
   tab.
2. **Connector plugins (code, out of process).** This is the
   JSON-RPC-over-stdio protocol in `ARCHITECTURE.md` ("Third-party
   connectors"). It's for anything a manifest can't express: parsing
   markdown, webhooks, services without an MCP server. It runs as a
   separate program, so a crash or a bug can't take tab down.

A manifest is deliberately not a programming language. When one needs an
`if`, the work belongs in tier 2.

## The manifest

One file per service: JSON on disk, shown here as TOML for reading.

```toml
manifest = 1
id = "sentry"
name = "Sentry"
url = "https://mcp.sentry.dev/mcp"          # the catalog entry
color = "#362d59"
category = "code"
description = "Agents can look up issues, stack traces and events."

[queue]
description = "Unresolved issues come into your queue, from every organization you're in: yours and unassigned ones, unless you pick otherwise. Resolved or ignored issues are marked done here."
interval = "5m"

[[queue.kinds]]
kind = "sentry.issue"
label = "Sentry issue"
icon = "alert"
tone = "danger"

# The person's pick. Each choice sets variables the steps use.
[[queue.choices]]
value = "mine_unassigned"
label = "Mine + unassigned"
vars = { searches = ["is:unresolved assigned:me", "is:unresolved is:unassigned"] }

[[queue.choices]]
value = "mine"
label = "Only mine"
vars = { searches = ["is:unresolved assigned:me"] }

[[queue.choices]]
value = "all"
label = "All unresolved"
vars = { searches = ["is:unresolved"] }

# Steps run in order. A step's answer can feed the next one with `for_each`.
[[queue.steps]]
name = "orgs"
call = ["find_organizations", "list_organizations"]   # the first one the server has
args = {}
list = "organizations"                 # where the list is in the answer
limit = 5

[[queue.steps]]
call = ["search_issues", "find_issues", "list_issues"]
for_each = { org = "orgs", query = "$searches" }       # every org × every search
args = { organizationSlug = "{org.slug}", regionUrl = "{org.regionUrl?}", query = "{query}", sort = "date", limit = 100 }
list = "issues"
emit = "issue"                         # items come from this step

[queue.issue]
external_id = "{org.slug}/{shortId}"
title = "{shortId} {title}"
url = "{url? ?? permalink? ?? https://{org.slug}.sentry.io/issues/{shortId}/}"
updated_at = "lastSeen"
resolved_when = { status = ["resolved", "ignored", "archived", "muted"] }
fields = [
  { label = "Where", value = "{culprit?}" },
  { label = "Events", value = "{occurrences? ?? count?}" },
  { label = "Users", value = "{usersImpacted? ?? userCount?}" },
]
metadata = { organization = "{org.slug}", short_id = "{shortId}", region_url = "{org.regionUrl?}" }

# Asked about items that dropped out of the list.
[queue.check]
call = ["get_issue_details"]
args = { organizationSlug = "{metadata.organization}", issueId = "{metadata.short_id}", regionUrl = "{metadata.region_url?}" }
resolved_when = { status = ["resolved", "ignored", "archived", "muted"] }
```

How it handles the three gaps from earlier:

- **More than one call:** steps, plus `for_each` over an earlier step's
  list (and over a list from the person's choice). The engine caps the
  total calls per look, on top of the gateway's budgets.
- **Paging and since:** a step can have
  `page = { cursor = "nextCursor", more = "hasNextPage", arg = "cursor", max = 3 }`
  and `since = { arg = "updatedAt", overlap = "5m" }`. A manifest with
  `since` is incremental, like Linear today.
- **Non-JSON answers:** the engine reads `structuredContent`, then JSON in
  the text, which is what `payload()` does today. Markdown isn't parsed: a
  service that only answers in markdown needs a tier 2 plugin. Sentry's
  server returns JSON now, so its markdown fallback can go once the
  manifest ships.

**Templates** are the only expression language:
- `{path}` reads from the current item or from a variable;
- `?` makes a value optional (the field is dropped when it's missing);
- `??` falls back to the next option.

There is no arithmetic, no conditions and no loops beyond `for_each`.

## The engine

`ManifestRecipe` implements the existing `Recipe` trait. `McpConnector`
doesn't change: it still runs recipes, dedupes on `external_id`, checks
items that dropped out, and resolves what finished. `sync::recipe()`
becomes a lookup in a registry loaded from:

1. the built-ins, embedded in the binary with `include_str!`;
2. a folder of the person's own (`<data>/recipes/*.json`), for writing and
   testing one locally;
3. later, the marketplace.

`remote_mcp::catalog()` is built from the same manifests, so adding a
service is one file.

## Safety

A manifest is data, but it does act on the person's behalf:

- **It only calls tools on its own connection,** through `Gateway::run`
  with the same budgets and audit log as today.
- **It only calls read-only tools.** The engine refuses a step whose tool
  doesn't say it's read-only (MCP's `readOnlyHint`), and refuses tools
  the person turned Off. Filling the queue never changes anything
  upstream.
- **Strings from a manifest are shown as text.** That covers
  descriptions, labels and fields: no HTML and no links outside `url`.
- **The engine validates a manifest before running it:** a known schema
  version, steps that point at real step names, and at most 10 steps and
  50 calls per look. A manifest that fails validation shows the error on
  the connection's page, the same way a failed look does today.
- **Marketplace manifests are signed and reviewed.** Installing one shows
  what it reads ("Calls `search_issues` and `get_issue_details` on your
  Sentry connection") before it runs.

## Moving there

1. **Done (tab#159):** the UI reads the queue description and choices
   from the recipe. No connector names are left in the UI.
2. **Build the engine** and a manifest JSON schema, with tests that run
   the Linear, Sentry and Notion manifests against the same fake servers
   `sync.rs` tests use today. The Rust recipes stay until the manifests
   pass every one of those tests.
3. **Swap the built-ins to manifests** and delete the Rust recipes and the
   `match` in `sync::recipe()`. Move `catalog()` into the manifests.
4. **Load the person's own manifests** from the data folder, with a
   "Reload" button and errors on the connection page.
5. **Build tier 2,** the stdio connector protocol, for what manifests
   can't do.
6. **Marketplace:** signing, review and installing, as its own design.

## Open questions

- **Manifests for built-in connectors:** should GitHub and email (the
  connectors that aren't MCP) get manifests too, or stay Rust? They do
  much more than list and check, so probably Rust, or tier 2.
- **Choices beyond one pick:** some services will want a free-text search
  and a choice together. Today a connection has one `queue_query` field.
  A map of variables (`queue_vars`) would cover both, and needs a config
  migration.
- **Cloud:** manifests are data, so the cloud runner could run them as
  well. That is phase 4 of `connect-via-mcp.md`.
