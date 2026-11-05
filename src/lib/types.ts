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

export interface ConnectorInfo {
  id: string;
  name: string;
  icon: string;
  description: string;
  kinds: KindInfo[];
  settings: SettingField[];
  available: boolean;
  enabled: boolean;
  status: ConnectorStatus;
}

export type Change = "ingested" | "updated" | "ranked" | "snoozed" | "woke" | "done" | "reopened";
export type EngineEvent =
  | { type: "item_changed"; change: Change; item: QueueItem }
  | { type: "connector_changed"; connector: ConnectorInfo };

export interface RateLimitState {
  provider: string;
  resource: string;
  limit: number | null;
  remaining: number | null;
  reset_at: string | null;
  blocked_until: string | null;
  updated_at: string;
}

export type Outcome = "network" | "cache_hit" | "revalidated" | "stale" | "rate_limited" | "error";

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
}
