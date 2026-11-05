// Browser-only stand-in for the Rust backend. Mirrors the demo connector's
// sample data closely enough to design against.

import type {
  AuditEntry, Block, ConnectorInfo, EngineEvent, ItemState, QueueItem, RateLimitState, TabConfig, Urgency, UsageRow,
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

const connectors: ConnectorInfo[] = [
  { id: "github", name: "GitHub", icon: "github", description: "Review requests, CI failures, mentions and comments from your GitHub notifications.", available: true, enabled: false, status: { state: "disabled" },
    kinds: [
      { kind: "github.review_requested", label: "Review requested", icon: "git-pull-request", tone: "accent" },
      { kind: "github.ci_failed", label: "CI failed", icon: "x-circle", tone: "danger" },
      { kind: "github.comment", label: "Comment", icon: "message", tone: "neutral" },
    ],
    settings: [
      { key: "token", label: "Personal access token", kind: "secret", help: "Needs notifications and repo read access.", required: true },
      { key: "poll_seconds", label: "Poll interval (seconds)", kind: "number", help: "GitHub's requested interval wins if it is longer. Minimum 30.", required: false },
    ] },
  { id: "demo", name: "Sample data", icon: "sparkles", description: "Realistic sample work from every kind of source, so you can try tab before connecting anything.", available: true, enabled: true, status: { state: "running", last_sync: ago(1) }, kinds: [], settings: [] },
  { id: "agents", name: "Agent sessions", icon: "bot", description: "Claude Code, Codex, OpenCode or any CLI agent, streamed live with its terminal. Sessions that need you land in the queue.", available: false, enabled: false, status: { state: "disabled" },
    kinds: [
      { kind: "agent.needs_input", label: "Agent needs input", icon: "bot", tone: "warning" },
      { kind: "agent.needs_review", label: "Agent ready for review", icon: "bot", tone: "accent" },
    ], settings: [] },
  { id: "slack", name: "Slack", icon: "slack", description: "Mentions, DMs and threads you're in.", available: false, enabled: false, status: { state: "disabled" },
    kinds: [
      { kind: "slack.mention", label: "Mention", icon: "slack", tone: "info" },
      { kind: "slack.dm", label: "Direct message", icon: "slack", tone: "info" },
      { kind: "slack.channel", label: "Channel", icon: "slack", tone: "neutral" },
    ], settings: [] },
  { id: "gmail", name: "Gmail", icon: "mail", description: "Mail addressed to you, with newsletters and bots filtered down.", available: false, enabled: false, status: { state: "disabled" },
    kinds: [
      { kind: "email.direct", label: "Email", icon: "mail", tone: "neutral" },
      { kind: "email.newsletter", label: "Newsletter", icon: "mail", tone: "neutral" },
    ], settings: [] },
  { id: "zoom", name: "Zoom", icon: "video", description: "Action items assigned to you from meeting AI summaries.", available: false, enabled: false, status: { state: "disabled" },
    kinds: [{ kind: "meeting.action_item", label: "Meeting action item", icon: "video", tone: "accent" }], settings: [] },
  { id: "jira", name: "Jira", icon: "ticket", description: "Issues assigned to you, mentions, and status changes on work you own.", available: false, enabled: false, status: { state: "disabled" }, kinds: [], settings: [] },
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
  connectors: { demo: { enabled: true, settings: {} } },
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
          if (!item || !(item.kind.startsWith("github.") || item.kind === "agent.needs_review")) return [];
          return [
            { type: "stats", items: [{ label: "added", value: "+612", tone: "success" }, { label: "removed", value: "−203", tone: "danger" }, { label: "files", value: "14" }, { label: "commits", value: "6" }] },
            { type: "fields", items: [{ label: "State", value: "Open", tone: "success" }, { label: "Branch", value: "idempotency-keys → main" }, { label: "Checks", value: "11 passed, 1 pending", tone: "warning" }] },
            { type: "text", markdown: true, text: "Adds an `Idempotency-Key` header to all webhook deliveries and stores processed keys for 24h.\n\n- Dedupes retries from the queue worker\n- Backfills keys for in-flight deliveries" },
          ] satisfies Block[];
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
