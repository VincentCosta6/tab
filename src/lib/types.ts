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
  | { type: "approval_resolved"; session_id: string; id: string };

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
  kind: "text" | "tool" | "tool_result" | "status" | "error" | "approval" | "result";
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

export interface PrFile { filename: string; status: string; additions: number; deletions: number; previous_filename?: string | null; original?: string | null; modified?: string | null }
export interface ReviewComment { id: number; path: string; line: number | null; side: string | null; body: string; user: string; created_at: string }
export interface PullReview { repo: string; number: number; title: string; author: string; url: string; base_sha: string; head_sha: string; files: PrFile[]; comments: ReviewComment[]; sample: boolean }

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
    ai: { enabled: boolean; api_key: string | null; model: string | null; instructions: string };
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
}
