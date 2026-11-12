// Browser-only stand-in for the Rust backend. Mirrors the demo connector's
// sample data closely enough to design against.

import { NOT_HANDLED, withActions, workflowMock } from "./mock-workflows";
// The same harness metadata the app ships.
import harnessData from "../../crates/tab-core/src/agents/harnesses.json";
import { create, toBinary } from "@bufbuild/protobuf";
import { timestampFromDate, timestampNow } from "@bufbuild/protobuf/wkt";
import { ErrorCode, HubKind, HubListSchema, HubSchema, HubState, HubStatusSchema, PlanSchema, Role, RunnerState, RunnerStatusSchema, type HubStatus } from "./protocol/cloud";
import type {
  McpCatalogEntry, McpCloudState, JobGaps, McpImportCandidate, McpImportResult, McpProbe, McpServerConfig, McpServerInfo, McpSignInEvent, McpTool,
  EmailSignInStatus, MailProvider,
  Account, AgentKind, GhLoginCode, GhStatus, Harness, LinkSummary, LspServer, ModelOption,
  AuditEntry, Block, Chat, ConnectorInfo, ContextEntry, ContextStatus, EngineEvent, ItemState, Pressure, QueueItem, RateLimitState,
  Entity, GraphView, Project, ProjectNote, ProjectRepo, ProjectResource, ResourceInput, Segment, PullReview, Session, SessionMessage, TabConfig, Urgency, UsageRow, WorkspaceProgress,
} from "./types";

const harnesses = harnessData as unknown as Harness[];
// `?agents=none` starts with no agent installed, `?agents=claude_code,codex` with those.
const mockInstalled = new Set<AgentKind>(
  ((typeof location !== "undefined" && new URLSearchParams(location.search).get("agents")) || "claude_code").split(",").filter((k) => k !== "none") as AgentKind[],
);
const mockOpenCode = () => mockInstalled.has("opencode");
const mockModels = (): ModelOption[] => {
  const claude = (harnessData[0].models as { id: string; label: string; common?: boolean }[]).map((m) => ({ agent: "claude_code" as AgentKind, id: m.id, label: m.label, common: !!m.common }));
  if (!mockOpenCode()) return mockInstalled.has("claude_code") ? claude : [];
  const common = harnessData[1].common_models as string[];
  const oc = ["anthropic/claude-sonnet-5-5", "anthropic/claude-opus-5-5", "anthropic/claude-haiku-5-5", "openai/gpt-5.5", "openai/gpt-5.5-mini", "google/gemini-3-pro", "ollama/qwen3:32b", "openrouter/deepseek/deepseek-v4"]
    .map((id) => ({ agent: "opencode" as AgentKind, id, label: id.slice(id.indexOf("/") + 1), common: common.includes(id) }));
  return [...(mockInstalled.has("claude_code") ? claude : []), ...oc];
};

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
      ranked_by: score === base ? "rules" : "rules+claude-haiku-5-5", fired: [`kind ${kind} = ${base}`] },
  };
});

// A planning chat that split its work into phases: they fold under it, each
// waiting on the one before.
{
  const task = (id: string, title: string, body: string, score: number, min: number, extra: Partial<QueueItem> = {}): QueueItem => ({
    id, source: "tab", external_id: id, kind: "tab.task", title, body, url: null, actor: extra.parent_id ? "agent" : "you", context: "tab",
    occurred_at: ago(min), blocks: [], entities: [], metadata: { created_by: extra.parent_id ? "agent" : "user" },
    state: "open", snoozed_until: null, created_at: ago(min), updated_at: ago(min),
    ranking: { score, base_score: score, ai_adjustment: null, urgency: urgency(score), reason: "A task", ranked_by: "rules", fired: [] },
    ...extra,
  });
  items.push(
    task("plan-hub", "Prototype project hub with resources and coordinator", "Design the hub, then build it in phases.", 52, 30),
    task("plan-p2", "Projects phase 2: index project resources for search", "Builds on the prototype.", 52, 4, { parent_id: "plan-hub" }),
    task("plan-p3", "Projects phase 3: coordinator monitoring and auto-compaction", "Builds on phase 2.", 47, 3, { parent_id: "plan-hub", blocked_by: ["plan-p2"] }),
    task("plan-p4", "Projects phase 4: end-to-end encrypted vault", "Builds on phase 3.", 52, 2, { parent_id: "plan-hub", blocked_by: ["plan-p3"] }),
  );
}

const base = { rate_limits: [], category: "built_in", features: [] as string[], builtin: false };
const plainConnectors: ConnectorInfo[] = [
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
  { ...base, id: "email", name: "Email (IMAP)", icon: "mail", category: "communication", description: "Mail from MXroute, Fastmail, iCloud or any IMAP host. Spam and newsletters are sorted with rules, not AI.", available: true, enabled: false, status: { state: "disabled" },
    kinds: [{ kind: "email.direct", label: "Email", icon: "mail", tone: "info" }],
    settings: [
      { key: "host", label: "IMAP server", kind: "text", help: "For MXroute, the server name from your welcome email, e.g. heracles.mxrouting.net.", required: true },
      { key: "username", label: "Email address", kind: "text", help: null, required: true },
      { key: "password", label: "Password", kind: "secret", help: "Your mailbox password, or an app password if your host uses them. Not needed when you sign in with Google or Microsoft.", required: true },
      { key: "port", label: "Port", kind: "number", help: "993 (IMAP over TLS) unless your host says otherwise.", required: false },
    ] },
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
    budget: { daily_usd: 10, monthly_usd: null, per_turn_usd: null, ranker_plan_cap: 0.8 }, kind: "claude_code", command: null, model: null, workdir: "C:\\Users\\vince\\code", permission_mode: "auto", opencode_url: null, strict: true },
  mcp: { port: 47821, token: "mock-token" },
  storage: { keep_days: 7, max_gb: 5 },
  sandbox: {
    enabled: true, protected: [], hidden: [], container_runtime: "", ssh_in_chats: true,
    confine: true, folders: ["~/tmp", "~/src", "~/code", "~/dev", "~/Developer", "~/projects", "~/repos", "~/git", "~/work", "~/workspace"],
    lsp_domains: ["crates.io", "*.crates.io", "proxy.golang.org", "sum.golang.org", "pypi.org", "files.pythonhosted.org", "registry.npmjs.org", "github.com", "*.githubusercontent.com"],
  },
  hub: { mode: "local" },
  onboarded: true,
};
// `?account=1` starts the browser mock signed in.
let mockAccount: Account | null = typeof location !== "undefined" && new URLSearchParams(location.search).has("account")
  ? { user_id: "mock-google", email: "you@gmail.com", name: "You", avatar_url: null, provider: "google" }
  : null;

let mockHub: HubStatus | null = null;
let mockHubFailed = false;
// `?hub=cloud` starts the browser mock as if this computer moved to tab cloud.
let mockMoved = typeof location !== "undefined" && /^cloud/.test(new URLSearchParams(location.search).get("hub") ?? "");
/** `?hub=cloud-offline`: moved, but tab cloud can't be reached. */
const mockOffline = typeof location !== "undefined" && new URLSearchParams(location.search).get("hub") === "cloud-offline";
let mockSecrets: string[] = ["harness/claude_code/ANTHROPIC_API_KEY"];
const mockPlan = () => create(PlanSchema, { name: "trial", endsAt: timestampFromDate(new Date(Date.now() + 14 * 86400_000)), active: true });
const mockCloudHub = (state: HubState): HubStatus =>
  create(HubStatusSchema, {
    hub: {
      hubId: state === HubState.NONE ? "" : "5e7ecedf5a07a215de6ad0cc9d6ca7080b0e32c07931566c24ac88cdf129feee",
      kind: HubKind.PERSONAL, role: Role.OWNER, state, region: state === HubState.NONE ? "" : "wnam",
      readyAt: state === HubState.READY ? timestampNow() : undefined,
    },
    plan: mockPlan(),
  });
if (mockMoved) mockHub = mockCloudHub(HubState.READY);

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

const connectors: ConnectorInfo[] = withActions(plainConnectors);

const mockSlashCommands = [
  { name: "code-review", description: "Review the current diff, or a PR number/branch/path target, for correctness bugs", argument_hint: "[low|medium|high|xhigh|max|ultra] [--fix] [--comment] [<pr#>|<branch>|<path>]", aliases: ["review"], kind: "builtin" },
  { name: "compact", description: "Free up context by summarizing the conversation so far", argument_hint: "<optional custom summarization instructions>", aliases: [], kind: "builtin" },
  { name: "context", description: "Show current context usage", argument_hint: "", aliases: [], kind: "builtin" },
  { name: "init", description: "Initialize a new CLAUDE.md file with codebase documentation", argument_hint: "", aliases: [], kind: "builtin" },
  { name: "effort", description: "Set effort level for model usage", argument_hint: "<low|medium|high|xhigh|max|auto>", aliases: [], kind: "builtin" },
  { name: "security-review", description: "Complete a security review of the pending changes on the current branch", argument_hint: "", aliases: [], kind: "builtin" },
  { name: "deploy", description: "Deploy a branch to staging", argument_hint: "<branch> [staging|production]", aliases: [], kind: "command" },
  { name: "frontend-design", description: "Build distinctive, production-grade frontend interfaces", argument_hint: "", aliases: [], kind: "skill" },
  { name: "pdf", description: "Read, fill and merge PDF files", argument_hint: "[<path>]", aliases: [], kind: "skill" },
  { name: "mcp__docs__search", description: "Search the team docs (MCP)", argument_hint: "<query>", aliases: [], kind: "mcp" },
];

export const mockApi = {
  async invoke<T>(cmd: string, args: Record<string, any> = {}): Promise<T> {
    const r = await (async () => {
      const wf = workflowMock(cmd, args, { items: () => items, connectors: () => connectors, emit });
      if (wf !== NOT_HANDLED) return wf;
      const mcp = mcpHandle(cmd, args);
      if (mcp !== NOT_HANDLED) return mcp;
      switch (cmd) {
        case "agent_mcp_servers": return [{ name: "tab", status: "connected" }, ...mcpServersInfo().map((m) => ({ name: m.id, status: m.ready ? "connected" : "needs-auth" })), { name: "playwright", status: "failed" }];
        case "agent_slash_commands": return args.agent === "claude_code" || !args.agent ? mockSlashCommands : args.agent === "opencode" ? [{ name: "init", description: "create/update AGENTS.md", argument_hint: "[arguments]", aliases: [], kind: "builtin" }, { name: "review", description: "review changes [commit|branch|pr], defaults to uncommitted", argument_hint: "[arguments]", aliases: [], kind: "builtin" }] : [];
        case "agent_subagents": return [{ name: "Explore", description: "Read-only search agent" }, { name: "Plan", description: "Designs implementation plans" }];
        case "slash_arg_values": return args.kind === "branch" ? ["main", "slash-menu-rich", "fix-hub-sync"] : ["src/", "crates/", "docs/", "README.md", "package.json"].map((p) => `${args.typed}${p}`);
        case "agent_activity": return sessions.filter((s) => s.item_id && items.find((i) => i.id === s.item_id)?.state !== "done").map((s) => ({ item_id: s.item_id!, agent: s.agent, status: s.status, updated_at: s.updated_at }));
        case "graph_view": return mockGraph(args.projectId ?? null, args.days ?? null);
        case "graph_delete": {
          const g = mockGraph(null, null);
          const hit = g.nodes.find((n) => n.id === args.id);
          if (!hit || hit.kind === "project") return 0;
          const mine = new Set(items.filter((i) => i.entities.some((x) => x.key === hit.key)).map((i) => i.id));
          const gone = [hit, ...(args.withExclusive ? exclusiveOf(hit, mine) : [])];
          gone.forEach((n) => removedNodes.add(n.key));
          return gone.length;
        }
        case "graph_node": {
          const g = mockGraph(null, null);
          const entity = g.nodes.find((n) => n.id === args.id)!;
          const near = g.edges.filter((e) => e.src === args.id || e.dst === args.id).sort((a, b) => b.weight - a.weight);
          const connected = near.map((e) => ({ entity: g.nodes.find((n) => n.id === (e.src === args.id ? e.dst : e.src))!, shared: e.weight }));
          const hits = items.filter((i) => (entity.kind === "project" ? `project:${i.project_id}` === entity.key : i.entities.some((x) => x.key === entity.key)));
          return { entity, exclusive: entity.kind === "project" ? [] : exclusiveOf(entity, new Set(hits.map((i) => i.id))), connected: connected.slice(0, 12), items: hits.slice(0, 12), notes: notes.filter((n) => `project:${n.project_id}` === entity.key) };
        }
        case "coordinator_chat_open": {
          const session = sessions.find((s) => s.cwd === `coordinator:${args.projectId}`) ?? null;
          return { session, messages: session ? messages.filter((m) => m.session_id === session.id) : [], approvals: [], running: false } satisfies Chat;
        }
        case "coordinator_chat_send": {
          const key = `coordinator:${args.projectId}`;
          const project = projects.find((p) => p.id === args.projectId);
          if (!project) throw new Error("That project was deleted.");
          let session = sessions.find((s) => s.cwd === key);
          if (!session) {
            session = { id: `sess-${Date.now()}`, item_id: null, agent: config.agent.kind ?? "claude_code", agent_session_id: null, title: `Coordinator · ${project.name}`, status: "working", cwd: key, created_at: new Date().toISOString(), updated_at: new Date().toISOString() };
            sessions.push(session);
          }
          simulateCoordinatorTurn(session, project, String(args.text), args.attachments ?? []);
          return session;
        }
        case "project_resources": return resources.filter((r) => r.project_id === args.projectId).sort((a, b) => Number(b.pinned) - Number(a.pinned) || a.kind.localeCompare(b.kind) || a.title.localeCompare(b.title));
        case "project_resource_save": {
          const now = new Date().toISOString();
          const r = args.resource as ResourceInput;
          if (r.access === "vault") throw new Error("the vault isn't available yet: it needs end-to-end encryption, which comes in a later release");
          if (!r.title.trim()) throw new Error("a resource needs a title");
          if (!r.url && r.kind !== "note") throw new Error(`a ${r.kind} needs a link`);
          const old = resources.find((x) => x.project_id === args.projectId && (r.id ? x.id === r.id : x.kind === r.kind && x.url === r.url));
          const fields = { kind: r.kind, title: r.title.trim(), url: r.url, note: r.note ?? "", access: r.access ?? "all", options: r.options ?? old?.options ?? {}, pinned: r.pinned ?? old?.pinned ?? false, updated_at: now };
          if (old) return Object.assign(old, fields);
          const fresh: ProjectResource = { id: `res-${Date.now()}`, project_id: args.projectId, status: r.kind === "note" ? "indexed" : r.kind === "service" ? "live" : "pending", chunks: 0, added_by: "user", created_at: now, ...fields };
          resources.push(fresh);
          return fresh;
        }
        case "project_file_add": {
          const now = new Date().toISOString();
          const name = String(args.name);
          if (!/\.(pdf|docx|xlsx|md|mdx|markdown|txt|text|csv|tsv|json|html?|rst|org|ya?ml|toml|xml|log)$/i.test(name)) throw new Error(`tab can't read ${name} yet: add PDF, Word (.docx), Excel (.xlsx), markdown or text files`);
          const fresh: ProjectResource = { id: `res-${Date.now()}`, project_id: args.projectId, kind: "file", title: name, url: `sha256:${Date.now().toString(16).padStart(64, "0")}`, note: "", access: args.access ?? "all", options: { name, size: (args.bytes as Uint8Array).length }, status: "pending", chunks: 0, pinned: false, added_by: "user", created_at: now, updated_at: now };
          resources.push(fresh);
          // Indexing happens in the background; pretend it finished.
          setTimeout(() => Object.assign(fresh, { status: "indexed", chunks: Math.max(1, Math.round(fresh.options.size as number / 1200)), indexed_at: new Date().toISOString() }), 1500);
          return fresh;
        }
        case "project_resource_reindex": return null;
        case "project_resource_delete": {
          const n = resources.length;
          resources = resources.filter((r) => !(r.project_id === args.projectId && r.id === args.id));
          return resources.length < n;
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
          const old = projects.find((x) => x.id === args.id);
          const p: Project = { id: args.id ?? `p-${Date.now()}`, name: String(args.name).trim(), color: args.color, icon: args.icon, about: old?.about ?? "", rules: old?.rules ?? "", coordinator_watch: old?.coordinator_watch ?? true, coordinator_brief: old?.coordinator_brief ?? "08:00", compact_at: old?.compact_at ?? 200000, created_at: old?.created_at ?? new Date().toISOString() };
          if (!p.name) throw new Error("a project needs a name");
          projects = [...projects.filter((x) => x.id !== p.id), p];
          return p;
        }
        case "project_delete": {
          projects = projects.filter((x) => x.id !== args.id);
          notes = notes.filter((n) => n.project_id !== args.id);
          repos = repos.filter((r) => r.project_id !== args.id);
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
        case "rename_item": {
          const i = items.find((x) => x.id === args.id);
          if (!i) return null;
          i.title = args.title;
          i.metadata = { ...(i.metadata as object), title_source: "user" };
          emit({ type: "item_changed", change: "updated", item: { ...i } });
          return i;
        }
        case "titler_default_rules": return "Name the thread the way a good engineer names a ticket: 3 to 7 words, at most 60 characters.";
        case "coordinator_settings_save": {
          const p = projects.find((x) => x.id === args.projectId);
          if (!p) throw new Error("That project was deleted.");
          if (args.watch != null) p.coordinator_watch = !!args.watch;
          if (args.brief != null) p.coordinator_brief = String(args.brief).trim();
          if (args.compactAt != null) p.compact_at = Math.max(0, Number(args.compactAt));
          return { ...p };
        }
        case "coordinator_history": {
          const session = sessions.find((s) => s.cwd === `coordinator:${args.projectId}`);
          return session ? segments.filter((g) => g.session_id === session.id).sort((a, b) => b.ended_at.localeCompare(a.ended_at)) : [];
        }
        case "coordinator_segment": return segments.find((g) => g.id === args.segmentId) ?? null;
        case "coordinator_segment_messages": {
          const g = segments.find((x) => x.id === args.segmentId);
          if (!g) return [];
          const all = messages.filter((m) => m.session_id === g.session_id);
          const end = all.findIndex((m) => m.kind === "compacted" && m.meta?.segment_id === g.id);
          const start = all.slice(0, end).map((m) => m.kind).lastIndexOf("compacted");
          return all.slice(start + 1, end);
        }
        case "coordinator_summary_save": {
          const g = segments.find((x) => x.id === args.segmentId);
          if (!g) throw new Error("That summary is gone.");
          g.summary = String(args.summary).trim();
          g.updated_at = new Date().toISOString();
          return { ...g };
        }
        case "coordinator_compact": {
          const session = sessions.find((s) => s.cwd === `coordinator:${args.projectId}`);
          if (!session) throw new Error("The coordinator hasn't started yet.");
          const all = messages.filter((m) => m.session_id === session.id);
          const from = all.map((m) => m.kind).lastIndexOf("compacted") + 1;
          const stretch = all.slice(from);
          if (!stretch.some((m) => m.role === "assistant" && m.kind === "text")) throw new Error("there's nothing to compact yet");
          const used = [...stretch].reverse().find((m) => m.kind === "result")?.meta?.context?.used ?? 0;
          const now = new Date().toISOString();
          const g: Segment = { id: `seg-${Date.now()}`, session_id: session.id, project_id: String(args.projectId), agent_session_id: session.agent_session_id, summary: "**Open:** Refund for Linear Ltd (waiting on you) · checkout deploy failing since the env rename\n**Decisions:** refunds over $100 need you\n**Watching:** 3 threads; trial cohort ends Nov 15", tokens: used, messages: stretch.length, reason: "manual", started_at: stretch[0]?.created_at ?? now, ended_at: now, updated_at: now };
          segments.push(g);
          session.agent_session_id = null;
          post(session, "system", "compacted", "", { segment_id: g.id, tokens: used, messages: g.messages, reason: "manual", started_at: g.started_at });
          return g;
        }
        case "project_set_profile": {
          const p = projects.find((x) => x.id === args.id);
          if (!p) return null;
          if (args.about != null) p.about = String(args.about).trim();
          if (args.rules != null) p.rules = String(args.rules).trim();
          return { ...p };
        }
        case "project_repos": return repos.filter((r) => r.project_id === args.projectId).sort((a, b) => a.repo.localeCompare(b.repo));
        case "project_repo_save": {
          const now = new Date().toISOString();
          const name = String(args.repo).trim().replace(/\.git$/, "").replace(/^(https?:\/\/)?github\.com\/|^git@github\.com:/, "");
          if (!name) throw new Error("a repo needs a name or URL");
          const old = repos.find((r) => r.project_id === args.projectId && r.repo.toLowerCase() === name.toLowerCase());
          if (old) return Object.assign(old, { path: args.path ?? old.path, note: args.note ?? old.note, updated_at: now });
          const r: ProjectRepo = { id: `r-${Date.now()}`, project_id: args.projectId, repo: name, path: args.path ?? null, note: args.note ?? "", added_by: "user", created_at: now, updated_at: now };
          repos = [...repos, r];
          return r;
        }
        case "project_repo_delete": repos = repos.filter((r) => !(r.project_id === args.projectId && (r.id === args.id || r.repo === args.id))); return null;
        case "project_items": return items.filter((i) => i.project_id === args.projectId).sort((a, b) => Number(a.state === "done") - Number(b.state === "done") || b.occurred_at.localeCompare(a.occurred_at)).slice(0, 50);
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
          if (args.what === "pull" && showcasePulls[`${args.repo}#${args.id}`]) return showcasePulls[`${args.repo}#${args.id}`];
          return args.what === "pull"
            ? { title: "Retry webhook deliveries with exponential backoff", state: "open", author: "dana-k", checks: "passing", updated_at: new Date(Date.now() - 3_600_000).toISOString(), merge_state: "blocked", review_decision: "review_required", open_threads: 2 }
            : { title: "Sample " + args.what, state: "open", author: "sam", checks: null, updated_at: null };
        case "start_dictation": throw "Dictation needs the desktop app.";
        case "attachment_save": return `${URL.createObjectURL(new Blob([args.bytes]))}#${args.name}`;
        case "file_open": return null;
        case "file_stamps": return (args.paths as string[]).map(() => null);
        case "image_read": return await (await fetch(String(args.path))).arrayBuffer();
        case "file_read": return await (await fetch(String(args.path))).arrayBuffer();
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
        case "done_guards": return [];
        case "context_status": return Object.fromEntries((args.ids as string[]).flatMap((id) => (mockContextStatus[id] ? [[id, mockContextStatus[id]]] : [])));
        case "mark_done": return setState(args.id, "done");
        case "snooze": return setState(args.id, "snoozed", new Date(Date.now() + args.minutes * 60_000).toISOString());
        case "reopen": return setState(args.id, "open");
        case "list_connectors": return connectors;
        case "rate_limits": return limits;
        case "audit_log": return audit;
        case "api_usage": return usage;
        case "get_config": return config;
        case "save_config": config = args.config; return config;
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
          return { session, messages: session ? messages.filter((m) => m.session_id === session.id) : [], approvals: [], running: session?.status === "working" } satisfies Chat;
        }
        case "chat_send": {
          const item = args.itemId ? items.find((i) => i.id === args.itemId)! : createTask(String(args.text).split("\n")[0].slice(0, 80), null, args.projectId ?? null);
          let session = sessions.find((s) => s.item_id === item.id);
          if (!session) {
            session = { id: `sess-${Date.now()}`, item_id: item.id, agent: config.agent.kind ?? "claude_code", agent_session_id: null, title: item.title, status: "working", cwd: "~/code", created_at: new Date().toISOString(), updated_at: new Date().toISOString() };
            sessions.push(session);
          }
          if (session.model == null) {
            const agent = config.agent;
            session.model = agent.models?.[session.agent] ?? (agent.kind === session.agent ? agent.model : null) ?? "";
          }
          simulateTurn(session, item, String(args.text), args.attachments ?? []);
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
          simulateReviewTurn(session, String(args.text), args.attachments ?? []);
          return session;
        }
        case "set_agent_mode": config = { ...config, agent: { ...config.agent, permission_mode: args.mode } }; return config;
        case "chat_stop": return null;
        case "chat_interrupt": return null;
        case "approval_resolve": return true;
        case "agent_health": return { kind: "claude_code", found: true, path: "/Users/you/.local/bin/claude", version: mockUpgraded ? "2.1.318 (Claude Code)" : "2.1.91 (Claude Code)", error: null, install: "native", upgrade: "claude update", latest: "2.1.318" };
        case "agent_upgrade": {
          const steps = ["Checking for updates…", "Downloading 2.1.318 (38%)", "Downloading 2.1.318 (100%)", "Installing…", "Updated to 2.1.318"];
          for (const s of steps) { await new Promise((r) => setTimeout(r, 700)); upgradeListeners.forEach((l) => l(s)); }
          mockUpgraded = true;
          return undefined;
        }
        case "app_version": return { version: mockAppUpdated ? "0.0.4" : "0.0.3", updates: true, reason: null };
        case "app_update_check": {
          await new Promise((r) => setTimeout(r, 600));
          return mockAppUpdated ? null : { version: "0.0.4", notes: "## What's changed\n- Settings has its own pages\n- tab updates itself", date: new Date().toISOString() };
        }
        case "app_update_install": {
          const total = 18_400_000;
          for (let d = 0; d <= total; d += 2_300_000) { await new Promise((r) => setTimeout(r, 250)); appUpdateListeners.forEach((l) => l({ downloaded: d, total })); }
          mockAppUpdated = true;
          return undefined;
        }
        case "detect_agents": return harnesses.map((h) => mockInstalled.has(h.id)
          ? { kind: h.id, found: true, path: `/usr/local/bin/${h.command}`, version: h.id === "claude_code" ? "2.1.318 (Claude Code)" : "1.4.2", error: null }
          : { kind: h.id, found: false, path: null, version: null, error: `\`${h.command}\` was not found on PATH.`, install_command: h.install.npm ? `npm install -g ${h.install.npm}` : h.install.unix });
        case "agent_harnesses": return harnesses;
        case "agent_models": return mockModels();
        case "agent_install": {
          const h = harnesses.find((x) => x.id === args.kind);
          const steps = [`npm install -g ${h?.install.npm ?? h?.command}`, "added 12 packages in 4s", `Installed ${h?.name} 1.4.2`];
          for (const s of steps) { await new Promise((r) => setTimeout(r, 600)); upgradeListeners.forEach((l) => l(s)); }
          mockInstalled.add(args.kind as AgentKind);
          return undefined;
        }
        case "set_session_model": {
          const session = sessions.find((s) => s.id === args.sessionId)!;
          session.model = args.model ?? "";
          emit({ type: "session_changed", session: { ...session } });
          return { ...session };
        }
        case "set_agent_model": {
          const agent: AgentKind = args.agent ?? config.agent.kind ?? "claude_code";
          const models = { ...(config.agent.models ?? {}) };
          if (args.model) models[agent] = args.model; else delete models[agent];
          const kind = args.forNewChats && args.agent ? args.agent : config.agent.kind;
          config = { ...config, agent: { ...config.agent, kind, models, model: kind ? models[kind] ?? null : null } };
          return config;
        }
        case "folder_problem": return null;
        case "mcp_info": return { url: "http://127.0.0.1:47821/mcp", token: "mock-token" };
        case "github_pull": return samplePull(args.repo, args.number);
        case "storage_usage": return {
          attachments: { bytes: 1_048_576, files: 15 },
          sessions: { bytes: 311_296, files: 64 },
          workspaces: { bytes: mockCheckouts.reduce((n, c) => n + c.bytes, 10_485_760), files: 900 },
          checkouts: mockCheckouts,
        };
        case "sandbox_status": return {
          enabled: true, backend: "unavailable", enforced: false, container: "docker",
          summary: "Windows has no sandbox tab can use yet, so file limits aren't enforced. Agents still start without tab's secrets, Claude Code is told not to touch protected folders, and stopping a chat ends everything it started.",
          fix: "Workflow commands that name a container image run isolated in Docker or Podman.",
        };
        case "storage_clean": return { removed: 3, bytes: 2_200_000 };
        case "account_status": return { configured: true, account: mockAccount };
        case "account_sign_in": {
          await new Promise((r) => setTimeout(r, 1200));
          mockAccount = args.provider === "apple"
            ? { user_id: "mock-apple", email: null, name: null, avatar_url: null, provider: "apple" }
            : { user_id: "mock-google", email: "you@gmail.com", name: "You", avatar_url: null, provider: "google" };
          return mockAccount;
        }
        case "account_cancel_sign_in": return null;
        case "account_sign_out": mockAccount = null; mockHub = null; return null;
        case "cloud_hub_status":
          if (!mockAccount) throw { code: ErrorCode.UNAUTHENTICATED, message: "Sign in to use tab cloud." };
          return toBinary(HubStatusSchema, mockHub ?? mockCloudHub(HubState.NONE));
        case "cloud_hubs": {
          if (!mockAccount) throw { code: ErrorCode.UNAUTHENTICATED, message: "Sign in to use tab cloud." };
          const personal = (mockHub ?? mockCloudHub(HubState.NONE)).hub!;
          const team = create(HubSchema, { hubId: "a1".repeat(32), kind: HubKind.TEAM, name: "Design team", role: Role.MEMBER, state: HubState.READY, region: "weur" });
          return toBinary(HubListSchema, create(HubListSchema, { hubs: [personal, team], plan: mockPlan() }));
        }
        case "cloud_hub_set_up": {
          if (!mockAccount) throw { code: ErrorCode.UNAUTHENTICATED, message: "Sign in to use tab cloud." };
          // `?hub=fail` fails the first attempt, `?hub=plan` acts as if the trial ended.
          const q = new URLSearchParams(location.search).get("hub");
          await new Promise((r) => setTimeout(r, 2500));
          if (q === "plan") throw { code: ErrorCode.PLAN_REQUIRED, message: "Your tab cloud trial has ended. Choose a plan at tabqueue.com to keep using it." };
          if (q === "fail" && !mockHubFailed) {
            mockHubFailed = true;
            mockHub = mockCloudHub(HubState.FAILED);
            mockHub.hub!.error = "tab couldn't finish setting up your hub. Try again in a moment.";
          } else {
            mockHub = mockCloudHub(HubState.READY);
          }
          return toBinary(HubStatusSchema, mockHub);
        }
        case "hub_status": {
          const started = new Date(Date.now() - 3 * 3600_000).toISOString();
          return {
            runs_on: "local", protocol_version: 1, started_at: started,
            url: "wss://127.0.0.1:47850/hub", fingerprint: "sha256/3f1c9a0e5d7b2c48e6a1f0b9d3c7e2a5b8f4d1c6e9a2b7f0c3d8e5a1b6f9c2d4", http3: true, path: "C:\\Users\\vince\\AppData\\Roaming\\dev.tab.app\\hub.db",
            head: 18_422, records: 6_310, edges: 0, live_leases: 1, storage_bytes: 41_943_040, files: 38, file_bytes: 12_582_912,
            cpu_percent: 2.4 + Math.random() * 3, memory_bytes: 186_000_000 + Math.round(Math.random() * 4_000_000), cpus: 12, pending: 0,
            cloud_hub: mockMoved ? "5e7ecedf5a07a215de6ad0cc9d6ca7080b0e32c07931566c24ac88cdf129feee" : null, ...(mockOffline && mockMoved
              ? { cloud_connected: false, cloud_error: "HTTP error: 503 Service Unavailable", pending: 14 }
              : { cloud_connected: mockMoved, cloud_error: null }),
            pulled_to: 18_422,
            synced: [{ what: "Queue items", count: 5_102 }, { what: "Chats", count: 812 }, { what: "Projects", count: 9 }, { what: "Notes", count: 41 }],
            connections: [
              { id: 0, device: "this-computer", client: "tab-desktop/0.1.0", address: "this app", connected_at: started, synced_to: 18_422, behind: 0, this_app: true },
              { id: 3, device: "pixel-8", client: "tab-mobile/0.1.0", address: "127.0.0.1:53122", connected_at: new Date(Date.now() - 600_000).toISOString(), synced_to: 18_410, behind: 12, this_app: false },
            ],
          };
        }
        case "hub_move_to_local": mockMoved = false; return 6_310;
        case "hub_move_to_cloud": {
          for (let done = 0; done <= 18_422; done += 6_000) await new Promise((r) => setTimeout(r, 300));
          mockMoved = true;
          return { kind: "moved", changes: 18_422 };
        }
        case "runners": {
          const now = new Date().toISOString();
          return [
            { id: "this-computer", name: "This MacBook", kind: "computer", harnesses: ["claude_code", "codex"], repos: ["acme/api"], connections: ["github"], seen_at: now, awake: true, this_app: true },
            { id: "desk-pc", name: "Desk PC", kind: "computer", harnesses: ["claude_code"], repos: [], connections: [], seen_at: new Date(Date.now() - 3 * 3600_000).toISOString(), awake: false, this_app: false },
            ...(mockMoved ? [{ id: "cloud", name: "tab cloud", kind: "cloud", harnesses: ["claude_code", "opencode", "codex", "pi"], repos: [], connections: ["linear"], seen_at: now, awake: true, this_app: false }] : []),
          ];
        }
        case "jobs_recent": return [];
        case "cloud_runner_status":
        case "cloud_runner_wake":
          if (!mockMoved) throw { code: ErrorCode.NOT_FOUND, message: "This computer's work isn't in tab cloud yet." };
          return toBinary(RunnerStatusSchema, create(RunnerStatusSchema, { state: RunnerState.RUNNING, seenAt: timestampNow(), detail: "Polling 1 connection." }));
        case "cloud_secret_names": return mockSecrets;
        case "cloud_secret_set":
          mockSecrets = mockSecrets.filter((n) => n !== args.name).concat(args.value ? [args.name as string] : []);
          return mockSecrets;
        case "connection_runs_on": {
          if (args.runsOn === "cloud" && !mockMoved) throw { code: ErrorCode.BAD_REQUEST, message: "Move your hub to tab cloud first (Settings → Hub), then a connection can run there." };
          const c = config.connectors[args.connection as string];
          if (c) config = { ...config, connectors: { ...config.connectors, [args.connection as string]: { ...c, runs_on: args.runsOn as "cloud" | "this_computer" } } };
          return config;
        }
        case "storage_remove_checkouts": {
          const gone = mockCheckouts.filter((c) => !args.repo || (c.repo === args.repo && c.number === args.number));
          mockCheckouts = mockCheckouts.filter((c) => !gone.includes(c));
          return { removed: gone.length, bytes: gone.reduce((n, c) => n + c.bytes, 0) };
        }
        case "lsp_servers": return lspServers;
        case "lsp_install": {
          await new Promise((r) => setTimeout(r, 1800));
          lspServers = lspServers.map((s) => (s.id === args.id ? { ...s, installed: true, path: `/Users/you/go/bin/${s.command}` } : s));
          return lspServers.find((s) => s.id === args.id);
        }
        case "lsp_start": throw new Error("Language servers run in the desktop app.");
        case "deps_prepare": return { state: "none", lockfile: null, manager: null, key: null, reused: false, message: "Dependencies install in the desktop app." };
        case "deps_types": return [];
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
  onAppUpdate(cb: (p: { downloaded: number; total: number | null }) => void) {
    appUpdateListeners.add(cb);
    return () => void appUpdateListeners.delete(cb);
  },
  onAgentUpgrade(cb: (line: string) => void) {
    upgradeListeners.add(cb);
    return () => void upgradeListeners.delete(cb);
  },
  onGhCode(cb: (c: GhLoginCode) => void) {
    ghCodeListeners.add(cb);
    return () => void ghCodeListeners.delete(cb);
  },
  onMcpSignIn(cb: (e: McpSignInEvent) => void) {
    mcpSignInListeners.add(cb);
    return () => void mcpSignInListeners.delete(cb);
  },
};

let mockCheckouts = [
  { repo: "acme/web", number: 412, last_used: new Date(Date.now() - 3600_000).toISOString(), bytes: 2_500_000 },
  { repo: "acme/api", number: 88, last_used: new Date(Date.now() - 9 * 86400_000).toISOString(), bytes: 1_700_000_000 },
];

let lspServers: LspServer[] = [
  { id: "vtsls", name: "TypeScript (vtsls)", languages: ["typescript", "javascript"], command: "vtsls", note: "Full types from the project's dependencies, installed once per lockfile and shared between pull requests.", installed: false, path: null, install_command: "npm install --ignore-scripts @vtsls/language-server@0.3.0", needs: null, needs_url: null, managed: "@vtsls/language-server@0.3.0", deps: "js" },
  { id: "gopls", name: "Go (gopls)", languages: ["go"], command: "gopls", note: "Hover, go to definition, references and errors across the module.", installed: false, path: null, install_command: "go install golang.org/x/tools/gopls@latest", needs: null, needs_url: null, managed: null, deps: null },
  { id: "sqls", name: "SQL (sqls)", languages: ["sql"], command: "sqls", note: "Syntax errors and outline. Table and column info needs a database connection in sqls's own config.", installed: false, path: null, install_command: "go install github.com/sqls-server/sqls@latest", needs: null, needs_url: null, managed: null, deps: null },
  { id: "rust-analyzer", name: "Rust (rust-analyzer)", languages: ["rust"], command: "rust-analyzer", note: "Hover, go to definition and references. Indexing a large workspace takes a minute.", installed: true, path: "/Users/you/.cargo/bin/rust-analyzer", install_command: "rustup component add rust-analyzer", needs: null, needs_url: null, managed: null, deps: null },
  { id: "pyright", name: "Python (pyright)", languages: ["python"], command: "pyright-langserver", note: "Hover, go to definition, references and type errors.", installed: false, path: null, install_command: null, needs: "npm", needs_url: "https://nodejs.org", managed: null, deps: null },
];

let gh: GhStatus = { installed: false, path: null, version: null, logged_in: false, user: null, install_command: "brew install gh", error: null };
const ghCodeListeners = new Set<(c: GhLoginCode) => void>();

// Remote MCP connections. Sign-ins finish on their own after a moment.
const mcpCatalog: McpCatalogEntry[] = [
  { id: "linear", name: "Linear", url: "https://mcp.linear.app/mcp", description: "Agents can find, create and update issues, projects and comments.", category: "work", color: "#5e6ad2", fills_queue: true, kinds: [{ kind: "linear.issue", label: "Linear issue", icon: "linear", tone: "accent" }] },
  { id: "notion", name: "Notion", url: "https://mcp.notion.com/mcp", description: "Agents can search, read and edit pages and databases.", category: "work", color: "#191919", fills_queue: true, queue_query: "tasks assigned to me", kinds: [{ kind: "notion.page", label: "Notion page", icon: "notion", tone: "neutral" }, { kind: "notion.database", label: "Notion database", icon: "notion", tone: "neutral" }] },
  { id: "atlassian", name: "Jira & Confluence", url: "https://mcp.atlassian.com/v1/mcp", description: "Agents can search, comment on and move tickets, and read pages.", category: "work", color: "#1868db", fills_queue: false, kinds: [] },
  { id: "sentry", name: "Sentry", url: "https://mcp.sentry.dev/mcp", description: "Agents can look up issues, stack traces and events.", category: "code", color: "#362d59", fills_queue: true, kinds: [{ kind: "sentry.issue", label: "Sentry issue", icon: "alert", tone: "danger" }] },
  { id: "vercel", name: "Vercel", url: "https://mcp.vercel.com", description: "Agents can read deployments, build logs and project settings.", category: "code", color: "#000000", fills_queue: false, kinds: [] },
  { id: "stripe", name: "Stripe", url: "https://mcp.stripe.com", description: "Agents can look up customers, payments, invoices and disputes.", category: "work", color: "#635bff", fills_queue: false, kinds: [] },
];
const mcpSignedIn = new Map<string, string>();
/** Connections whose sign-in for tab cloud the hub keeps. */
const mcpCloudSignedIn = new Set<string>();
const mcpSignInListeners = new Set<(e: McpSignInEvent) => void>();
let mailSignedIn: EmailSignInStatus["signed_in"] = null;
const mockMcpTools: Omit<McpTool, "policy" | "chosen" | "cloud_policy" | "cloud_chosen">[] = [
  { name: "list_issues", title: "List issues", description: "Search and filter issues.", read_only: true },
  { name: "get_issue", title: "Get issue", description: "One issue with its comments.", read_only: true },
  { name: "list_projects", title: "List projects", description: "Projects in the workspace.", read_only: true },
  { name: "create_comment", title: "Comment", description: "Comment on an issue.", read_only: false },
  { name: "update_issue", title: "Update issue", description: "Change status, assignee or estimate.", read_only: false },
  { name: "create_issue", title: "Create issue", description: "Create an issue.", read_only: false },
];
function mcpServersInfo(): McpServerInfo[] {
  return Object.entries(config.mcp_servers ?? {}).map(([id, c]) => ({
    ...c,
    id,
    ready: c.enabled && (!!c.command || !c.sign_in || mcpSignedIn.has(id)),
    signed_in_at: mcpSignedIn.get(id) ?? null,
    error: null,
    tool_count: !c.sign_in || mcpSignedIn.has(id) ? mockMcpTools.length : null,
    cloud: mockImport.find((m) => m.key === c.imported_from)?.cloud ?? { portable: true, why_not: null, needs_key: false },
    keys: !!c.headers?.length || !!c.command?.env?.length,
    queue_sync: mcpCatalog.find((e) => e.id === c.catalog)?.fills_queue
      ? { on: c.queue !== false, last_sync: c.queue !== false && mcpSignedIn.has(id) ? mcpSignedIn.get(id)! : null, error: null, found: c.queue !== false && mcpSignedIn.has(id) ? 3 : null }
      : null,
  }));
}

/** What Claude Code has, in the mock. */
const here = { portable: false, why_not: null, needs_key: false };
const mockImport: (McpImportCandidate & { config: McpServerConfig })[] = [
  { key: "user:linear", name: "linear", source: "user", path: null, kind: "remote", url: "https://mcp.linear.app/mcp", command: null, header_names: [], env_names: [], cloud: { portable: true, why_not: null, needs_key: false }, catalog: "linear", status: "new", existing: null, id: "linear", note: null,
    config: { name: "Linear", url: "https://mcp.linear.app/mcp", enabled: true, sign_in: true, catalog: "linear", imported_from: "user:linear" } },
  { key: "user:github", name: "github", source: "user", path: null, kind: "remote_keys", url: "https://api.githubcopilot.com/mcp/", command: null, header_names: ["Authorization"], env_names: [], cloud: { portable: true, why_not: null, needs_key: true }, catalog: null, status: "new", existing: null, id: "github", note: null,
    config: { name: "github", url: "https://api.githubcopilot.com/mcp/", enabled: true, sign_in: false, headers: ["Authorization"], imported_from: "user:github" } },
  { key: "user:playwright", name: "playwright", source: "user", path: null, kind: "local", url: null, command: "npx -y @playwright/mcp@latest", header_names: [], env_names: [], cloud: { portable: true, why_not: null, needs_key: false }, catalog: null, status: "new", existing: null, id: "playwright", note: null,
    config: { name: "playwright", url: "", enabled: true, sign_in: false, command: { command: "npx", args: ["-y", "@playwright/mcp@latest"] }, imported_from: "user:playwright" } },
  { key: "project:/users/vincent/code/shop:postgres", name: "postgres", source: "project", path: "/Users/vincent/code/shop", kind: "local", url: null, command: "uvx postgres-mcp --access-mode=restricted", header_names: [], env_names: ["DATABASE_URI"], cloud: { ...here, why_not: "talks to this computer or your network", needs_key: true }, catalog: null, status: "new", existing: null, id: "postgres", note: null,
    config: { name: "postgres", url: "", enabled: true, sign_in: false, command: { command: "uvx", args: ["postgres-mcp", "--access-mode=restricted"], env: ["DATABASE_URI"], cwd: "/Users/vincent/code/shop" }, imported_from: "project:/users/vincent/code/shop:postgres" } },
  { key: "repo:/users/vincent/code/shop:docs", name: "docs", source: "repo", path: "/Users/vincent/code/shop", kind: "local", url: null, command: "npx -y @acme/docs-mcp", header_names: [], env_names: ["DOCS_TOKEN"], cloud: { portable: true, why_not: null, needs_key: true }, catalog: null, status: "new", existing: null, id: "docs", note: "DOCS_TOKEN isn't set for tab, so it'll be empty.",
    config: { name: "docs", url: "", enabled: true, sign_in: false, command: { command: "npx", args: ["-y", "@acme/docs-mcp"], env: ["DOCS_TOKEN"], cwd: "/Users/vincent/code/shop" }, imported_from: "repo:/users/vincent/code/shop:docs" } },
  { key: "user:obsidian", name: "obsidian", source: "user", path: null, kind: "local", url: null, command: "/Users/vincent/bin/obsidian-mcp ~/Notes", header_names: [], env_names: [], cloud: { ...here, why_not: "runs a program from this computer" }, catalog: null, status: "new", existing: null, id: "obsidian", note: null,
    config: { name: "obsidian", url: "", enabled: true, sign_in: false, command: { command: "/Users/vincent/bin/obsidian-mcp", args: ["~/Notes"] }, imported_from: "user:obsidian" } },
  { key: "claude_ai:Notion", name: "Notion", source: "claude_ai", path: null, kind: "claude_ai", url: "https://mcp.notion.com/mcp", command: null, header_names: [], env_names: [], cloud: { portable: true, why_not: null, needs_key: false }, catalog: "notion", status: "new", existing: null, id: "notion", note: null,
    config: { name: "Notion", url: "https://mcp.notion.com/mcp", enabled: true, sign_in: true, catalog: "notion", imported_from: "claude_ai:Notion" } },
  { key: "user:old-sse", name: "old-sse", source: "user", path: null, kind: "remote", url: "https://example.com/sse", command: null, header_names: [], env_names: [], cloud: here, catalog: null, status: "unsupported", existing: null, id: "old-sse", note: "It uses the older SSE transport, which tab doesn't speak yet.",
    config: { name: "", url: "", enabled: false, sign_in: false } },
  { key: "claude_ai:Health", name: "Health", source: "claude_ai", path: null, kind: "claude_ai", url: null, command: null, header_names: [], env_names: [], cloud: here, catalog: null, status: "unsupported", existing: null, id: "health", note: "tab can't read claude.ai connectors' settings, and doesn't know this one. It keeps working in chats that aren't strict.",
    config: { name: "", url: "", enabled: false, sign_in: false } },
];
function mcpImportScan(): McpImportCandidate[] {
  const servers = config.mcp_servers ?? {};
  return mockImport.map(({ config: c, ...m }) => {
    if (m.status === "unsupported") return m;
    const existing = Object.entries(servers).find(([, s]) => s.imported_from === m.key || (!!c.url && s.url === c.url) || (!!m.catalog && s.catalog === m.catalog))?.[0] ?? null;
    return existing ? { ...m, status: "imported", existing, id: existing } : m;
  });
}
function mcpSetServers(next: Record<string, McpServerConfig>) {
  config = { ...config, mcp_servers: next };
  emit({ type: "config_changed" });
}
function mcpFinishSoon(id: string, cloud = false) {
  setTimeout(() => {
    if (cloud) mcpCloudSignedIn.add(id);
    else mcpSignedIn.set(id, new Date().toISOString());
    mcpSignInListeners.forEach((l) => l({ id, ok: true, error: null, cloud }));
    emit({ type: "config_changed" });
  }, 1800);
}
function mcpHandle(cmd: string, args: Record<string, any>): unknown {
  const servers = config.mcp_servers ?? {};
  switch (cmd) {
    case "mcp_catalog": return mcpCatalog;
    case "mcp_servers": return mcpServersInfo();
    case "mcp_probe": {
      let u: URL;
      try { u = new URL(String(args.url).trim()); } catch { throw "That isn't a URL. It should look like https://example.com/mcp"; }
      if (u.protocol !== "https:" && u.hostname !== "localhost") throw "MCP servers need an https:// address";
      const host = u.hostname.replace(/^mcp\./, "").split(".")[0];
      let id = host.toLowerCase().replace(/[^a-z0-9-]/g, "-") || "mcp";
      while (servers[id]) id = id + "-2";
      const probe: McpProbe = { url: u.toString(), name: host.charAt(0).toUpperCase() + host.slice(1), sign_in: !u.hostname.includes("open"), id };
      return probe;
    }
    case "mcp_add": {
      if (servers[args.id]) throw "There's already a connection called " + args.id + ".";
      mcpSetServers({ ...servers, [args.id]: { name: args.name, url: args.url, enabled: true, sign_in: args.signIn, ...(args.catalog ? { catalog: args.catalog } : {}) } });
      if (!args.signIn) return null;
      mcpFinishSoon(args.id);
      return args.url + "#authorize";
    }
    case "mcp_sign_in": mcpFinishSoon(args.id); return (servers[args.id]?.url ?? "") + "#authorize";
    case "mcp_sign_out": mcpSignedIn.delete(args.id); emit({ type: "config_changed" }); return null;
    case "email_sign_in_status": {
      const status: EmailSignInStatus = {
        providers: [{ id: "google", name: "Google", available: true }, { id: "microsoft", name: "Microsoft", available: true }],
        signed_in: mailSignedIn,
      };
      return status;
    }
    case "email_sign_in": {
      const provider = args.provider as MailProvider;
      setTimeout(() => {
        const address = provider === "google" ? "you@gmail.com" : "you@outlook.com";
        mailSignedIn = { provider, address, signed_in_at: new Date().toISOString() };
        const host = provider === "google" ? "imap.gmail.com" : "outlook.office365.com";
        const prev = config.connectors.email?.settings ?? {};
        config = { ...config, connectors: { ...config.connectors, email: { enabled: true, settings: { ...prev, auth: provider, host, port: 993, username: address } } } };
        emit({ type: "config_changed" });
      }, 1800);
      return provider === "google" ? "https://accounts.google.com/#authorize" : "https://login.microsoftonline.com/#authorize";
    }
    case "email_sign_out": {
      mailSignedIn = null;
      const c = config.connectors.email;
      if (c?.settings.auth) {
        const { auth: _, ...settings } = c.settings;
        config = { ...config, connectors: { ...config.connectors, email: { ...c, enabled: false, settings } } };
      }
      emit({ type: "config_changed" });
      return null;
    }
    case "mcp_remove": {
      const rest = { ...servers };
      delete rest[args.id];
      mcpSignedIn.delete(args.id);
      mcpSetServers(rest);
      return null;
    }
    case "mcp_set_enabled": mcpSetServers({ ...servers, [args.id]: { ...servers[args.id], enabled: args.enabled } }); return null;
    case "mcp_set_queue": mcpSetServers({ ...servers, [args.id]: { ...servers[args.id], queue: args.on ? undefined : false, queue_query: args.query?.trim() || undefined } }); return null;
    case "mcp_tools": {
      const chosen = servers[args.id]?.tools ?? {};
      const cloud = servers[args.id]?.cloud_tools ?? {};
      return mockMcpTools.map((t) => ({
        ...t,
        policy: chosen[t.name] ?? (t.read_only ? "allow" : "ask"), chosen: t.name in chosen,
        cloud_policy: cloud[t.name] ?? (t.read_only ? "allow" : "off"), cloud_chosen: t.name in cloud,
      }));
    }
    case "mcp_cloud_states": {
      const fits = new Map(mcpServersInfo().map((s) => [s.id, s.cloud]));
      const states: McpCloudState[] = Object.entries(servers).map(([id, c]) => {
        const fit = fits.get(id);
        const allowed = !!c.cloud_jobs;
        if (!mockMoved) return { id, allowed, state: "no_hub", why: "Move your hub to tab cloud first (Settings → Hub)." };
        if (fit && !fit.portable) return { id, allowed, state: "cant_run", why: fit.why_not };
        if (!allowed) return { id, allowed, state: "off", why: null };
        return { id, allowed, state: !c.sign_in || mcpCloudSignedIn.has(id) ? "ready" : "sign_in", why: null };
      });
      return states;
    }
    case "mcp_set_cloud": {
      if (args.on && !mockMoved) throw "Move your hub to tab cloud first (Settings → Hub), then connections can be used in cloud jobs.";
      const s = servers[args.id];
      mcpSetServers({ ...servers, [args.id]: { ...s, cloud_jobs: args.on || undefined } });
      if (!args.on || !s.sign_in || mcpCloudSignedIn.has(args.id)) return null;
      mcpFinishSoon(args.id, true);
      return s.url + "#authorize-tab-cloud";
    }
    case "mcp_cloud_sign_in": mcpFinishSoon(args.id, true); return (servers[args.id]?.url ?? "") + "#authorize-tab-cloud";
    case "mcp_set_cloud_tool": {
      if (args.policy === "ask") throw "Cloud jobs can't ask: nobody is there to answer. Pick Allowed or Off.";
      const s = servers[args.id];
      const cloud_tools = { ...(s.cloud_tools ?? {}) };
      if (args.policy) cloud_tools[args.tool] = args.policy;
      else delete cloud_tools[args.tool];
      mcpSetServers({ ...servers, [args.id]: { ...s, cloud_tools } });
      return null;
    }
    case "cloud_job_gaps": {
      // One finished cloud job on the first item that needed Linear (and a write it doesn't have), and Notion.
      const gaps: JobGaps[] = [{
        job_id: "job-mock-1", run_id: "run-mock-1", workflow_id: "wf-triage", workflow_name: "Triage new Sentry issues", step_id: "fix",
        item_id: items[0]?.id ?? null, failed: false, finished_at: ago(25),
        missing: [
          servers.linear?.cloud_jobs
            ? { connection: "linear", name: "Linear", tool: "create_issue", why: "tool_off" }
            : { connection: "linear", name: "Linear", why: "not_in_cloud" },
          { connection: "obsidian", name: "obsidian", why: "only_on_computer" },
        ],
      }];
      return gaps;
    }
    case "mcp_import_scan": return mcpImportScan();
    case "mcp_import": {
      const found = mcpImportScan();
      const next = { ...servers };
      const results: McpImportResult[] = [];
      for (const key of args.keys as string[]) {
        const c = found.find((f) => f.key === key && (f.status === "new" || f.status === "changed"));
        const cfg = mockImport.find((m) => m.key === key)?.config;
        if (!c || !cfg) continue;
        next[c.id] = cfg;
        results.push({ key, id: c.id, name: cfg.name, sign_in: cfg.sign_in, error: null });
      }
      mcpSetServers(next);
      return results;
    }
    case "mcp_set_tool": {
      const s = servers[args.id];
      const tools = { ...(s.tools ?? {}) };
      if (args.policy) tools[args.tool] = args.policy;
      else delete tools[args.tool];
      mcpSetServers({ ...servers, [args.id]: { ...s, tools } });
      return null;
    }
  }
  return NOT_HANDLED;
}

const upgradeListeners = new Set<(line: string) => void>();
let mockUpgraded = false;
const appUpdateListeners = new Set<(p: { downloaded: number; total: number | null }) => void>();
let mockAppUpdated = false;

// A link an agent attached, to show links rendered by what they point at.
let contexts: ContextEntry[] = items.slice(0, 4).map((i) => ({ id: `ctx-seed-${i.id}`, item_id: i.id, section: "context", pinned: false, ref_item: null, url: "https://github.com/acme/payments/pull/491", title: "https://github.com/acme/payments/pull/491", summary: null, added_by: "agent", created_at: new Date().toISOString() }));
let projects: Project[] = [
  { id: "p-pay", name: "Payments", color: "#7c5cff", icon: "zap", about: "Checkout, the ledger and refunds: everything that moves money for Acme's customers.", rules: "- Every change to the ledger needs a review from Priya.\n- Feature-flag anything customer-facing.", coordinator_watch: true, coordinator_brief: "08:00", compact_at: 200000, created_at: ago(9000) },
  { id: "p-sec", name: "SOC 2", color: "#e5484d", icon: "shield", about: "", rules: "", coordinator_watch: true, coordinator_brief: "08:00", compact_at: 200000, created_at: ago(9000) },
];
let segments: Segment[] = [];
let repos: ProjectRepo[] = [
  { id: "r1", project_id: "p-pay", repo: "acme/payments", path: "~/src/payments", note: "Checkout API and ledger service", added_by: "agent", created_at: ago(3000), updated_at: ago(3000) },
  { id: "r2", project_id: "p-pay", repo: "acme/web", path: null, note: "Checkout UI", added_by: "user", created_at: ago(900), updated_at: ago(900) },
];
let resources: ProjectResource[] = [
  { id: "res1", project_id: "p-pay", kind: "website", title: "acme.com/pricing", url: "https://acme.com/pricing", note: "Public pricing and plans", access: "all", options: { depth: 1, schedule: "daily" }, status: "indexed", chunks: 42, pinned: false, added_by: "user", created_at: ago(2000), updated_at: ago(60), indexed_at: ago(45), last_used_at: ago(12) },
  { id: "res2", project_id: "p-pay", kind: "service", title: "Stripe dashboard", url: "https://dashboard.stripe.com", note: "Charges, refunds, disputes", access: "coordinator", options: {}, status: "live", chunks: 0, pinned: false, added_by: "user", created_at: ago(2000), updated_at: ago(2000) },
  { id: "res3", project_id: "p-pay", kind: "service", title: "Cloudflare · checkout-edge", url: "https://dash.cloudflare.com/acme/workers/services/view/checkout-edge", note: "Edge worker in front of checkout", access: "all", options: { watch_deploys: true }, status: "live", chunks: 0, pinned: false, added_by: "user", created_at: ago(1500), updated_at: ago(1500) },
  { id: "res4", project_id: "p-pay", kind: "email", title: "Billing inbox", url: "label:billing", note: "Customer billing questions", access: "coordinator", options: {}, status: "indexed", chunks: 118, pinned: false, added_by: "user", created_at: ago(1500), updated_at: ago(30), indexed_at: ago(20), last_used_at: ago(240) },
  { id: "res7", project_id: "p-pay", kind: "file", title: "Merchant agreement 2026.pdf", url: `sha256:${"ab".repeat(32)}`, note: "Signed terms with our processor", access: "coordinator", options: { name: "Merchant agreement 2026.pdf", size: 184_220 }, status: "indexed", chunks: 37, pinned: false, added_by: "user", created_at: ago(800), updated_at: ago(800), indexed_at: ago(799) },
  { id: "res8", project_id: "p-pay", kind: "website", title: "status.acme.com", url: "https://status.acme.com", note: "", access: "all", options: { depth: 0, schedule: "hourly" }, status: "failed", chunks: 0, pinned: false, added_by: "agent", created_at: ago(300), updated_at: ago(300), index_error: "status.acme.com didn't answer (timed out)" },
  { id: "res5", project_id: "p-pay", kind: "note", title: "Refund policy", url: null, note: "Full refunds within 30 days of a charge. Over $500 needs Priya.", access: "all", options: {}, status: "indexed", chunks: 1, pinned: true, added_by: "user", created_at: ago(900), updated_at: ago(900), indexed_at: ago(900), last_used_at: ago(3) },
  { id: "res6", project_id: "p-pay", kind: "link", title: "Runbook: checkout incidents", url: "https://docs.google.com/document/d/1abc", note: "", access: "all", options: {}, status: "needs_sign_in", chunks: 0, pinned: false, added_by: "agent", created_at: ago(400), updated_at: ago(400) },
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
    ranking: { score: 62, base_score: 62, ai_adjustment: null, urgency: "today", reason: "A task you created", ranked_by: "rules", fired: ["kind tab.task = 62"] },
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
const withImages = (a: string[]) => (a.length ? { attachments: a } : null);

function simulateTurn(session: Session, item: QueueItem, text: string, attachments: string[] = []) {
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  (async () => {
    post(session, "user", "text", text, withImages(attachments));
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
    post(session, "system", "result", "", { cost_usd: 0.021, duration_ms: 8400, context: { model: "claude-sonnet-5-5", window: 200000, used: 38200, first_call: 21000, cache_read: 30100, cache_write: 6200, fresh_input: 1100, output: 800, tab_context: 2400 } });
    status(session, "idle");
  })();
}

/**
 * `?showcase`: the incident item opens on a finished turn and a second one
 * caught mid-work (plan, tool calls, tests running), for screenshots
 * of the chat on tabqueue.com.
 */
const showcasePulls: Record<string, LinkSummary> = {
  "acme/payments#488": { title: "Cut cart cache TTL to 5s for price freshness", state: "merged", author: "marco-r", checks: "passing", updated_at: ago(70) },
  "acme/payments#493": { title: "Restore cart cache TTL behind cart_cache_ttl flag", state: "open", author: "you", checks: "pending", updated_at: ago(1), merge_state: "blocked", review_decision: "review_required", open_threads: 0 },
};

function showcase() {
  const item = items.find((i) => i.title.includes("#incident-checkout"));
  if (!item) return;
  const session: Session = { id: "sess-showcase", item_id: item.id, agent: "claude_code", agent_session_id: "4f1c-showcase", title: "Checkout latency after the 14:10 deploy", status: "working", cwd: "~/src/payments", created_at: ago(6), updated_at: ago(0) };
  sessions.push(session);
  const tool = (content: string, name: string, input: unknown, result?: string) => {
    post(session, "assistant", "tool", content, { tool: name, input });
    if (result !== undefined) post(session, "tool", "tool_result", result);
  };
  post(session, "user", "text", "What changed in the 14:10 deploy? Is the cart cache the cause?");
  tool("mcp__tab__tab_search: 14:10 deploy checkout", "mcp__tab__tab_search", { query: "14:10 deploy checkout" }, "3 results");
  post(session, "assistant", "text", "The incident thread puts the slowdown right at 14:10. Checking what that deploy shipped.");
  tool("git log --oneline --since=13:30 origin/main", "Bash", { command: "git log --oneline --since=13:30 origin/main", description: "List commits in the deploy" }, "a41c2e9 Cut cart cache TTL to 5s for price freshness (#488)\n7be0d13 Bump tracing to 0.1.41");
  tool("Read src/cart/cache.rs", "Read", { file_path: "src/cart/cache.rs" }, "const CART_TTL: Duration = Duration::from_secs(5);");
  tool("mcp__tab__tab_add_found: #incident-checkout thread", "mcp__tab__tab_add_found", { title: "#incident-checkout thread" }, "Added under found.");
  post(session, "assistant", "text",
    "Yes. The 14:10 deploy shipped one real change:\n\nhttps://github.com/acme/payments/pull/488\n\nIt cut `CART_TTL` from **300s to 5s**, so almost every `/checkout` call now misses the cache and rebuilds the cart from Postgres. That matches the p99 jump from 400ms to 1.7s in the incident thread, which I filed under Found.\n\nThe safe fix is to restore 300s behind a flag, so pricing can still opt into the short TTL.");
  post(session, "system", "result", "", { cost_usd: 0.034, duration_ms: 14200, context: { model: "claude-opus-5-5", window: 200000, used: 46800, first_call: 22400, cache_read: 41200, cache_write: 4300, fresh_input: 1300, output: 1100, tab_context: 2600 } });
  post(session, "user", "text", "Restore the TTL behind a flag, open a PR, and let Priya know in the thread.");
  tool("mcp__tab__tab_steps", "mcp__tab__tab_steps", { steps: [
    { title: "Find the TTL change", status: "completed" },
    { title: "Restore 300s behind a flag", status: "completed" },
    { title: "Run the cart tests", status: "in_progress", activeForm: "Running the cart tests" },
    { title: "Open a PR", status: "pending" },
    { title: "Let Priya know in #incident-checkout", status: "pending" },
  ] });
  tool("git switch -c fix/cart-cache-ttl", "Bash", { command: "git switch -c fix/cart-cache-ttl", description: "Make a branch" }, "Switched to a new branch 'fix/cart-cache-ttl'");
  tool("Edit src/cart/cache.rs", "Edit", { file_path: "src/cart/cache.rs" }, "Updated src/cart/cache.rs");
  tool("Edit src/flags.rs", "Edit", { file_path: "src/flags.rs" }, "Updated src/flags.rs");
  post(session, "assistant", "text", "`CART_TTL` is back to 300s behind the `cart_cache_ttl` flag, so pricing can still opt into 5s. Running the tests before I open the PR.");
  tool("cargo test -p cart", "Bash", { command: "cargo test -p cart", description: "Run the cart tests" });
}
if (typeof location !== "undefined" && new URLSearchParams(location.search).has("showcase")) showcase();

/** Task statuses by item, for `?statuses`: what the GitHub connector would report. */
const mockContextStatus: Record<string, ContextStatus[]> = {};

/** `?statuses`: idle chats on the first few items, one per row status. */
function statusesDemo() {
  // Skip the first: the app opens it, which reads its reply.
  const open = items.filter((i) => i.state === "open").slice(1, 5);
  const later = new Date(Date.now() + 60_000).toISOString();
  const pr = (n: number, label: string, tone: ContextStatus["tone"], waiting_on: ContextStatus["waiting_on"]): ContextStatus =>
    ({ source: "github", label, detail: `acme/api#${n}: ${label}`, tone, waiting_on, finished: waiting_on === "nobody", url: `https://github.com/acme/api/pull/${n}` });
  open.forEach((item, k) => {
    // The first finished a turn you haven't read; the rest you've read.
    sessions.push({ id: `sess-status-${k}`, item_id: item.id, agent: "claude_code", agent_session_id: null, title: item.title, status: "idle", cwd: "~/code", created_at: ago(30), updated_at: k === 0 ? later : ago(30) });
  });
  if (open[1]) mockContextStatus[open[1].id] = [pr(114, "Review PR", "accent", "you")];
  if (open[2]) mockContextStatus[open[2].id] = [pr(115, "In review", "info", "others")];
  if (open[3]) mockContextStatus[open[3].id] = [pr(116, "Merged", "success", "nobody")];
}
if (typeof location !== "undefined" && new URLSearchParams(location.search).has("statuses")) statusesDemo();

/** The demo items' entities as a graph, with projects as nodes. */
/** Keys of graph nodes the person deleted, so the mock graph forgets them. */
const removedNodes = new Set<string>();

/** Nodes whose items are all among `mine`, so they'd be left floating without this one. */
function exclusiveOf(node: Entity, mine: Set<string>): Entity[] {
  return mockGraph(null, null).nodes.filter((n) => {
    if (n.id === node.id || n.kind === "project") return false;
    const theirs = items.filter((i) => i.entities.some((x) => x.key === n.key));
    return theirs.length > 0 && theirs.every((i) => mine.has(i.id));
  });
}

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
    const here = i.entities.filter((e) => !removedNodes.has(e.key)).map((e) => node(e.kind, e.key, e.label));
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

function simulateCoordinatorTurn(session: Session, project: Project, text: string, attachments: string[] = []) {
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  (async () => {
    post(session, "user", "text", text, withImages(attachments));
    status(session, "working");
    await sleep(400);
    post(session, "assistant", "tool", `tab: tab_project: ${project.name}`, { tool: "mcp__tab__tab_project" });
    post(session, "tool", "tool_result", "about, rules, 6 resources, 3 notes");
    post(session, "assistant", "tool", `tab: tab_search: ${text.slice(0, 40)}`, { tool: "mcp__tab__tab_search" });
    post(session, "tool", "tool_result", "5 matches");
    const open = items.filter((i) => i.project_id === project.id && i.state !== "done");
    const reply = `Here's where **${project.name}** stands.\n\n- **${open.length} open** in the queue${open[0] ? `, most urgent: ${open[0].title}` : ""}.\n- The billing inbox had 3 refund questions this week; all are inside the 30-day policy (Refund policy note).\n- Stripe shows no new disputes since Monday.\n\nWant me to start a task for any of these?`;
    for (let i = 0; i < reply.length; i += 14) {
      emit({ type: "session_delta", session_id: session.id, text: reply.slice(i, i + 14) });
      await sleep(25);
    }
    post(session, "assistant", "text", reply);
    post(session, "system", "result", "", { cost_usd: 0.011, duration_ms: 4800, context: { model: "claude-opus-5-5", window: 200000, used: 31200 + messages.filter((m) => m.session_id === session.id).length * 900, first_call: 24000, tab_context: 3000 } });
    status(session, "idle");
  })();
}

/** A canned review answer: explains the sample's capture loop, with code links. */
function simulateReviewTurn(session: Session, text: string, attachments: string[] = []) {
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  (async () => {
    post(session, "user", "text", text, withImages(attachments));
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
  // A long file with scattered edits, to exercise big stacked diffs.
  const bigLines = Array.from({ length: 3000 }, (_, i) => `export const entry${i} = { id: ${i}, label: "ledger entry ${i}", amount: ${(i * 37) % 1000} };`);
  const bigAfter = bigLines.map((l, i) => (i % 150 === 75 ? l.replace("ledger entry", "settled entry") : l));
  bigAfter.splice(1500, 0, "// settled entries are archived monthly", "export const ARCHIVE_AFTER_DAYS = 30;");
  return {
    repo, number, node_id: "sample", title: "Retry payment capture with idempotency keys",
    body: "Captures sometimes time out on Stripe's side and we drop the charge.\n\nThis retries timeouts and connection resets up to four times with jittered backoff, and sends an idempotency key so a retry can't double-capture.\n\n- `capture()` retries transient errors\n- new `backoff()` helper\n- test for the retry path",
    author: "dana-k", author_avatar: null, url: `https://github.com/${repo}/pull/${number}`,
    state: "open", draft: false, base_ref: "main", head_ref: "dana/capture-retries", head_repo: repo,
    base_sha: "sample-base", head_sha: "sample-head", additions: 29, deletions: 4, commits: 3, created_at: ago(180),
    mergeable: "mergeable", merge_state: "unstable", merge_method: "squash", review_decision: "review_required",
    checks: { state: "failure", runs: [
      { name: "lint", status: "completed", conclusion: "success", url: null },
      { name: "unit-tests (node 22)", status: "completed", conclusion: "failure", url: null, required: true },
      { name: "build", status: "completed", conclusion: "success", url: null },
    ] },
    labels: [{ name: "payments", color: "0e8a16" }], reviewers: ["you"], viewer: "you", viewer_can_update: true,
    files: [
      f("src/capture.ts", "modified", 18, 4, before, after),
      f("src/backoff.ts", "added", 5, 0, "", backoff),
      f("test/capture.test.ts", "modified", 6, 0, testBefore, testAfter),
      f("src/ledger.ts", "modified", 22, 20, bigLines.join("\n") + "\n", bigAfter.join("\n") + "\n"),
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
