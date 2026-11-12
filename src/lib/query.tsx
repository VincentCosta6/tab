// Data layer: React Query with caching turned all the way up.
//
// Nothing goes stale on a timer (staleTime: Infinity). What no screen has
// shown for ten minutes is dropped (gcTime) and read again when it's next
// needed; these are local reads. The queue, items and previews are persisted
// to localStorage, so a restart renders the last known queue instantly; chats,
// PR files and search results are not (see PERSIST_SKIP). Freshness comes
// from the engine instead of polling: every engine event patches or
// invalidates exactly the queries it affects (see useLiveEvents). Remote APIs
// are never hit from here directly; previews come from the backend's own
// preview cache, which a rate-limit-aware refresher keeps current.

import { QueryClient, useQueryClient } from "@tanstack/react-query";
import { PersistQueryClientProvider, removeOldestQuery } from "@tanstack/react-query-persist-client";
import { createSyncStoragePersister } from "@tanstack/query-sync-storage-persister";
import { useEffect, type ReactNode } from "react";
import { onEngineEvent } from "./api";
import type { Chat, ConnectorInfo, ItemContext, QueueItem, Session } from "./types";

/**
 * Beyond link previews (refreshed for every connection), the queries that
 * show a connection's page, so a change to it shows up right away. A
 * connection with nothing of its own leaves this out.
 */
const pageQueries: Record<string, (page: string) => readonly (readonly unknown[])[]> = {
  github: (page) => {
    const m = /^https:\/\/github\.com\/([^/]+\/[^/]+)(?:\/(pull|issues)\/(\d+))?/.exec(page);
    if (!m) return [];
    const [, repo, kind, n] = m;
    // A repo page means a push or repo change: any of its pull requests may have moved.
    const pulls = kind === "pull" ? [keys.pull(repo, Number(n))] : kind ? [] : [["pull", repo]];
    return [...pulls, ["github"]];
  },
};

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: Infinity,
      // Unbounded, this kept every chat, PR file and search a session ever
      // opened: hundreds of MB after a long day.
      gcTime: 10 * 60_000,
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
  // localStorage holds about 5 MB. Past that every write failed and the saved
  // cache silently stopped updating; drop the oldest queries until it fits.
  retry: removeOldestQuery,
});

/**
 * Queries not saved across restarts: streaming buffers and live telemetry,
 * the app's version and update check (a restart may change them), and what is
 * big and cheap to read again (chats, PR files and pulls, search results).
 */
const PERSIST_SKIP = new Set(["stream", "pressure", "search-index", "workflow-issues", "app-version", "app-update", "chat", "chats", "file", "pull"]);
const persisted = (key: readonly unknown[]) =>
  !PERSIST_SKIP.has(String(key[0])) && !(key[0] === "items" && key[2]);

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
const CACHE_VERSION = "3";

export const keys = {
  items: (state: string, query: string) => ["items", state, query] as const,
  allItems: ["items"] as const,
  mcpServers: ["mcp-servers"] as const,
  mcpCatalog: ["mcp-catalog"] as const,
  mcpTools: (id: string) => ["mcp-tools", id] as const,
  mcpImport: ["mcp-import"] as const,
  mcpCloud: ["mcp-cloud"] as const,
  cloudJobGaps: ["cloud-job-gaps"] as const,
  item: (id: string) => ["item", id] as const,
  preview: (id: string) => ["preview", id] as const,
  context: (id: string) => ["context", id] as const,
  linkedTasks: (url: string) => ["linked-tasks", url] as const,
  chat: (itemId: string) => ["chat", itemId] as const,
  chats: (itemId: string) => ["chats", itemId] as const,
  stream: (sessionId: string) => ["stream", sessionId] as const,
  connectors: ["connectors"] as const,
  config: ["config"] as const,
  projects: ["projects"] as const,
  agentActivity: ["agent-activity"] as const,
  workflowActivity: ["workflow-activity"] as const,
  itemWorkflowRuns: (itemId: string) => ["item-workflow-runs", itemId] as const,
  contextStatus: (ids: string[]) => ["context-status", ids] as const,
  notes: (projectId: string) => ["notes", projectId] as const,
  repos: (projectId: string) => ["project-repos", projectId] as const,
  resources: (projectId: string) => ["project-resources", projectId] as const,
  coordinator: (projectId: string) => ["chat", `coordinator:${projectId}`] as const,
  coordinatorHistory: (projectId: string) => ["coordinator-history", projectId] as const,
  segment: (id: string) => ["segment", id] as const,
  projectItems: (projectId: string) => ["project-items", projectId] as const,
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
        dehydrateOptions: { shouldDehydrateQuery: (q) => persisted(q.queryKey) && q.state.status === "success" },
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
export function chatKeyForSession(qc: QueryClient, sessionId: string) {
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
          if (e.change !== "ranked") qc.invalidateQueries({ queryKey: ["activity", e.item.id] });
          // Patch the item into its cached context, unless that context is
          // already due a refetch: writing to it would mark it fresh and
          // drop entries attached since (staleTime is Infinity).
          if (!qc.getQueryState(keys.context(e.item.id))?.isInvalidated)
            qc.setQueriesData<ItemContext | null>({ queryKey: keys.context(e.item.id) }, (c) => (c ? { ...c, item: e.item } : c));
          // Lists are local SQLite reads; coalesce bursts into one refetch.
          clearTimeout(reload);
          // Agent activity skips done items, so a state change moves the badge.
          reload = window.setTimeout(() => {
            qc.invalidateQueries({ queryKey: keys.allItems });
            qc.invalidateQueries({ queryKey: keys.agentActivity });
          }, 120);
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
          qc.invalidateQueries({ queryKey: ["activity", e.item_id] });
          qc.invalidateQueries({ queryKey: ["context-status"] });
          qc.invalidateQueries({ queryKey: ["linked-tasks"] });
          break;
        case "project_changed":
          // From the hub, a deleted note or repo comes with no project id:
          // refresh every project's.
          qc.invalidateQueries({ queryKey: e.project_id ? keys.notes(e.project_id) : ["notes"] });
          qc.invalidateQueries({ queryKey: e.project_id ? keys.repos(e.project_id) : ["project-repos"] });
          qc.invalidateQueries({ queryKey: e.project_id ? keys.resources(e.project_id) : ["project-resources"] });
          qc.invalidateQueries({ queryKey: e.project_id ? keys.projectItems(e.project_id) : ["project-items"] });
          qc.invalidateQueries({ queryKey: keys.projects });
          break;
        case "workflows_changed":
          qc.invalidateQueries({ queryKey: ["workflows"] });
          break;
        case "cli_changed":
          // An agent changed something on the service. What shows it is
          // refreshed by the pages_changed that comes with it.
          qc.invalidateQueries({ queryKey: ["linked-tasks"] });
          break;
        case "pages_changed":
          // The backend already dropped its cached copies and re-fetches
          // queue items at these pages (they arrive as preview_changed).
          for (const page of e.pages) {
            qc.invalidateQueries({ queryKey: ["link-preview", page] });
            qc.invalidateQueries({ queryKey: ["context-status"] });
            for (const queryKey of pageQueries[e.connector]?.(page) ?? []) qc.invalidateQueries({ queryKey });
          }
          break;
        case "workflow_run":
          qc.invalidateQueries({ queryKey: ["workflow-runs"] });
          qc.invalidateQueries({ queryKey: keys.workflowActivity });
          qc.invalidateQueries({ queryKey: ["workflow-run-view", e.run.id] });
          if (e.run.item_id) qc.invalidateQueries({ queryKey: keys.itemWorkflowRuns(e.run.item_id) });
          if (e.run.item_id && e.run.finished_at) qc.invalidateQueries({ queryKey: ["activity", e.run.item_id] });
          break;
        case "approval_requested":
        case "approval_resolved":
        case "session_changed":
          if (e.type === "session_changed" && e.session.item_id) qc.invalidateQueries({ queryKey: ["activity", e.session.item_id] });
          clearTimeout(activity);
          activity = window.setTimeout(() => {
            qc.invalidateQueries({ queryKey: keys.agentActivity });
            // A workflow's agent asking you something moves its item to "Waiting for you".
            qc.invalidateQueries({ queryKey: keys.workflowActivity });
            qc.invalidateQueries({ queryKey: ["item-workflow-runs"] });
          }, 150);
          break;
      }
      switch (e.type) {
        case "session_changed": {
          const s = e.session;
          if (!s.item_id) {
            // Keyed sessions (a PR's review chat) are found by id.
            const key = chatKeyForSession(qc, s.id);
            if (key) qc.setQueryData<Chat>(key, (c) => (c ? { ...c, session: s, running: isRunning(s, c.running) } : c));
          } else {
            qc.invalidateQueries({ queryKey: keys.chats(s.item_id) });
            qc.setQueryData<Chat>(keys.chat(s.item_id), (c) =>
              // An earlier chat of the task (after /clear) carries on in
              // the background without taking the task's view back.
              c?.session && c.session.id !== s.id && opened(s) < opened(c.session)
                ? c
                : {
                    session: s,
                    messages: c?.session?.id === s.id ? c.messages : [],
                    approvals: c?.session?.id === s.id ? c.approvals : [],
                    running: isRunning(s, c?.running ?? false),
                  },
            );
          }
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
          qc.invalidateQueries({ queryKey: keys.mcpServers });
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

/** When a chat became its task's current one; older copies don't say, so it's when it was created. */
const opened = (s: Session) => s.opened_at ?? s.created_at;

/**
 * Whether a session's turn is still going. "Needs you" means either an
 * approval mid-turn (still going) or a turn that ended on a question (over),
 * and the status alone can't tell which, so it keeps what we knew: a turn
 * ending goes idle first, then to "needs you".
 */
function isRunning(s: Session, was: boolean): boolean {
  if (s.status === "needs_input") return was;
  return s.status === "working" || s.status === "monitoring";
}
