// The one place the UI talks to the backend. In a plain browser (vite dev
// without Tauri) it falls back to an in-memory mock so the UI can be built
// and screenshotted without the desktop shell.

import type {
  AuditEntry, Block, ConnectorInfo, EngineEvent, ItemDetail, ItemState, QueueItem,
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
  itemPreview: (id: string) => call<Block[]>("item_preview", { id }),
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
