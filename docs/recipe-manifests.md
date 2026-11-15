# Connection manifests

Status: built for MCP connections (`remote_mcp::manifest`). Not built yet:
a Reload button, load errors on the Connections page, connector plugins,
and the marketplace (see "What's next").

## Why

tab should be able to take a new connector without a release: a manifest
someone writes for their own tool, and later one installed from a
marketplace. Each service tab knows the MCP server of is now one JSON file:
its catalog entry and, optionally, how to fill the queue from it. One
engine (`ManifestRecipe`) runs every manifest. No service is named in
Rust or in the UI.

- Built-ins: `crates/tab-core/src/remote_mcp/manifests/*.json`, compiled
  in (Linear, Notion, Jira & Confluence, Sentry, Vercel, Stripe).
- The person's own: `<app data>/recipes/*.json`, read at startup. One with
  a built-in's id takes its place. A file that doesn't load is skipped and
  logged with the reason.

`docs/connect-via-mcp.md` §3 gave three reasons recipes were Rust. Here's
how manifests handle each:

1. **More than one call per look:** steps, where a step can run once per
   item of an earlier step's list (`for_each`). Sentry lists your
   organizations, then searches each one.
2. **Paging and "changed since":** `page` follows a cursor, and
   `incremental` gives the steps `$since`. Linear uses both.
3. **Answers that aren't JSON:** manifests read `structuredContent`, or
   JSON in the text. They don't parse markdown. Sentry's server answers in
   JSON now, so its markdown parsing is gone. A service that only answers
   in markdown needs a connector plugin.

## The format

`manifests/sentry.json` is the fullest example. The fields:

```jsonc
{
  "manifest": 1,                       // the format version
  "id": "sentry", "name": "Sentry", "url": "https://mcp.sentry.dev/mcp",
  "description": "Agents can …",        // for the catalog
  "category": "code", "color": "#362d59",
  "queue": {                           // leave out: agent tools only
    "description": "What comes in, and when it's marked done.",
    "interval": "5m",                  // at least 1m
    "kinds": [{ "kind": "sentry.issue", "label": "Sentry issue", "icon": "alert", "tone": "danger" }],
    "choices": [                       // what the person picks; the first is the default
      { "value": "mine", "label": "Only mine", "vars": { "searches": ["is:unresolved assigned:me"] } }
    ],
    "search": { "hint": "tasks assigned to me" },   // or a typed search, as $query
    "incremental": { "overlap": "5m" },             // later looks list only changes, from $since
    "steps": [
      { "name": "orgs", "call": ["find_organizations"], "list": ["organizations"], "limit": 5,
        "if_empty": "Sentry didn't say which organizations you're in" },
      { "call": ["search_issues"],     // the first of these the server has
        "if_missing": "Sentry didn't offer a tool to search issues",
        "for_each": [{ "var": "org", "in": "orgs" }, { "var": "search", "in": "$searches" }],
        "args": { "organizationSlug": "{$org.slug}", "query": "{$search}", "limit": 100 },
        "list": ["issues"],
        "page": { "cursor": "{nextCursor}", "more": "{hasMore}", "arg": "cursor", "max": 3 } }
    ],
    "item": {                          // each thing in the last step's list
      "id": "{$org.slug}/{shortId}",
      "title": "{shortId} {title ?? shortId}",
      "url": ["{url ?? permalink}", "https://{$org.slug}.sentry.io/issues/{shortId}/"],
      "kind": { "from": "{type}", "map": { "database": "notion.database" }, "default": "notion.page" },
      "body": "…", "actor": "…", "context": "…", "time": "{lastSeen}",
      "resolved": [{ "value": "{status}", "in": ["resolved", "ignored"] }, { "value": "{completedAt}", "set": true }],
      "fields_as": "fields",           // or "badges"
      "fields": [{ "label": "Priority", "value": "{priority|name}", "skip": ["No priority"], "tones": { "Urgent": "danger" } }],
      "text": { "value": "{description|clip:1500}", "markdown": true },
      "link": "Open in Sentry",
      "entities": [{ "kind": "project", "key": "sentry:{$org.slug}/{projectSlug}", "label": "{projectSlug}" }],
      "metadata": { "short_id": "{shortId}" }
    },
    "check": {                         // asked about items that dropped out of the list
      "call": ["get_issue_details"],
      "args": { "issueId": "{$metadata.short_id}" },
      "unwrap": ["issue"],
      "known": "{status}"              // otherwise tab can't tell
    }
  }
}
```

**Templates** are the only expression language:
- `{path}` reads the current thing (`{status.type}`) or a variable
  (`{$org.slug}`);
- `??` falls back to the next option, and `'text'` is a literal;
- `|` applies a filter: `name`, `lower`, `short_id`, `nodash` or `clip:N`.

A template with a missing value (absent, null or empty) is missing as a
whole. Where a manifest gives a list of templates, the first one that
isn't missing wins. An argument whose template is missing is left out. A
lone `{…}` keeps its JSON type.

There are no conditions, and no loops beyond `for_each`. When a manifest
needs an `if`, the work belongs in a connector plugin.

**Resolved items:** an incremental look keeps items that finished upstream,
so they're marked done. A first look, or a look that lists everything open,
drops them.

## Safety

- **Tools:** a manifest only calls tools on its own connection, through
  `Gateway::run`, with the same budgets and audit log as before.
- **Read-only:** manifests that don't ship with tab may only call tools the
  server marks read-only (MCP's `readOnlyHint`). Built-ins are reviewed
  with tab's code, and some servers don't annotate their tools.
- **Limits:** at most 10 steps and 50 calls per look, and a look at most
  once a minute.
- **Checked on load:** a manifest is validated when it loads:
  - the version;
  - its id;
  - its interval;
  - that every template parses;
  - that `for_each` points at earlier steps;
  - that item kinds are among the queue's kinds.

  Unknown fields are refused, so typos don't pass silently.
- **Strings are shown as text:** no HTML, and no links other than `url`.

## What's next

1. **Reload and errors in the app:** a Reload button on Connections that
   re-reads `recipes/` and rebuilds the sync connectors, and that shows the
   files that didn't load and why. `manifest::load_dir` already returns
   those errors.
2. **Connector plugins:** the stdio protocol in `ARCHITECTURE.md`, for
   what manifests can't do (markdown, webhooks, services without MCP).
3. **Marketplace:** signing, review and installing, as its own design.
   Installing a manifest should show what it reads ("Calls `search_issues`
   on your Sentry connection") before it runs.
4. **Choices beyond one pick:** a connection keeps one `queue_query`, so a
   manifest offers either choices or a typed search, not both. A map of
   variables (`queue_vars`) would cover both, and needs a config migration.
5. **Cloud:** manifests are data, so the cloud runner could run them too.
   That's phase 4 of `connect-via-mcp.md`.
