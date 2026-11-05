// Browser-only stand-in for the Rust backend. Mirrors the demo connector's
// sample data closely enough to design against.

import type {
  GhLoginCode, GhStatus, LspServer,
  AuditEntry, Block, Chat, ConnectorInfo, ContextEntry, EngineEvent, ItemState, Pressure, QueueItem, RateLimitState,
  Entity, GraphView, Project, ProjectNote, PullReview, Session, SessionMessage, TabConfig, Urgency, UsageRow, WorkspaceProgress,
} from "./types";

const ago = (min: number) => new Date(Date.now() - min * 60_000).toISOString();

type Seed = [kind: string, title: string, body: string, actor: string, context: string, url: string, min: number, score: number, base: number, reason: string];

const seeds: Seed[] = [
  ["slack.mention", "@you in #incident-checkout: are you around? latency on /checkout is 4x", "p99 on /checkout jumped from 400ms to 1.7s after the 14:10 deploy. Rolling back now but need someone who knows the cart cache to confirm.", "Priya (on-call)", "#incident-checkout", "https://slack.com", 4, 96, 83, "On-call is mid-incident and asking for you by name; the cart cache is yours."],
  ["agent.needs_input", "Agent session blocked: which migration strategy for the orders table?", "Working on \"split orders.status into enum\". Two options: (a) backfill in place with a lock, ~3 min downtime, (b) dual-write for one release. Waiting on your choice before continuing.", "Claude · orders-refactor", "acme/orders", "", 12, 87, 87, "An agent is blocked on you"],
  ["github.review_requested", "Review requested: Add idempotency keys to payment webhooks (#482)", "Dana requested your review. 14 files changed, +612 −203. \"Need this merged before Thursday's release cut, happy to pair if it's easier.\"", "dana-k", "acme/payments", "https://github.com/acme/payments/pull/482", 35, 82, 75, "Dana needs this merged before Thursday's release cut."],
  ["agent.needs_review", "Agent finished: Add pagination to /v2/invoices — ready for review", "Opened PR #491. All tests pass. Summary: cursor-based pagination, default page size 50, backwards compatible.", "Claude · invoices-pagination", "acme/payments", "https://github.com/acme/payments/pull/491", 22, 65, 65, "An agent finished work for you to review"],
  ["github.ci_failed", "CI failed on your PR: Migrate auth middleware to tower (#1290)", "integration-tests (ubuntu-latest) failed: test_refresh_token_rotation timed out after 30s.", "github-actions", "acme/api", "https://github.com/acme/api/pull/1290/checks", 50, 65, 65, "CI failed on your change"],
  ["slack.dm", "DM from Sam: quick q on the feature flag for the new onboarding flow", "Is it safe to flip onboarding_v2 to 50% today, or are we still waiting on the analytics fix?", "Sam (PM)", "Direct message", "", 18, 60, 60, "Direct message"],
  ["meeting.action_item", "From \"Q4 planning sync\": you own the rate-limit design doc", "Zoom AI summary: \"Vincent to draft the rate-limiting design doc and circulate by Friday. Marco to provide current traffic numbers.\"", "Zoom AI Companion", "Q4 planning sync", "", 95, 55, 55, "Action item assigned to you"],
  ["email.direct", "Re: SOC 2 evidence for access reviews", "Hi, could you send the screenshots of the quarterly access review for the prod database by end of next week? Thanks, Alex (Compliance)", "alex@acme.com", "Inbox", "", 300, 53, 53, "Email addressed to you"],
  ["github.comment", "3 new comments on Refactor queue worker retries (#477)", "Marco: \"Should the backoff cap be configurable? 5 min feels long for the email worker.\"", "marco-r", "acme/payments", "https://github.com/acme/payments/pull/477", 140, 45, 45, "New activity on a thread you're in"],
  ["slack.channel", "#eng-announcements: Reminder, office closed Monday", "Friendly reminder that the office is closed Monday for the holiday. On-call rotation is unchanged.", "People Team", "#eng-announcements", "", 400, 25, 25, "Slack activity"],
  ["email.newsletter", "This week in Rust #612", "Weekly digest. Unsubscribe anytime.", "TWiR", "Inbox", "", 600, 0, 0, "Email"],
];

const urgency = (s: number): Urgency => (s >= 80 ? "now" : s >= 60 ? "today" : s >= 35 ? "this_week" : "whenever");

function entities(kind: string, actor: string, context: string, url: string) {
  const app = kind.split(".")[0];
  const e = [{ kind: "person", key: `${app}:${actor}`, label: actor, rel: "from" }];
  if (context.includes("/")) e.push({ kind: "repo", key: `github:${context}`, label: context, rel: "in" });
  else if (context.startsWith("#")) e.push({ kind: "channel", key: `slack:${context}`, label: context, rel: "in" });
  const pr = url.split("/pull/")[1]?.split("/")[0];
  if (pr) e.push({ kind: "pull_request", key: `github:${context}#${pr}`, label: `${context}#${pr}`, rel: "about" });
  return e;
}

let items: QueueItem[] = seeds.map(([kind, title, body, actor, context, url, min, score, base, reason], i) => {
  const blocks: Block[] = [{ type: "quote", text: body, author: actor }];
  if (kind === "agent.needs_input")
    blocks.push({ type: "choices", prompt: "Which migration strategy?", options: ["Backfill in place (~3 min downtime)", "Dual-write for one release"] });
  return {
    id: `mock-${i}`, source: "demo", external_id: `seed-${i}`, kind, title, body, url: url || null, actor, context,
    occurred_at: ago(min), blocks, entities: entities(kind, actor, context, url), metadata: { demo: true },
    state: "open", snoozed_until: null, created_at: ago(min), updated_at: ago(min),
    ranking: { score, base_score: base, ai_adjustment: score === base ? null : score - base, urgency: urgency(score), reason,
      ranked_by: score === base ? "rules" : "rules+claude-haiku-5-5" },
  };
});

const base = { rate_limits: [], category: "built_in", features: [] as string[], builtin: false };
const connectors: ConnectorInfo[] = [
  { ...base, id: "tab", name: "tab", icon: "tab", builtin: true, description: "tab's own tasks and schedules. Anything you or an agent asks tab to do becomes a task here, with a priority, an optional due time, and its own agent session.", available: true, enabled: true, status: { state: "running", last_sync: ago(0) },
    features: ["Tasks with priority (low, normal, high, urgent) and due times", "Scheduled tasks wake up in the queue when they are due", "Every task can own an agent session", "Agents create and update tasks through tab's MCP server"],
    kinds: [
      { kind: "tab.task", label: "Task", icon: "check-square", tone: "accent" },
      { kind: "tab.scheduled", label: "Scheduled", icon: "clock", tone: "warning" },
      { kind: "agent.needs_input", label: "Agent needs input", icon: "bot", tone: "warning" },
      { kind: "agent.needs_review", label: "Agent ready for review", icon: "bot", tone: "accent" },
    ], settings: [] },
  { ...base, id: "github", name: "GitHub", icon: "github", category: "code", description: "Review requests, CI failures, mentions and comments from your GitHub notifications.", available: true, enabled: false, status: { state: "disabled" },
    features: ["Review pull requests in a built-in VS Code editor with side-by-side diffs", "Comment on lines without leaving tab", "Marking an item done marks the notification read"],
    rate_limits: [
      { provider: "github", resource: "core", requests: 5000, per_seconds: 3600, burst: 40, note: "REST: 5,000 requests per hour per token" },
      { provider: "github", resource: "search", requests: 30, per_seconds: 60, burst: 10, note: "Search: 30 requests per minute" },
    ],
    kinds: [
      { kind: "github.review_requested", label: "Review requested", icon: "git-pull-request", tone: "accent" },
      { kind: "github.ci_failed", label: "CI failed", icon: "x-circle", tone: "danger" },
      { kind: "github.comment", label: "Comment", icon: "message", tone: "neutral" },
    ],
    settings: [
      { key: "token", label: "Personal access token", kind: "secret", help: "Needs notifications and repo read access.", required: true },
      { key: "poll_seconds", label: "Poll interval (seconds)", kind: "number", help: "GitHub's requested interval wins if it is longer. Minimum 30.", required: false },
    ] },
  { ...base, id: "demo", name: "Sample data", icon: "sparkles", description: "Realistic sample work from every kind of source, so you can try tab before connecting anything.", available: true, enabled: true, status: { state: "running", last_sync: ago(1) }, kinds: [], settings: [] },
  { ...base, id: "slack", name: "Slack", icon: "slack", category: "communication", description: "Mentions, DMs and threads you're in, with who said it, where, and a short preview.", available: false, enabled: false, status: { state: "disabled" },
    rate_limits: [{ provider: "slack", resource: "default", requests: 50, per_seconds: 60, burst: 15, note: "Tier 3 methods: about 50 requests per minute" }],
    kinds: [
      { kind: "slack.mention", label: "Mention", icon: "slack", tone: "info" },
      { kind: "slack.dm", label: "Direct message", icon: "slack", tone: "info" },
      { kind: "slack.channel", label: "Channel", icon: "slack", tone: "neutral" },
    ], settings: [] },
  { ...base, id: "gmail", name: "Gmail", icon: "gmail", category: "communication", description: "Mail addressed to you, with newsletters and bots filtered down.", available: false, enabled: false, status: { state: "disabled" },
    kinds: [
      { kind: "email.direct", label: "Email", icon: "gmail", tone: "neutral" },
      { kind: "email.newsletter", label: "Newsletter", icon: "gmail", tone: "neutral" },
    ], settings: [] },
  { ...base, id: "zoom", name: "Zoom", icon: "zoom", category: "communication", description: "Action items assigned to you from meeting AI summaries.", available: false, enabled: false, status: { state: "disabled" },
    kinds: [{ kind: "meeting.action_item", label: "Meeting action item", icon: "zoom", tone: "accent" }], settings: [] },
  { ...base, id: "jira", name: "Jira", icon: "jira", category: "work", description: "Issues assigned to you, mentions, and status changes on work you own.", available: false, enabled: false, status: { state: "disabled" },
    kinds: [{ kind: "jira.assigned", label: "Assigned issue", icon: "jira", tone: "accent" }], settings: [] },
];

const usage: UsageRow[] = [
  { provider: "anthropic", connector: "ranker", purpose: "rank item", total: 42, network: 11, cache_hits: 0, revalidated: 0, stale: 0, rate_limited: 0, errors: 0, avg_ms: 812 },
  { provider: "github", connector: "github", purpose: "poll notifications", total: 60, network: 4, cache_hits: 0, revalidated: 56, stale: 0, rate_limited: 0, errors: 0, avg_ms: 214 },
  { provider: "github", connector: "github", purpose: "item preview", total: 23, network: 6, cache_hits: 17, revalidated: 0, stale: 0, rate_limited: 0, errors: 0, avg_ms: 301 },
  { provider: "github", connector: "github", purpose: "latest comment", total: 9, network: 9, cache_hits: 0, revalidated: 0, stale: 0, rate_limited: 0, errors: 0, avg_ms: 188 },
];

const outcomes: AuditEntry["outcome"][] = ["revalidated", "cache_hit", "network", "revalidated", "cache_hit", "network", "revalidated", "stale"];
const audit: AuditEntry[] = outcomes.map((outcome, i) => ({
  id: 100 - i, ts: ago(i * 1.5), provider: i % 4 === 2 ? "anthropic" : "github", connector: i % 4 === 2 ? "ranker" : "github",
  purpose: i % 4 === 2 ? "rank item" : i % 2 ? "item preview" : "poll notifications", method: i % 4 === 2 ? "POST" : "GET",
  url: i % 4 === 2 ? "https://api.anthropic.com/v1/messages" : i % 2 ? "https://api.github.com/repos/acme/payments/pulls/482" : "https://api.github.com/notifications?per_page=50",
  outcome, status: outcome === "cache_hit" ? 200 : outcome === "revalidated" ? 304 : 200, duration_ms: outcome === "cache_hit" ? 0 : 150 + i * 40,
  rl_resource: i % 4 === 2 ? "requests" : "core", rl_limit: i % 4 === 2 ? 4000 : 5000, rl_remaining: i % 4 === 2 ? 3988 : 4871 - i,
  rl_reset: new Date(Date.now() + 38 * 60_000).toISOString(), error: outcome === "stale" ? "network error, served cached copy" : null,
}));

const limits: RateLimitState[] = [
  { provider: "anthropic", resource: "requests", limit: 4000, remaining: 3988, reset_at: new Date(Date.now() + 40_000).toISOString(), blocked_until: null, updated_at: ago(1) },
  { provider: "github", resource: "core", limit: 5000, remaining: 4871, reset_at: new Date(Date.now() + 38 * 60_000).toISOString(), blocked_until: null, updated_at: ago(1) },
  { provider: "github", resource: "search", limit: 30, remaining: 2, reset_at: new Date(Date.now() + 45_000).toISOString(), blocked_until: null, updated_at: ago(3) },
];

let config: TabConfig = {
  profile: { name: "Vincent", role: "Engineer", handles: ["VincentCosta6"] },
  ranking: {
    rules: { kinds: [], default_score: 30, keywords: [{ contains: "outage", delta: 20 }, { contains: "urgent", delta: 15 }], vip_actors: [], vip_boost: 15, muted: [] },
    ai: { enabled: true, backend: "auto", api_key: "••••••••", model: null, instructions: "Anything from on-call or my manager is urgent. Dependabot can wait until Friday." },
  },
  connectors: { demo: { enabled: true, settings: {} }, tab: { enabled: true, settings: {} } },
  agent: {
    budget: { daily_usd: 10, monthly_usd: null, per_turn_usd: null, ranker_plan_cap: 0.8 }, kind: "claude_code", command: null, model: null, workdir: "C:\\Users\\vince\\code", permission_mode: "default", opencode_url: null },
  mcp: { port: 47821, token: "mock-token" },
  onboarded: true,
};

const listeners = new Set<(e: EngineEvent) => void>();
const emit = (e: EngineEvent) => listeners.forEach((l) => l(e));
const sorted = (xs: QueueItem[]) =>
  [...xs].sort((a, b) => (b.ranking?.score ?? 50) - (a.ranking?.score ?? 50) || b.occurred_at.localeCompare(a.occurred_at));
const setState = (id: string, state: ItemState, snoozed_until: string | null = null) => {
  items = items.map((i) => (i.id === id ? { ...i, state, snoozed_until, updated_at: new Date().toISOString() } : i));
  const item = items.find((i) => i.id === id) ?? null;
  if (item) emit({ type: "item_changed", change: state === "done" ? "done" : state === "snoozed" ? "snoozed" : "reopened", item });
  return item;
};

export const mockApi = {
  async invoke<T>(cmd: string, args: Record<string, any> = {}): Promise<T> {
    const r = await (async () => {
      switch (cmd) {
        case "agent_activity": return sessions.filter((s) => s.item_id && s.status !== "idle").map((s) => ({ item_id: s.item_id!, agent: s.agent, status: s.status, updated_at: s.updated_at }));
        case "graph_view": return mockGraph(args.projectId ?? null, args.days ?? null);
        case "graph_node": {
          const g = mockGraph(null, null);
          const entity = g.nodes.find((n) => n.id === args.id)!;
          const near = g.edges.filter((e) => e.src === args.id || e.dst === args.id).sort((a, b) => b.weight - a.weight);
          const connected = near.map((e) => ({ entity: g.nodes.find((n) => n.id === (e.src === args.id ? e.dst : e.src))!, shared: e.weight }));
          const hits = items.filter((i) => (entity.kind === "project" ? `project:${i.project_id}` === entity.key : i.entities.some((x) => x.key === entity.key)));
          return { entity, connected: connected.slice(0, 12), items: hits.slice(0, 12), notes: notes.filter((n) => `project:${n.project_id}` === entity.key) };
        }
        case "ask_chat_open": {
          const session = sessions.find((s) => s.cwd === `ask:${args.projectId ?? "all"}`) ?? null;
          return { session, messages: session ? messages.filter((m) => m.session_id === session.id) : [], approvals: [], running: false } satisfies Chat;
        }
        case "ask_chat_send": {
          const key = `ask:${args.projectId ?? "all"}`;
          let session = sessions.find((s) => s.cwd === key);
          if (!session) {
            session = { id: `sess-${Date.now()}`, item_id: null, agent: config.agent.kind ?? "claude_code", agent_session_id: null, title: "Ask", status: "working", cwd: key, created_at: new Date().toISOString(), updated_at: new Date().toISOString() };
            sessions.push(session);
          }
          simulateAskTurn(session, String(args.text), args.focus ?? []);
          return session;
        }
        case "search_index_status": {
          const on = !!config.connectors.tab?.settings?.semantic_search;
          if (!on) return { state: "off" };
          const total = items.length;
          const t = (Date.now() / 1000) % 12;
          if (t < 3) return { state: "downloading", model: "minishlab/potion-base-8M", received: Math.round((t / 3) * 31e6), total: 31e6 };
          if (t < 6) return { state: "indexing", model: "minishlab/potion-base-8M", done: Math.round(((t - 3) / 3) * total), total };
          return { state: "ready", model: "minishlab/potion-base-8M", dim: 256, docs: total, indexed_at: new Date().toISOString() };
        }
        case "projects_list": return [...projects].sort((a, b) => a.name.localeCompare(b.name));
        case "project_save": {
          const p: Project = { id: args.id ?? `p-${Date.now()}`, name: String(args.name).trim(), color: args.color, icon: args.icon, created_at: new Date().toISOString() };
          if (!p.name) throw new Error("a project needs a name");
          projects = [...projects.filter((x) => x.id !== p.id), p];
          return p;
        }
        case "project_delete": {
          projects = projects.filter((x) => x.id !== args.id);
          notes = notes.filter((n) => n.project_id !== args.id);
          items = items.map((i) => (i.project_id === args.id ? { ...i, project_id: null } : i));
          return null;
        }
        case "item_set_project": {
          const i = items.find((x) => x.id === args.id);
          if (!i) return null;
          i.project_id = args.projectId;
          emit({ type: "item_changed", change: "updated", item: i });
          return i;
        }
        case "project_notes": return notes.filter((n) => n.project_id === args.projectId);
        case "project_note_save": {
          const now = new Date().toISOString();
          const old = notes.find((n) => n.id === args.id);
          if (old) Object.assign(old, { text: args.text, added_by: "user", updated_at: now });
          else notes = [{ id: `n-${Date.now()}`, project_id: args.projectId, topic: args.topic, text: args.text, added_by: "user", session_id: null, sources: [], created_at: now, updated_at: now }, ...notes.filter((n) => !(n.project_id === args.projectId && n.topic.toLowerCase() === String(args.topic).toLowerCase()))];
          return null;
        }
        case "project_note_delete": notes = notes.filter((n) => n.id !== args.id); return null;
        case "list_items": return sorted(items.filter((i) => args.states.includes(i.state)));
        case "search_items": {
          const q = String(args.query).toLowerCase();
          return sorted(items.filter((i) => args.states.includes(i.state) && `${i.title} ${i.body} ${i.actor} ${i.context}`.toLowerCase().includes(q)));
        }
        case "github_link_summary":
          return args.what === "pull"
            ? { title: "Retry webhook deliveries with exponential backoff", state: "open", author: "dana-k", checks: "failing", updated_at: new Date(Date.now() - 3_600_000).toISOString() }
            : { title: "Sample " + args.what, state: "open", author: "sam", checks: null, updated_at: null };
        case "item_by_url": {
          const u = String(args.url).replace(/[?#].*$/, "").replace(/\/$/, "");
          return items.find((i) => i.url && (i.url === u || /^[/#?]/.test(i.url.slice(u.length)) && i.url.startsWith(u))) ?? null;
        }
        case "item_detail": {
          const item = items.find((i) => i.id === args.id);
          if (!item) return null;
          const keys = new Set(item.entities.map((e) => e.key));
          const related = items.filter((o) => o.id !== item.id && o.entities.some((e) => keys.has(e.key)));
          return { item, related };
        }
        case "item_preview": {
          const item = items.find((i) => i.id === args.id);
          const wrap = (blocks: Block[]) => ({ item_id: args.id, blocks, fetched_at: ago(3), stale: false });
          if (!item || !(item.kind.startsWith("github.") || item.kind === "agent.needs_review")) return wrap([]);
          return wrap([
            { type: "stats", items: [{ label: "added", value: "+612", tone: "success" }, { label: "removed", value: "−203", tone: "danger" }, { label: "files", value: "14" }, { label: "commits", value: "6" }] },
            { type: "fields", items: [{ label: "State", value: "Open", tone: "success" }, { label: "Branch", value: "idempotency-keys → main" }, { label: "Checks", value: "11 passed, 1 pending", tone: "warning" }] },
            { type: "text", markdown: true, text: "Adds an `Idempotency-Key` header to all webhook deliveries and stores processed keys for 24h.\n\n- Dedupes retries from the queue worker\n- Backfills keys for in-flight deliveries" },
          ] satisfies Block[]);
        }
        case "mark_done": return setState(args.id, "done");
        case "snooze": return setState(args.id, "snoozed", new Date(Date.now() + args.minutes * 60_000).toISOString());
        case "reopen": return setState(args.id, "open");
        case "list_connectors": return connectors;
        case "rate_limits": return limits;
        case "audit_log": return audit;
        case "api_usage": return usage;
        case "get_config": return config;
        case "save_config": config = args.config; return config;
        case "set_agent_model": config = { ...config, agent: { ...config.agent, model: args.model } }; return config;
        case "connection_set_secrets": {
          const c = config.connectors[args.id] ?? { enabled: false, settings: {} };
          config = { ...config, connectors: { ...config.connectors, [args.id]: { enabled: true, settings: { ...c.settings, ...Object.fromEntries(Object.keys(args.values).map((k) => [k, "••••••••"])) } } } };
          return config;
        }
        case "rate_pressure": return pressure();
        case "item_context": {
          const item = items.find((i) => i.id === args.id);
          if (!item) return null;
          const entries = contexts.filter((c) => c.item_id === item.id);
          const keys = new Set(item.entities.map((e) => e.key));
          const attached = new Set(entries.map((e) => e.ref_item?.id));
          const related = items.filter((o) => o.id !== item.id && !attached.has(o.id) && o.entities.some((e) => keys.has(e.key))).slice(0, 6);
          return { item, entries, related };
        }
        case "context_add": {
          const ref = items.find((i) => i.id === args.entry.ref_item_id) ?? null;
          const e: ContextEntry = { id: `ctx-${Date.now()}`, item_id: args.itemId, section: args.entry.section, pinned: false, ref_item: ref, url: args.entry.url ?? null, title: args.entry.title, summary: args.entry.summary ?? null, added_by: args.entry.added_by, created_at: new Date().toISOString() };
          contexts.push(e);
          emit({ type: "context_changed", item_id: args.itemId });
          return e;
        }
        case "context_update": {
          const e = contexts.find((c) => c.id === args.id);
          if (!e) return null;
          if (args.section) e.section = args.section;
          if (args.pinned !== null && args.pinned !== undefined) e.pinned = args.pinned;
          emit({ type: "context_changed", item_id: e.item_id });
          return e;
        }
        case "context_remove": {
          const e = contexts.find((c) => c.id === args.id);
          contexts = contexts.filter((c) => c.id !== args.id);
          if (e) emit({ type: "context_changed", item_id: e.item_id });
          return null;
        }
        case "create_task": return createTask(args.task.title, args.task.body ?? null);
        case "chat_open": {
          const session = sessions.find((s) => s.item_id === args.itemId) ?? null;
          return { session, messages: session ? messages.filter((m) => m.session_id === session.id) : [], approvals: [], running: false } satisfies Chat;
        }
        case "chat_send": {
          const item = args.itemId ? items.find((i) => i.id === args.itemId)! : createTask(String(args.text).split("\n")[0].slice(0, 80), null, args.projectId ?? null);
          let session = sessions.find((s) => s.item_id === item.id);
          if (!session) {
            session = { id: `sess-${Date.now()}`, item_id: item.id, agent: config.agent.kind ?? "claude_code", agent_session_id: null, title: item.title, status: "working", cwd: "~/code", created_at: new Date().toISOString(), updated_at: new Date().toISOString() };
            sessions.push(session);
          }
          simulateTurn(session, item, String(args.text));
          return { item, session };
        }
        case "review_chat_open": {
          const session = sessions.find((s) => s.cwd === `review:${args.repo}#${args.number}`) ?? null;
          return { session, messages: session ? messages.filter((m) => m.session_id === session.id) : [], approvals: [], running: false } satisfies Chat;
        }
        case "review_chat_send": {
          const key = `review:${args.target.repo}#${args.target.number}`;
          let session = sessions.find((s) => s.cwd === key);
          if (!session) {
            session = { id: `sess-${Date.now()}`, item_id: null, agent: config.agent.kind ?? "claude_code", agent_session_id: null, title: args.target.title, status: "working", cwd: key, created_at: new Date().toISOString(), updated_at: new Date().toISOString() };
            sessions.push(session);
          }
          simulateReviewTurn(session, String(args.text));
          return session;
        }
        case "set_agent_mode": config = { ...config, agent: { ...config.agent, permission_mode: args.mode } }; return config;
        case "chat_stop": return null;
        case "approval_resolve": return true;
        case "detect_agents": return [
          { kind: "opencode", found: false, path: null, version: null, error: "`opencode` was not found on PATH." },
          { kind: "claude_code", found: true, path: "C:\\Users\\vince\\AppData\\Roaming\\npm\\claude.cmd", version: "2.1.318 (Claude Code)", error: null },
        ];
        case "mcp_info": return { url: "http://127.0.0.1:47821/mcp", token: "mock-token" };
        case "github_pull": return samplePull(args.repo, args.number);
        case "lsp_servers": return lspServers;
        case "lsp_install": {
          await new Promise((r) => setTimeout(r, 1800));
          lspServers = lspServers.map((s) => (s.id === args.id ? { ...s, installed: true, path: `/Users/you/go/bin/${s.command}` } : s));
          return lspServers.find((s) => s.id === args.id);
        }
        case "lsp_start": throw new Error("Language servers run in the desktop app.");
        case "gh_status": return gh;
        case "usage_summary": {
          const in3h = new Date(Date.now() + 3 * 3600_000).toISOString();
          const in4d = new Date(Date.now() + 4 * 86400_000).toISOString();
          const now = new Date().toISOString();
          return {
            today_usd: 3.42, month_usd: 41.8, today_agent_usd: 3.31, today_ranker_usd: 0.11, today_turns: 14, today_tokens: 912_000,
            billing: "subscription",
            windows: [
              { window: "five_hour", utilization: 0.34, resets_at: in3h, status: "allowed", updated_at: now },
              { window: "seven_day", utilization: 0.82, resets_at: in4d, status: "allowed_warning", updated_at: now },
            ],
          };
        }
        case "gh_install":
          await new Promise((r) => setTimeout(r, 1500));
          gh = { ...gh, installed: true, path: "/opt/homebrew/bin/gh", version: "2.89.0" };
          return gh;
        case "gh_login":
          setTimeout(() => ghCodeListeners.forEach((l) => l({ code: "8F2C-41DA", url: "https://github.com/login/device" })), 400);
          await new Promise((r) => setTimeout(r, 6000));
          gh = { ...gh, logged_in: true, user: "vincentcosta6" };
          return gh;
        default: throw new Error(`mock: unknown command ${cmd}`);
      }
    })();
    return structuredClone(r) as T;
  },
  subscribe(cb: (e: EngineEvent) => void) {
    listeners.add(cb);
    return () => listeners.delete(cb);
  },
  onWorkspace(_cb: (p: WorkspaceProgress) => void) {
    return () => {};
  },
  onGhCode(cb: (c: GhLoginCode) => void) {
    ghCodeListeners.add(cb);
    return () => void ghCodeListeners.delete(cb);
  },
};

let lspServers: LspServer[] = [
  { id: "gopls", name: "Go (gopls)", languages: ["go"], command: "gopls", note: "Hover, go to definition, references and errors across the module.", installed: false, path: null, install_command: "go install golang.org/x/tools/gopls@latest", needs: null, needs_url: null },
  { id: "sqls", name: "SQL (sqls)", languages: ["sql"], command: "sqls", note: "Syntax errors and outline. Table and column info needs a database connection in sqls's own config.", installed: false, path: null, install_command: "go install github.com/sqls-server/sqls@latest", needs: null, needs_url: null },
  { id: "rust-analyzer", name: "Rust (rust-analyzer)", languages: ["rust"], command: "rust-analyzer", note: "Hover, go to definition and references. Indexing a large workspace takes a minute.", installed: true, path: "/Users/you/.cargo/bin/rust-analyzer", install_command: "rustup component add rust-analyzer", needs: null, needs_url: null },
  { id: "pyright", name: "Python (pyright)", languages: ["python"], command: "pyright-langserver", note: "Hover, go to definition, references and type errors.", installed: false, path: null, install_command: null, needs: "npm", needs_url: "https://nodejs.org" },
];

let gh: GhStatus = { installed: false, path: null, version: null, logged_in: false, user: null, install_command: "brew install gh", error: null };
const ghCodeListeners = new Set<(c: GhLoginCode) => void>();

// A link an agent attached, to show links rendered by what they point at.
let contexts: ContextEntry[] = items.slice(0, 4).map((i) => ({ id: `ctx-seed-${i.id}`, item_id: i.id, section: "found", pinned: false, ref_item: null, url: "https://github.com/acme/payments/pull/491", title: "https://github.com/acme/payments/pull/491", summary: null, added_by: "agent", created_at: new Date().toISOString() }));
let projects: Project[] = [
  { id: "p-pay", name: "Payments", color: "#7c5cff", icon: "zap", created_at: ago(9000) },
  { id: "p-sec", name: "SOC 2", color: "#e5484d", icon: "shield", created_at: ago(9000) },
];
let notes: ProjectNote[] = [
  { id: "n1", project_id: "p-pay", topic: "deploys", text: "Payments deploys go through the staging branch; prod is cut on Tuesdays and Thursdays.", added_by: "agent", session_id: null, sources: [], created_at: ago(3000), updated_at: ago(3000) },
  { id: "n2", project_id: "p-pay", topic: "owners", text: "Dana owns checkout; Priya owns the ledger service.", added_by: "user", session_id: null, sources: [], created_at: ago(900), updated_at: ago(900) },
];
// Put some of the sample work into projects.
items = items.map((i) => ({ ...i, project_id: /checkout|payment|PAY-|ledger|refund/i.test(`${i.title} ${i.body}`) ? "p-pay" : /SOC 2|security|audit/i.test(`${i.title} ${i.body}`) ? "p-sec" : null }));
const sessions: Session[] = [];
const messages: SessionMessage[] = [];
let msgId = 1;

function createTask(title: string, body: string | null, projectId: string | null = null): QueueItem {
  const now = new Date().toISOString();
  const item: QueueItem = {
    id: `task-${Date.now()}`, source: "tab", external_id: String(Date.now()), kind: "tab.task", title, body, url: null, actor: "you", context: "tab",
    occurred_at: now, blocks: [], entities: [], metadata: {}, state: "open", snoozed_until: null, project_id: projectId, created_at: now, updated_at: now,
    ranking: { score: 62, base_score: 62, ai_adjustment: null, urgency: "today", reason: "A task you created", ranked_by: "rules" },
  };
  items = [item, ...items];
  emit({ type: "item_changed", change: "ingested", item });
  return item;
}

function post(session: Session, role: SessionMessage["role"], kind: SessionMessage["kind"], content: string, meta: unknown = null) {
  const m: SessionMessage = { id: msgId++, session_id: session.id, role, kind, content, meta, created_at: new Date().toISOString() };
  messages.push(m);
  emit({ type: "session_message", message: m });
}

function status(session: Session, s: Session["status"]) {
  session.status = s;
  session.agent_session_id = session.agent_session_id ?? "4f1c-mock";
  emit({ type: "session_changed", session: { ...session } });
}

/** Plays back a plausible agent turn so the chat can be designed in a browser. */
function simulateTurn(session: Session, item: QueueItem, text: string) {
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  (async () => {
    post(session, "user", "text", text);
    status(session, "working");
    await sleep(500);
    post(session, "assistant", "tool", "tab: tab_search: checkout", { tool: "mcp__tab__tab_search" });
    post(session, "tool", "tool_result", "2 results");
    const related = items.find((i) => i.id !== item.id && i.kind.startsWith("slack."));
    if (related) {
      contexts.push({ id: `ctx-${Date.now()}`, item_id: item.id, section: "found", pinned: false, ref_item: related, url: null, title: related.title, summary: "Same incident; on-call is asking about the cart cache.", added_by: "agent", created_at: new Date().toISOString() });
      emit({ type: "context_changed", item_id: item.id });
      post(session, "assistant", "tool", `tab: tab_add_found: ${related.title}`, { tool: "mcp__tab__tab_add_found" });
      post(session, "tool", "tool_result", "Added under found.");
    }
    const reply = `I looked through your queue for context on **${item.title}**.\n\n- The #incident-checkout thread is the same problem, so I filed it under Found.\n- Next I'd check the cart cache TTL change from the 14:10 deploy.\n\nWant me to open a branch and draft the fix?`;
    for (let i = 0; i < reply.length; i += 12) {
      emit({ type: "session_delta", session_id: session.id, text: reply.slice(i, i + 12) });
      await sleep(30);
    }
    post(session, "assistant", "text", reply);
    post(session, "system", "result", "", { cost_usd: 0.021, duration_ms: 8400 });
    status(session, "idle");
  })();
}

/** The demo items' entities as a graph, with projects as nodes. */
function mockGraph(projectId: string | null, days: number | null): GraphView {
  const since = days ? Date.now() - days * 86_400_000 : 0;
  const scoped = items.filter((i) => (!projectId || i.project_id === projectId) && Date.parse(i.occurred_at) >= since);
  const ids = new Map<string, Entity>();
  const node = (kind: string, key: string, label: string) => {
    let n = ids.get(key);
    if (!n) ids.set(key, (n = { id: ids.size + 1, kind, key, label, item_count: 0 }));
    n.item_count++;
    return n.id;
  };
  const weights = new Map<string, number>();
  for (const i of scoped) {
    const here = i.entities.map((e) => node(e.kind, e.key, e.label));
    const p = projects.find((x) => x.id === i.project_id);
    if (p) here.push(node("project", `project:${p.id}`, p.name));
    const uniq = [...new Set(here)].sort((a, b) => a - b);
    uniq.forEach((a, k) => uniq.slice(k + 1).forEach((b) => weights.set(`${a}-${b}`, (weights.get(`${a}-${b}`) ?? 0) + 1)));
  }
  return {
    nodes: [...ids.values()],
    edges: [...weights].map(([k, weight]) => { const [src, dst] = k.split("-").map(Number); return { src, dst, weight }; }),
  };
}

function simulateAskTurn(session: Session, text: string, focus: string[]) {
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  (async () => {
    post(session, "user", "text", focus.length ? `(Selected in the graph: ${focus.join(", ")})\n\n${text}` : text);
    status(session, "working");
    await sleep(400);
    post(session, "assistant", "tool", `tab: tab_graph_search: ${focus[0] ?? "payments"}`, { tool: "mcp__tab__tab_graph_search" });
    post(session, "tool", "tool_result", "3 matches");
    const reply = `**${focus[0] ?? "acme/payments"}** sits at the center of the Payments work: Dana reviews most of its PRs, and the checkout incident and the idempotency-keys PR both touch it.\n\n- Open now: the p99 latency on /checkout and Dana's review request.\n- The project note says deploys go through staging on Tuesdays and Thursdays.`;
    for (let i = 0; i < reply.length; i += 14) {
      emit({ type: "session_delta", session_id: session.id, text: reply.slice(i, i + 14) });
      await sleep(25);
    }
    post(session, "assistant", "text", reply);
    post(session, "system", "result", "", { cost_usd: 0.009, duration_ms: 4100 });
    status(session, "idle");
  })();
}

/** A canned review answer: explains the sample's capture loop, with code links. */
function simulateReviewTurn(session: Session, text: string) {
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  (async () => {
    post(session, "user", "text", text);
    status(session, "working");
    await sleep(400);
    const fixes = /fix list/i.test(text);
    if (fixes) {
      post(session, "assistant", "tool", "Edit src/capture.ts", { tool: "Edit" });
      post(session, "tool", "tool_result", "Applied 1 edit");
      await sleep(300);
    } else {
      post(session, "assistant", "tool", "Read src/backoff.ts", { tool: "Read" });
      post(session, "tool", "tool_result", "5 lines");
    }
    const reply = fixes
      ? "Done, one change per item:\n\n- Stopped retrying `card_declined`: [isRetryable](src/capture.ts#L23) now only matches timeouts, resets and rate limits.\n\nNothing is committed; review the change in the checkout."
      : "It retries the capture up to [MAX_ATTEMPTS](src/capture.ts#L5) times. Each attempt reuses the same idempotency key, so Stripe never captures twice, and failures that [isRetryable](src/capture.ts#L23) rejects are thrown at once.\n\nBetween attempts it waits with [backoff](src/backoff.ts#L2), which sleeps a random time up to 250ms × 2^attempt, capped at 8s.";
    for (let i = 0; i < reply.length; i += 14) {
      emit({ type: "session_delta", session_id: session.id, text: reply.slice(i, i + 14) });
      await sleep(25);
    }
    post(session, "assistant", "text", reply);
    post(session, "system", "result", "", { cost_usd: 0.012, duration_ms: 5200 });
    status(session, "idle");
  })();
}

function pressure(): Pressure[] {
  return [
    { provider: "anthropic", resource: "requests", used: 11, budget: 50, window_seconds: 60, remote_remaining: 3988, remote_limit: 4000, headroom: 0.78, recent: 2, burst_threshold: 10, cooling_until: null, deferred: 0 },
    { provider: "github", resource: "core", used: 129, budget: 5000, window_seconds: 3600, remote_remaining: 4871, remote_limit: 5000, headroom: 0.97, recent: 3, burst_threshold: 40, cooling_until: null, deferred: 0 },
    { provider: "github", resource: "search", used: 28, budget: 30, window_seconds: 60, remote_remaining: 2, remote_limit: 30, headroom: 0.07, recent: 12, burst_threshold: 10, cooling_until: new Date(Date.now() + 22_000).toISOString(), deferred: 4 },
  ];
}

function samplePull(repo: string, number: number): PullReview {
  const before = `import { Charge } from "./types";\nimport { stripe } from "./clients";\n\nexport async function capture(charge: Charge) {\n  const result = await stripe.charges.capture(charge.id);\n  if (result.status !== "succeeded") {\n    throw new Error(\`capture failed: \${result.status}\`);\n  }\n  return result;\n}\n`;
  const after = `import { Charge } from "./types";\nimport { stripe } from "./clients";\nimport { backoff } from "./backoff";\n\nconst MAX_ATTEMPTS = 4;\n\nexport async function capture(charge: Charge) {\n  for (let attempt = 1; ; attempt++) {\n    try {\n      const result = await stripe.charges.capture(charge.id, {\n        idempotencyKey: \`capture-\${charge.id}\`,\n      });\n      if (result.status !== "succeeded") {\n        throw new Error(\`capture failed: \${result.status}\`);\n      }\n      return result;\n    } catch (err) {\n      if (attempt >= MAX_ATTEMPTS || !isRetryable(err)) throw err;\n      await backoff(attempt);\n    }\n  }\n}\n\nfunction isRetryable(err: unknown) {\n  return err instanceof Error && /timeout|ECONNRESET|rate limit/i.test(err.message);\n}\n`;
  const backoff = `/** Exponential backoff with full jitter, capped at 8 seconds. */\nexport function backoff(attempt: number) {\n  const ceiling = Math.min(8000, 250 * 2 ** attempt);\n  return new Promise((resolve) => setTimeout(resolve, Math.random() * ceiling));\n}\n`;
  const testBefore = `import { capture } from "../src/capture";\n\ntest("captures a charge", async () => {\n  await expect(capture(fixture("ch_ok"))).resolves.toMatchObject({ status: "succeeded" });\n});\n`;
  const testAfter = testBefore + `\ntest("retries timeouts with the same idempotency key", async () => {\n  const calls = mockStripe(["timeout", "timeout", "succeeded"]);\n  await capture(fixture("ch_flaky"));\n  expect(calls.map((c) => c.idempotencyKey)).toEqual(Array(3).fill("capture-ch_flaky"));\n});\n`;
  const f = (filename: string, status: string, additions: number, deletions: number, original: string, modified: string) =>
    ({ filename, status, additions, deletions, previous_filename: null, viewed: "unviewed", original, modified });
  return {
    repo, number, node_id: "sample", title: "Retry payment capture with idempotency keys",
    body: "Captures sometimes time out on Stripe's side and we drop the charge.\n\nThis retries timeouts and connection resets up to four times with jittered backoff, and sends an idempotency key so a retry can't double-capture.\n\n- `capture()` retries transient errors\n- new `backoff()` helper\n- test for the retry path",
    author: "dana-k", author_avatar: null, url: `https://github.com/${repo}/pull/${number}`,
    state: "open", draft: false, base_ref: "main", head_ref: "dana/capture-retries", head_repo: repo,
    base_sha: "sample-base", head_sha: "sample-head", additions: 29, deletions: 4, commits: 3, created_at: ago(180),
    mergeable: "mergeable", merge_state: "unstable", merge_method: "squash", review_decision: "review_required",
    checks: { state: "failure", runs: [
      { name: "lint", status: "completed", conclusion: "success", url: null },
      { name: "unit-tests (node 22)", status: "completed", conclusion: "failure", url: null },
      { name: "build", status: "completed", conclusion: "success", url: null },
    ] },
    labels: [{ name: "payments", color: "0e8a16" }], reviewers: ["you"], viewer: "you", viewer_can_update: true,
    files: [
      f("src/capture.ts", "modified", 18, 4, before, after),
      f("src/backoff.ts", "added", 5, 0, "", backoff),
      f("test/capture.test.ts", "modified", 6, 0, testBefore, testAfter),
    ],
    threads: [{
      id: "sample-thread-1", path: "src/capture.ts", line: 19, start_line: null, original_line: 19, side: "RIGHT",
      resolved: false, outdated: false, can_resolve: true, can_reply: true, diff_hunk: null,
      comments: [{ id: "sample-comment-1", database_id: 1, author: "dana-k", avatar: null, body: "Should `card_declined` ever be retried? I think this regex is enough but worth a second look.", created_at: ago(60), pending: false, url: null, viewer_did_author: false }],
    }],
    pending_review: null,
    sample_tree: [
      { path: "db/migrations/0007_refund_reason.sql", content: "-- Refunds keep the reason the customer gave.\nALTER TABLE refunds ADD COLUMN reason text;\n\nCREATE INDEX refunds_charge_id_idx ON refunds (charge_id);\n" },
      { path: "README.md", content: "# payments\n\nCharges, captures and refunds for acme.\n" },
      { path: "package.json", content: "{\n  \"name\": \"@acme/payments\",\n  \"private\": true,\n  \"scripts\": { \"test\": \"vitest\" }\n}\n" },
      { path: "src/types.ts", content: "export interface Charge {\n  id: string;\n  amount: number;\n  currency: string;\n}\n" },
      { path: "src/clients.ts", content: "import Stripe from \"stripe\";\n\nexport const stripe = new Stripe(process.env.STRIPE_KEY!);\n" },
      { path: "src/refund.ts", content: "import { Charge } from \"./types\";\nimport { stripe } from \"./clients\";\n\nexport async function refund(charge: Charge) {\n  return stripe.refunds.create({ charge: charge.id });\n}\n" },
    ],
    sample: true,
  };
}
