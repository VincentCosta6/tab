// Data layer: React Query with caching turned all the way up.
//
// Nothing goes stale on a timer (staleTime: Infinity) and nothing is evicted
// (gcTime: Infinity). The cache is persisted to localStorage, so a restart
// renders the last known queue, previews and chats instantly. Freshness comes
// from the engine instead of polling: every engine event patches or
// invalidates exactly the queries it affects (see useLiveEvents). Remote APIs
// are never hit from here directly; previews come from the backend's own
// preview cache, which a rate-limit-aware refresher keeps current.

import { QueryClient, useQueryClient } from "@tanstack/react-query";
import { PersistQueryClientProvider } from "@tanstack/react-query-persist-client";
import { createSyncStoragePersister } from "@tanstack/query-sync-storage-persister";
import { useEffect, type ReactNode } from "react";
import { onEngineEvent } from "./api";
import type { Chat, ConnectorInfo, ItemContext, QueueItem } from "./types";

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: Infinity,
      gcTime: Infinity,
      refetchOnWindowFocus: false,
      refetchOnReconnect: false,
      retry: 1,
    },
  },
});

const persister = createSyncStoragePersister({
  storage: typeof window === "undefined" ? undefined : safeStorage(),
  key: "tab.query-cache",
  throttleTime: 1000,
});

function safeStorage(): Storage | undefined {
  try {
    const s = window.localStorage;
    s.setItem("tab.probe", "1");
    s.removeItem("tab.probe");
    return s;
  } catch {
    return undefined;
  }
}

/** Bump when cached shapes change so old caches are dropped. */
const CACHE_VERSION = "2";

export const keys = {
  items: (state: string, query: string) => ["items", state, query] as const,
  allItems: ["items"] as const,
  item: (id: string) => ["item", id] as const,
  preview: (id: string) => ["preview", id] as const,
  context: (id: string) => ["context", id] as const,
  chat: (itemId: string) => ["chat", itemId] as const,
  stream: (sessionId: string) => ["stream", sessionId] as const,
  connectors: ["connectors"] as const,
  config: ["config"] as const,
  projects: ["projects"] as const,
  agentActivity: ["agent-activity"] as const,
  notes: (projectId: string) => ["notes", projectId] as const,
  pressure: ["pressure"] as const,
  pull: (repo: string, n: number) => ["pull", repo, n] as const,
  file: (repo: string, sha: string, path: string) => ["file", repo, sha, path] as const,
};

export function QueryProvider({ children }: { children: ReactNode }) {
  return (
    <PersistQueryClientProvider
      client={queryClient}
      persistOptions={{
        persister,
        maxAge: Infinity,
        buster: CACHE_VERSION,
        // Streaming buffers and live telemetry are not worth restoring.
        dehydrateOptions: { shouldDehydrateQuery: (q) => !["stream", "pressure", "search-index"].includes(String(q.queryKey[0])) && q.state.status === "success" },
      }}
      // The queue is a cheap local read, so re-read it once after restoring:
      // items can change while the app is closed (or a connection was turned off).
      onSuccess={() => queryClient.invalidateQueries({ queryKey: ["items"] })}
    >
      <LiveEvents />
      {children}
    </PersistQueryClientProvider>
  );
}

/** Finds the chat query (keyed by item) that holds a session. */
function chatKeyForSession(qc: QueryClient, sessionId: string) {
  for (const [key, data] of qc.getQueriesData<Chat>({ queryKey: ["chat"] })) {
    if (data?.session?.id === sessionId) return key;
  }
  return null;
}

function LiveEvents() {
  const qc = useQueryClient();
  useEffect(() => {
    let reload: number | undefined;
    let activity: number | undefined;
    let off: (() => void) | undefined;
    onEngineEvent((e) => {
      switch (e.type) {
        case "item_changed": {
          qc.setQueryData<QueueItem>(keys.item(e.item.id), e.item);
          qc.setQueriesData<ItemContext | null>({ queryKey: keys.context(e.item.id) }, (c) => (c ? { ...c, item: e.item } : c));
          // Lists are local SQLite reads; coalesce bursts into one refetch.
          clearTimeout(reload);
          reload = window.setTimeout(() => qc.invalidateQueries({ queryKey: keys.allItems }), 120);
          break;
        }
        case "connector_changed":
          qc.setQueryData<ConnectorInfo[]>(keys.connectors, (cs) => cs?.map((c) => (c.id === e.connector.id ? e.connector : c)));
          break;
        case "preview_changed":
          qc.setQueryData(keys.preview(e.preview.item_id), e.preview);
          break;
        case "context_changed":
          qc.invalidateQueries({ queryKey: keys.context(e.item_id) });
          break;
        case "project_changed":
          qc.invalidateQueries({ queryKey: keys.notes(e.project_id) });
          break;
        case "approval_requested":
        case "approval_resolved":
        case "session_changed":
          clearTimeout(activity);
          activity = window.setTimeout(() => qc.invalidateQueries({ queryKey: keys.agentActivity }), 150);
          break;
      }
      switch (e.type) {
        case "session_changed": {
          const s = e.session;
          if (!s.item_id) {
            // Keyed sessions (a PR's review chat) are found by id.
            const key = chatKeyForSession(qc, s.id);
            if (key) qc.setQueryData<Chat>(key, (c) => (c ? { ...c, session: s, running: s.status === "working" || s.status === "needs_input" } : c));
          } else
            qc.setQueryData<Chat>(keys.chat(s.item_id), (c) => ({
              session: s,
              messages: c?.session?.id === s.id ? c.messages : [],
              approvals: c?.session?.id === s.id ? c.approvals : [],
              running: s.status === "working" || s.status === "needs_input",
            }));
          break;
        }
        case "session_message": {
          const m = e.message;
          const key = chatKeyForSession(qc, m.session_id);
          if (key)
            qc.setQueryData<Chat>(key, (c) => (c && !c.messages.some((x) => x.id === m.id) ? { ...c, messages: [...c.messages, m] } : c));
          // A stored assistant message replaces the streamed text so far.
          if (m.role === "assistant" || m.kind === "result") qc.setQueryData(keys.stream(m.session_id), "");
          break;
        }
        case "session_delta":
          qc.setQueryData<string>(keys.stream(e.session_id), (t) => (t ?? "") + e.text);
          break;
        case "approval_requested": {
          const key = chatKeyForSession(qc, e.approval.session_id);
          if (key) qc.setQueryData<Chat>(key, (c) => (c ? { ...c, approvals: [...c.approvals.filter((a) => a.id !== e.approval.id), e.approval] } : c));
          break;
        }
        case "config_changed":
          qc.invalidateQueries({ queryKey: keys.config });
          qc.invalidateQueries({ queryKey: keys.connectors });
          break;
        case "approval_resolved": {
          const key = chatKeyForSession(qc, e.session_id);
          if (key) qc.setQueryData<Chat>(key, (c) => (c ? { ...c, approvals: c.approvals.filter((a) => a.id !== e.id) } : c));
          break;
        }
      }
    }).then((u) => (off = u));
    return () => {
      clearTimeout(reload);
      off?.();
    };
  }, [qc]);
  return null;
}
