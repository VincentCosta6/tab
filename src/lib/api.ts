// The one place the UI talks to the backend. In a plain browser (vite dev
// without Tauri) it falls back to an in-memory mock so the UI can be built
// and screenshotted without the desktop shell.

import type {
  AgentProbe, AuditEntry, Chat, ChatStarted, ConnectorInfo, ContextEntry, ContextSection, EngineEvent, GhLoginCode, GhStatus,
  ItemContext, ItemDetail, ItemState, NewContext, NewTask, Pressure, Preview, PrRef, PullReview, QueueItem,
  RateLimitState, ReviewAction, ReviewWorkspace, SearchHit, SearchQuery, TabConfig, UsageRow, UsageSummary, WorkspaceProgress,
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
  usageSummary: () => call<UsageSummary>("usage_summary"),
  ghStatus: () => call<GhStatus>("gh_status"),
  ghLogin: () => call<GhStatus>("gh_login"),
  ghInstall: () => call<GhStatus>("gh_install"),
  mcpInfo: () => call<{ url: string | null; token: string }>("mcp_info"),
  githubPull: (repo: string, number: number, sample: boolean) => call<PullReview>("github_pull", { repo, number, sample }),
  githubFile: (repo: string, sha: string, path: string) => call<string>("github_file", { repo, sha, path }),
  githubReviewAction: (pr: PrRef, action: ReviewAction) => call<void>("github_review_action", { pr, action }),
  workspacePrepare: (target: { repo: string; number: number; head_sha: string; base_sha: string; base_ref: string }) =>
    call<ReviewWorkspace>("review_workspace_prepare", { target }),
  workspaceFile: (repo: string, sha: string, path: string) => call<string>("review_workspace_file", { repo, sha, path }),
  workspaceFiles: (repo: string, number: number, paths: string[]) => call<[string, string][]>("review_workspace_files", { repo, number, paths }),
  workspaceSearch: (repo: string, number: number, query: SearchQuery) => call<SearchHit[]>("review_workspace_search", { repo, number, query }),
  workspaceOpen: (repo: string, number: number, target: "vscode" | "folder", path?: string, line?: number) =>
    call<void>("review_workspace_open", { repo, number, target, path: path ?? null, line: line ?? null }),
  vscodeAvailable: () => call<boolean>("review_vscode_available"),
};

export async function openUrl(url: string) {
  if (!inTauri) {
    window.open(url, "_blank", "noopener");
    return;
  }
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(url);
}

/** The one-time code `gh auth login` wants typed at github.com/login/device. */
export async function onGhCode(cb: (c: GhLoginCode) => void): Promise<() => void> {
  if (!inTauri) return mockApi.onGhCode(cb);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<GhLoginCode>("tab://gh-code", (e) => cb(e.payload));
}

export async function onEngineEvent(cb: (e: EngineEvent) => void): Promise<() => void> {
  if (!inTauri) return mockApi.subscribe(cb);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<EngineEvent>("tab://event", (e) => cb(e.payload));
}

/** Clone and checkout progress while a review workspace is prepared. */
export async function onWorkspaceProgress(cb: (p: WorkspaceProgress) => void): Promise<() => void> {
  if (!inTauri) return mockApi.onWorkspace(cb);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<WorkspaceProgress>("tab://workspace", (e) => cb(e.payload));
}
