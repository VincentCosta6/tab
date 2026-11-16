# Connection manifests

Status: built: manifests (`remote_mcp::manifest`), connector plugins
(`remote_mcp::plugin`) and the marketplace (`remote_mcp::marketplace`,
`marketplace/`). Before the marketplace works, someone has to make the
signing key (see "The marketplace"). Still to do is under "What's next".

## Why

tab should be able to take a new connector without a release: a manifest
someone writes for their own tool, and later one installed from a
marketplace. Each service tab knows the MCP server of is now one JSON file:
its catalog entry and, optionally, how to fill the queue from it. One
engine (`ManifestRecipe`) runs every manifest. No service is named in
Rust or in the UI.

- Built-ins: `crates/tab-core/src/remote_mcp/manifests/*.json`, compiled
  in (Linear, Notion, Jira & Confluence, Sentry, Vercel, Stripe).
- The person's own: `<app data>/recipes/*.json`.
- Installed from the marketplace: `<app data>/recipes/marketplace/*.json`.

tab reads both folders at startup and again when the person presses
Reload under Connections → Installed → Manifests. That section shows every
manifest that isn't built in, where it came from, and each file that didn't
load with the reason. Reload also restarts the queue sync, so changes apply
straight away. When two manifests have the same id, the person's own beats
the marketplace's, which beats the built-in.

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
    "kinds": [{ "kind": "sentry.issue", "label": "Sentry issue", "icon": "alert", "tone": "danger",
                // where items of this kind live upstream; tab routes them to a project by it
                "container": { "kind": "sentry.project", "key": "sentry:{$org.slug}/{projectSlug}", "label": "{projectSlug}" } }],
    "choices": [                       // what the person picks (queue_choice); the first is the default
      { "value": "mine", "label": "Only mine", "vars": { "searches": ["is:unresolved assigned:me"] } }
    ],
    "search": { "hint": "tasks assigned to me" },   // and/or a typed search (queue_query), as $query
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
- `|` applies a filter: `name`, `lower`, `short_id`, `nodash`, `prefix`
  (`TAB-123` → `TAB`) or `clip:N`.

A template with a missing value (absent, null or empty) is missing as a
whole. Where a manifest gives a list of templates, the first one that
isn't missing wins. An argument whose template is missing is left out. A
lone `{…}` keeps its JSON type.

There are no conditions, and no loops beyond `for_each`. When a manifest
needs an `if`, the work belongs in a connector plugin.

**Containers:** a kind's `container` names where its items live upstream
(a Sentry project, a Linear team, a Notion parent page). Its `kind` is
`<service>.<what>`. Ingest writes it to the graph as an entity with
`rel: "container"`, and tab routes the item to the project that container
is anchored to (the project's Routing section). The GitHub connector does
the same with `owner/repo`, and a plugin can return an entity with
`"rel": "container"` itself. A container with a missing value is left
out.

**Resolved items:** an incremental look keeps items that finished upstream,
so they're marked done. A first look, or a look that lists everything open,
drops them.

## Connector plugins

Some work needs code: answers in markdown, logic with conditions. For that,
a manifest in the person's own folder can name a program instead of
`steps`, `item` and `check`:

```json
"queue": {
  "description": "…", "kinds": [ … ], "choices": [ … ],
  "plugin": { "command": "node", "args": ["acme.js"], "network": [], "timeout": "60s" }
}
```

How tab runs it:
- **When:** tab starts the program for each look, and for each item check,
  from the manifest's folder. It stops the whole process tree when it's
  done or when the timeout passes.
- **Sandbox:** the program runs isolated. It can read files that aren't
  hidden, write only its own folder and temp, and reach only the hosts in
  `network` (none by default). Its environment holds none of tab's tokens.

They talk JSON-RPC 2.0, one message per line on stdin and stdout:

| Who asks | Method | Params | Answer |
|---|---|---|---|
| tab | `poll` | `{choice, query, since}` | `{items: [{id, title, kind?, body?, url?, actor?, context?, time?, resolved?, fields?, badges?, text?, markdown?, link?, entities?, metadata?}]}` |
| tab | `check` | `{external_id, metadata}` | `{resolved: true \| false \| null}` |
| plugin | `tools/list` | | `{tools: [{name, read_only}]}` |
| plugin | `tools/call` | `{name, arguments}` | the tool's result |
| plugin | `log` (notification) | `{message}` | |

**Tool calls go through tab.** tab calls the connection's tools as the
person, with the same budgets, audit log and read-only rule as a manifest's
steps, and with at most 50 calls per request. A plugin never sees the
connection's sign-in.

**Limits:** a look returns at most 500 items. Plugins only load from the
person's own folder, never from the marketplace.

## The marketplace

**Where it shows:** signed manifests appear under Connections →
Marketplace → Community manifests.

**The index:** `marketplace/index.json` in tab's repo (or the build
variable `TAB_MARKETPLACE_INDEX`). It lists each manifest's file, version,
SHA-256 and an Ed25519 signature over the file's bytes.

**Installing:**
1. tab downloads the file.
2. It checks the hash and the signature against the key the build was made
   with (`TAB_MARKETPLACE_PUBKEY`).
3. It validates the manifest, and refuses plugins and ids that don't match
   the listing.
4. It shows which tools the manifest will call, and whether it replaces a
   built-in.
5. Once the person confirms, it saves the file into the marketplace folder
   and reloads.

**Publishing:** `marketplace/README.md` covers publishing and making the
key. Until a key exists and release builds get its public half, the app
says that this build has no marketplace.

## Safety

- **Tools:** a manifest (or plugin) only calls tools on its own connection, through
  `Gateway::run`, with the same budgets and audit log as before.
- **Read-only:** manifests and plugins that don't ship with tab may only
  call tools the server marks read-only (MCP's `readOnlyHint`). Built-ins are reviewed
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

1. **The marketplace key:** make it (`manifest_sign keygen`), keep the
   private half safe, and give release builds `TAB_MARKETPLACE_PUBKEY`.
   Then publish the first manifests.
2. **Cloud:** manifests are data, so the cloud runner could run them too.
   That's phase 4 of `connect-via-mcp.md`. Plugins would stay local.
3. **Connectors beyond MCP:** GitHub and email are still Rust connectors.
   A plugin that is itself a local MCP server (a stdio command connection)
   covers new services without an MCP server of their own.
