// Browser-only stand-in for the Rust backend. Mirrors the demo connector's
// sample data closely enough to design against.

import type {
  AuditEntry, Block, Chat, ConnectorInfo, ContextEntry, EngineEvent, ItemState, Pressure, QueueItem, RateLimitState,
  Session, SessionMessage, TabConfig, Urgency, UsageRow,
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
    ai: { enabled: true, api_key: "••••••••", model: null, instructions: "Anything from on-call or my manager is urgent. Dependabot can wait until Friday." },
  },
  connectors: { demo: { enabled: true, settings: {} }, tab: { enabled: true, settings: {} } },
  agent: { kind: "claude_code", command: null, model: null, workdir: "C:\\Users\\vince\\code", permission_mode: "default", opencode_url: null },
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
    const r = (() => {
      switch (cmd) {
        case "list_items": return sorted(items.filter((i) => args.states.includes(i.state)));
        case "search_items": {
          const q = String(args.query).toLowerCase();
          return sorted(items.filter((i) => args.states.includes(i.state) && `${i.title} ${i.body} ${i.actor} ${i.context}`.toLowerCase().includes(q)));
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
          const item = args.itemId ? items.find((i) => i.id === args.itemId)! : createTask(String(args.text).split("\n")[0].slice(0, 80), null);
          let session = sessions.find((s) => s.item_id === item.id);
          if (!session) {
            session = { id: `sess-${Date.now()}`, item_id: item.id, agent: config.agent.kind ?? "claude_code", agent_session_id: null, title: item.title, status: "working", cwd: "~/code", created_at: new Date().toISOString(), updated_at: new Date().toISOString() };
            sessions.push(session);
          }
          simulateTurn(session, item, String(args.text));
          return { item, session };
        }
        case "chat_stop": return null;
        case "approval_resolve": return true;
        case "detect_agents": return [
          { kind: "opencode", found: false, path: null, version: null, error: "`opencode` was not found on PATH." },
          { kind: "claude_code", found: true, path: "C:\\Users\\vince\\AppData\\Roaming\\npm\\claude.cmd", version: "2.1.318 (Claude Code)", error: null },
        ];
        case "mcp_info": return { url: "http://127.0.0.1:47821/mcp", token: "mock-token" };
        case "github_pull": return samplePull(args.repo, args.number);
        default: throw new Error(`mock: unknown command ${cmd}`);
      }
    })();
    return structuredClone(r) as T;
  },
  subscribe(cb: (e: EngineEvent) => void) {
    listeners.add(cb);
    return () => listeners.delete(cb);
  },
};

let contexts: ContextEntry[] = [];
const sessions: Session[] = [];
const messages: SessionMessage[] = [];
let msgId = 1;

function createTask(title: string, body: string | null): QueueItem {
  const now = new Date().toISOString();
  const item: QueueItem = {
    id: `task-${Date.now()}`, source: "tab", external_id: String(Date.now()), kind: "tab.task", title, body, url: null, actor: "you", context: "tab",
    occurred_at: now, blocks: [], entities: [], metadata: {}, state: "open", snoozed_until: null, created_at: now, updated_at: now,
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

function pressure(): Pressure[] {
  return [
    { provider: "anthropic", resource: "requests", used: 11, budget: 50, window_seconds: 60, remote_remaining: 3988, remote_limit: 4000, headroom: 0.78, recent: 2, burst_threshold: 10, cooling_until: null, deferred: 0 },
    { provider: "github", resource: "core", used: 129, budget: 5000, window_seconds: 3600, remote_remaining: 4871, remote_limit: 5000, headroom: 0.97, recent: 3, burst_threshold: 40, cooling_until: null, deferred: 0 },
    { provider: "github", resource: "search", used: 28, budget: 30, window_seconds: 60, remote_remaining: 2, remote_limit: 30, headroom: 0.07, recent: 12, burst_threshold: 10, cooling_until: new Date(Date.now() + 22_000).toISOString(), deferred: 4 },
  ];
}

function samplePull(repo: string, number: number) {
  const before = `import { Charge } from "./types";\nimport { stripe } from "./clients";\n\nexport async function capture(charge: Charge) {\n  const result = await stripe.charges.capture(charge.id);\n  if (result.status !== "succeeded") {\n    throw new Error(\`capture failed: \${result.status}\`);\n  }\n  return result;\n}\n`;
  const after = `import { Charge } from "./types";\nimport { stripe } from "./clients";\nimport { backoff } from "./backoff";\n\nconst MAX_ATTEMPTS = 4;\n\nexport async function capture(charge: Charge) {\n  for (let attempt = 1; ; attempt++) {\n    try {\n      const result = await stripe.charges.capture(charge.id, {\n        idempotencyKey: \`capture-\${charge.id}\`,\n      });\n      if (result.status !== "succeeded") {\n        throw new Error(\`capture failed: \${result.status}\`);\n      }\n      return result;\n    } catch (err) {\n      if (attempt >= MAX_ATTEMPTS || !isRetryable(err)) throw err;\n      await backoff(attempt);\n    }\n  }\n}\n`;
  return {
    repo, number, title: "Retry payment capture with idempotency keys", author: "dana-k", url: `https://github.com/${repo}/pull/${number}`,
    base_sha: "sample-base", head_sha: "sample-head", sample: true,
    files: [
      { filename: "src/capture.ts", status: "modified", additions: 18, deletions: 4, original: before, modified: after },
      { filename: "src/backoff.ts", status: "added", additions: 5, deletions: 0, original: "", modified: "export function backoff(attempt: number) {\n  const ceiling = Math.min(8000, 250 * 2 ** attempt);\n  return new Promise((r) => setTimeout(r, Math.random() * ceiling));\n}\n" },
    ],
    comments: [{ id: 1, path: "src/capture.ts", line: 18, side: "RIGHT", body: "Should card_declined ever be retried?", user: "dana-k", created_at: ago(60) }],
  };
}
