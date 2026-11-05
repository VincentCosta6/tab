// The one place the UI talks to the backend. In a plain browser (vite dev
// without Tauri) it falls back to an in-memory mock so the UI can be built
// and screenshotted without the desktop shell.

import type {
  AgentProbe, AuditEntry, Chat, ChatStarted, ConnectorInfo, ContextEntry, ContextSection, EngineEvent,
  ItemContext, ItemDetail, ItemState, NewContext, NewTask, Pressure, Preview, PullReview, QueueItem,
  RateLimitState, TabConfig, UsageRow,
} from "./types";
import { mockApi } from "./mock";

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!inTauri) return mockApi.invoke<T>(cmd, args);
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export const api = {
  listItems: (states: ItemState[]) => call<QueueItem[]>("list_items", { states }),
  searchItems: (query: string, states: ItemState[]) => call<QueueItem[]>("search_items", { query, states }),
  itemDetail: (id: string) => call<ItemDetail | null>("item_detail", { id }),
  itemPreview: (id: string) => call<Preview>("item_preview", { id }),
  markDone: (id: string) => call<QueueItem | null>("mark_done", { id }),
  snooze: (id: string, minutes: number) => call<QueueItem | null>("snooze", { id, minutes }),
  reopen: (id: string) => call<QueueItem | null>("reopen", { id }),
  listConnectors: () => call<ConnectorInfo[]>("list_connectors"),
  rateLimits: () => call<RateLimitState[]>("rate_limits"),
  auditLog: (query: { provider?: string; connector?: string; outcome?: string; limit?: number }) =>
    call<AuditEntry[]>("audit_log", { query }),
  apiUsage: (hours: number) => call<UsageRow[]>("api_usage", { hours }),
  getConfig: () => call<TabConfig>("get_config"),
  saveConfig: (config: TabConfig) => call<TabConfig>("save_config", { config }),
  ratePressure: () => call<Pressure[]>("rate_pressure"),
  itemContext: (id: string) => call<ItemContext | null>("item_context", { id }),
  contextAdd: (itemId: string, entry: NewContext) => call<ContextEntry>("context_add", { itemId, entry }),
  contextUpdate: (id: string, patch: { section?: ContextSection; pinned?: boolean }) =>
    call<ContextEntry | null>("context_update", { id, section: patch.section ?? null, pinned: patch.pinned ?? null }),
  contextRemove: (id: string) => call<void>("context_remove", { id }),
  createTask: (task: NewTask) => call<QueueItem>("create_task", { task }),
  chatOpen: (itemId: string) => call<Chat>("chat_open", { itemId }),
  chatSend: (itemId: string | null, text: string) => call<ChatStarted>("chat_send", { itemId, text }),
  chatStop: (sessionId: string) => call<void>("chat_stop", { sessionId }),
  approvalResolve: (id: string, decision: "allow" | "always" | "deny", message?: string) =>
    call<boolean>("approval_resolve", { id, decision, message: message ?? null }),
  detectAgents: () => call<AgentProbe[]>("detect_agents"),
  mcpInfo: () => call<{ url: string | null; token: string }>("mcp_info"),
  githubPull: (repo: string, number: number, sample: boolean) => call<PullReview>("github_pull", { repo, number, sample }),
  githubFile: (repo: string, sha: string, path: string) => call<string>("github_file", { repo, sha, path }),
  githubComment: (c: { repo: string; number: number; commitId: string; path: string; line: number; side: string; body: string }) =>
    call<void>("github_comment", c),
};

export async function openUrl(url: string) {
  if (!inTauri) {
    window.open(url, "_blank", "noopener");
    return;
  }
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(url);
}

export async function onEngineEvent(cb: (e: EngineEvent) => void): Promise<() => void> {
  if (!inTauri) return mockApi.subscribe(cb);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<EngineEvent>("tab://event", (e) => cb(e.payload));
}
