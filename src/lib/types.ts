// Mirrors tab-core's model (crates/tab-core/src/model.rs). Keep in sync.

export type Tone = "neutral" | "accent" | "info" | "success" | "warning" | "danger";
export type Urgency = "now" | "today" | "this_week" | "whenever";
export type ItemState = "open" | "snoozed" | "done";

export interface Field { label: string; value: string; tone?: Tone }
export interface Link { label: string; url: string }

export type Block =
  | { type: "text"; text: string; markdown?: boolean }
  | { type: "quote"; text: string; author?: string | null }
  | { type: "fields"; items: Field[] }
  | { type: "stats"; items: Field[] }
  | { type: "badges"; items: Field[] }
  | { type: "links"; items: Link[] }
  | { type: "code"; text: string; language?: string | null }
  | { type: "html"; html: string }
  | { type: "choices"; prompt: string; options: string[] };

export interface EntityRef { kind: string; key: string; label: string; rel: string }

export interface Ranking {
  score: number;
  base_score: number;
  ai_adjustment: number | null;
  urgency: Urgency;
  reason: string;
  ranked_by: string;
}

export interface QueueItem {
  id: string;
  source: string;
  external_id: string;
  kind: string;
  title: string;
  body: string | null;
  url: string | null;
  actor: string | null;
  context: string | null;
  occurred_at: string;
  blocks: Block[];
  entities: EntityRef[];
  metadata: unknown;
  state: ItemState;
  ranking: Ranking | null;
  snoozed_until: string | null;
  project_id?: string | null;
  created_at: string;
  updated_at: string;
}

/** Search by meaning: what the local index is doing. */
export type IndexStatus =
  | { state: "off" }
  | { state: "downloading"; model: string; received: number; total: number | null }
  | { state: "loading"; model: string }
  | { state: "indexing"; model: string; done: number; total: number }
  | { state: "ready"; model: string; dim: number; docs: number; indexed_at: string | null }
  | { state: "error"; model: string; message: string; retry_at: string };

/** A group of work the person defines. Chats started in it belong to it. */
export interface Project { id: string; name: string; color: string; icon: string; created_at: string }

/** A durable fact about a project, from an agent or the person. One per topic. */
export interface ProjectNote {
  id: string;
  project_id: string;
  topic: string;
  text: string;
  added_by: string;
  session_id: string | null;
  sources: string[];
  created_at: string;
  updated_at: string;
}

export interface ItemDetail { item: QueueItem; related: QueueItem[] }

export interface KindInfo { kind: string; label: string; icon: string; tone: Tone }
export type SettingKind = "text" | "secret" | "number" | "toggle";
export interface SettingField { key: string; label: string; kind: SettingKind; help?: string | null; required: boolean }

export type ConnectorStatus =
  | { state: "disabled" }
  | { state: "starting" }
  | { state: "running"; last_sync: string | null }
  | { state: "rate_limited"; until: string }
  | { state: "error"; message: string }
  | { state: "stopped" };

export interface RateLimitSpec { provider: string; resource: string; requests: number; per_seconds: number; burst: number; note: string }

export interface ConnectorInfo {
  id: string;
  name: string;
  icon: string;
  description: string;
  kinds: KindInfo[];
  settings: SettingField[];
  available: boolean;
  rate_limits: RateLimitSpec[];
  category: string;
  features: string[];
  builtin: boolean;
  enabled: boolean;
  status: ConnectorStatus;
}

export type Change = "ingested" | "updated" | "ranked" | "snoozed" | "woke" | "done" | "reopened";
export type EngineEvent =
  | { type: "item_changed"; change: Change; item: QueueItem }
  | { type: "connector_changed"; connector: ConnectorInfo }
  | { type: "preview_changed"; preview: Preview }
  | { type: "context_changed"; item_id: string }
  | { type: "session_changed"; session: Session }
  | { type: "session_message"; message: SessionMessage }
  | { type: "session_delta"; session_id: string; text: string }
  | { type: "approval_requested"; approval: Approval }
  | { type: "approval_resolved"; session_id: string; id: string }
  | { type: "config_changed" }
  | { type: "project_changed"; project_id: string };

export interface Preview { item_id: string; blocks: Block[]; fetched_at: string | null; stale: boolean }

export type ContextSection = "context" | "found";
export interface ContextEntry {
  id: string;
  item_id: string;
  section: ContextSection;
  pinned: boolean;
  ref_item: QueueItem | null;
  url: string | null;
  title: string;
  summary: string | null;
  added_by: string;
  created_at: string;
}
export interface NewContext { section: ContextSection; ref_item_id?: string | null; url?: string | null; title: string; summary?: string | null; added_by: string }
export interface ItemContext { item: QueueItem; entries: ContextEntry[]; related: QueueItem[] }

export type AgentKind = "claude_code" | "opencode";
export type SessionStatus = "idle" | "working" | "needs_input" | "failed";
export interface Session {
  id: string;
  item_id: string | null;
  agent: AgentKind;
  agent_session_id: string | null;
  title: string;
  status: SessionStatus;
  cwd: string;
  created_at: string;
  updated_at: string;
}
export interface SessionMessage {
  id: number;
  session_id: string;
  role: "user" | "assistant" | "tool" | "system";
  kind: "text" | "tool" | "tool_result" | "status" | "error" | "approval" | "result" | "secret_request";
  content: string;
  meta: any;
  created_at: string;
}
export interface Approval { id: string; session_id: string; tool: string; summary: string; input: unknown }
export interface Chat { session: Session | null; messages: SessionMessage[]; approvals: Approval[]; running: boolean }
export interface ChatStarted { item: QueueItem; session: Session }
export interface NewTask { title: string; body?: string | null; priority?: string | null; due_at?: string | null; url?: string | null; created_by: string }
export interface GhStatus {
  installed: boolean;
  path: string | null;
  version: string | null;
  logged_in: boolean;
  user: string | null;
  install_command: string | null;
  error: string | null;
}

export interface GhLoginCode { code: string; url: string }

export interface AgentProbe { kind: AgentKind; found: boolean; path: string | null; version: string | null; error: string | null }

export interface Pressure {
  provider: string;
  resource: string;
  used: number;
  budget: number | null;
  window_seconds: number | null;
  remote_remaining: number | null;
  remote_limit: number | null;
  headroom: number;
  recent: number;
  burst_threshold: number;
  cooling_until: string | null;
  deferred: number;
}

export interface PrFile {
  filename: string; status: string; additions: number; deletions: number; previous_filename?: string | null;
  /** `viewed`, `unviewed`, or `dismissed` (viewed, then changed since). */
  viewed: string;
  original?: string | null; modified?: string | null;
}
export interface ThreadComment { id: string; database_id: number | null; author: string; avatar: string | null; body: string; created_at: string; pending: boolean; url: string | null; viewer_did_author: boolean }
export interface ReviewThread {
  id: string; path: string; line: number | null; start_line: number | null; original_line: number | null;
  side: "LEFT" | "RIGHT"; resolved: boolean; outdated: boolean; can_resolve: boolean; can_reply: boolean; diff_hunk: string | null;
  comments: ThreadComment[];
}
export interface CheckRun { name: string; status: string; conclusion: string | null; url: string | null }
export interface PullReview {
  repo: string; number: number; node_id: string; title: string; body: string; author: string; author_avatar: string | null; url: string;
  state: "open" | "closed" | "merged"; draft: boolean; base_ref: string; head_ref: string; head_repo: string | null;
  base_sha: string; head_sha: string; additions: number; deletions: number; commits: number; created_at: string;
  mergeable: string; merge_state: string; review_decision: string | null;
  checks: { state: string | null; runs: CheckRun[] };
  labels: { name: string; color: string }[]; reviewers: string[];
  viewer: string; viewer_can_update: boolean;
  files: PrFile[]; threads: ReviewThread[]; pending_review: { id: string; body: string } | null;
  sample_tree: { path: string; content: string }[];
  sample: boolean;
}
export interface ReviewWorkspace { path: string; head_sha: string; merge_base: string; files: string[]; reused: boolean }
export interface WorkspaceProgress { repo: string; number: number; step: "clone" | "fetch" | "checkout" | "index"; state: "running" | "done" | "error"; message: string; percent: number | null }
export interface SearchHit { path: string; line: number; column: number; text: string }
/** A language server tab can run for a checkout, and whether it's installed. */
export interface LspServer {
  id: string; name: string; languages: string[]; command: string; note: string;
  installed: boolean; path: string | null;
  /** What Install runs, when a tool for it is present. */
  install_command: string | null;
  /** When nothing here can install it: what to get first. */
  needs: string | null; needs_url: string | null;
}
export interface LspStarted { session: string; root_uri: string; capabilities: Record<string, unknown> }
export interface LspEvent { session: string; method: string; params: any }
export interface SearchQuery { query: string; regex: boolean; case_sensitive: boolean; whole_word: boolean; include: string | null }
export type ReviewAction =
  | { type: "mark_viewed"; path: string; viewed: boolean }
  | { type: "mark_viewed_many"; paths: string[]; viewed: boolean }
  | { type: "add_thread"; path: string; line: number; start_line: number | null; side: "LEFT" | "RIGHT"; body: string; pending: boolean }
  | { type: "reply"; thread_id: string; comment_id: number | null; body: string; pending: boolean }
  | { type: "edit_comment"; comment_id: string; body: string }
  | { type: "delete_comment"; comment_id: string }
  | { type: "resolve"; thread_id: string; resolved: boolean }
  | { type: "submit"; event: "comment" | "approve" | "request_changes"; body: string }
  | { type: "discard" }
  | { type: "set_state"; state: "open" | "closed" }
  | { type: "set_draft"; draft: boolean }
  | { type: "merge"; method: "merge" | "squash" | "rebase" }
  | { type: "update_branch" };
export interface PrRef { repo: string; number: number; node_id: string; head_sha: string; pending_review_id: string | null }

export interface RateLimitState {
  provider: string;
  resource: string;
  limit: number | null;
  remaining: number | null;
  reset_at: string | null;
  blocked_until: string | null;
  updated_at: string;
}

export type Outcome = "network" | "cache_hit" | "revalidated" | "stale" | "rate_limited" | "deferred" | "error";

export interface AuditEntry {
  id: number;
  ts: string;
  provider: string;
  connector: string;
  purpose: string;
  method: string;
  url: string;
  outcome: Outcome;
  status: number | null;
  duration_ms: number;
  rl_resource: string | null;
  rl_limit: number | null;
  rl_remaining: number | null;
  rl_reset: string | null;
  error: string | null;
}

export interface UsageRow {
  provider: string;
  connector: string;
  purpose: string;
  total: number;
  network: number;
  cache_hits: number;
  revalidated: number;
  stale: number;
  rate_limited: number;
  errors: number;
  avg_ms: number;
}

export interface KindScore { kind: string; score: number; label: string }
export interface KeywordRule { contains: string; delta: number }
export interface RankingRules {
  kinds: KindScore[];
  default_score: number;
  keywords: KeywordRule[];
  vip_actors: string[];
  vip_boost: number;
  muted: string[];
}

export interface TabConfig {
  profile: { name: string; role: string; handles: string[] };
  ranking: {
    rules: RankingRules;
    ai: { enabled: boolean; backend: "auto" | "api" | "claude_code"; api_key: string | null; model: string | null; instructions: string };
  };
  connectors: Record<string, { enabled: boolean; settings: Record<string, unknown> }>;
  agent: AgentConfig;
  mcp: { port: number; token: string };
  onboarded: boolean;
}

export interface AgentConfig {
  kind: AgentKind | null;
  command: string | null;
  model: string | null;
  workdir: string | null;
  permission_mode: string;
  opencode_url: string | null;
  budget: BudgetConfig;
}

export interface BudgetConfig {
  daily_usd: number | null;
  monthly_usd: number | null;
  per_turn_usd: number | null;
  /** 0..1: background queue reading pauses past this share of a Claude plan window. */
  ranker_plan_cap: number;
}

export interface PlanWindow {
  window: string;
  utilization: number | null;
  resets_at: string | null;
  status: string;
  updated_at: string;
}

export interface UsageSummary {
  today_usd: number;
  month_usd: number;
  today_agent_usd: number;
  today_ranker_usd: number;
  today_turns: number;
  today_tokens: number;
  billing: "subscription" | "api" | "unknown" | null;
  windows: PlanWindow[];
}
