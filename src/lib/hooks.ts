// Query hooks. Components read through these so caching rules live in one place.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "./api";
import { keys } from "./query";
import { hidePending } from "../modules/work/optimistic";
import type { AgentActivity, Chat, ItemState, TabConfig } from "./types";

/** Why `path` can't be a work folder, checked as the person types. */
export function useFolderProblem(path: string | null | undefined) {
  const p = (path ?? "").trim();
  const { data } = useQuery({
    queryKey: ["folder-problem", p],
    queryFn: () => api.folderProblem(p),
    enabled: !!p,
    staleTime: 2000,
  });
  return p ? data ?? null : null;
}

export function useItems(state: ItemState, query: string) {
  return useQuery({
    queryKey: keys.items(state, query.trim()),
    queryFn: () => (query.trim() ? api.searchItems(query, [state]) : api.listItems([state])),
    placeholderData: (prev) => prev,
    // Rows just marked done (or snoozed, reopened) stay out until it's saved.
    select: hidePending(state),
  });
}

export function useItem(id: string | null) {
  return useQuery({
    queryKey: keys.item(id ?? ""),
    queryFn: async () => (await api.itemDetail(id!))?.item ?? null,
    enabled: !!id,
  });
}

/**
 * Rich preview. Served from cache (memory, then localStorage, then the
 * backend's SQLite cache). The backend refreshes stale previews in the
 * background when rate limits allow and pushes them as events, so this
 * never refetches on its own.
 */
export function usePreview(id: string | null | undefined) {
  return useQuery({
    queryKey: keys.preview(id ?? ""),
    queryFn: () => api.itemPreview(id!),
    enabled: !!id,
  });
}

export function useItemContext(id: string | null) {
  return useQuery({ queryKey: keys.context(id ?? ""), queryFn: () => api.itemContext(id!), enabled: !!id });
}

export function useChat(itemId: string | null) {
  return useQuery<Chat>({
    queryKey: keys.chat(itemId ?? ""),
    queryFn: () => api.chatOpen(itemId!),
    enabled: !!itemId,
    // Sessions change while the app is closed only if an agent was killed;
    // reload once per launch to pick that up.
    staleTime: 0,
    refetchOnMount: "always",
  });
}

export function useStream(sessionId: string | null | undefined) {
  return useQuery({ queryKey: keys.stream(sessionId ?? ""), queryFn: () => "", enabled: false, initialData: "" }).data;
}

export function useConnectors() {
  return useQuery({ queryKey: keys.connectors, queryFn: api.listConnectors, staleTime: 0, refetchOnMount: "always" });
}

export function useConfig() {
  return useQuery({ queryKey: keys.config, queryFn: api.getConfig, staleTime: 0, refetchOnMount: "always" });
}

export function useSaveConfig() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (c: TabConfig) => api.saveConfig(c),
    onSuccess: async (saved) => {
      qc.setQueryData(keys.config, saved);
      await qc.invalidateQueries({ queryKey: keys.connectors });
      // Turning a connection off removes its items.
      await qc.invalidateQueries({ queryKey: keys.allItems });
    },
  });
}

/** Busy, waiting or failed agents by item. Kept live by session events. */
export function useAgentActivity(): Map<string, AgentActivity> {
  const { data = [] } = useQuery({ queryKey: keys.agentActivity, queryFn: api.agentActivity, staleTime: 0, refetchInterval: 30_000 });
  return new Map(data.map((a) => [a.item_id, a]));
}

/** Polls quickly while the index is busy, slowly otherwise. */
export function useSearchIndex() {
  return useQuery({
    queryKey: ["search-index"],
    queryFn: api.searchIndexStatus,
    staleTime: 0,
    refetchInterval: (q) => (["downloading", "loading", "indexing"].includes(q.state.data?.state ?? "") ? 800 : 5000),
  });
}

export function useProjects() {
  return useQuery({ queryKey: keys.projects, queryFn: api.projectsList });
}

export function useProjectNotes(projectId: string | null | undefined) {
  return useQuery({ queryKey: keys.notes(projectId ?? ""), queryFn: () => api.projectNotes(projectId!), enabled: !!projectId, staleTime: 0 });
}

export function useProjectRepos(projectId: string | null | undefined) {
  return useQuery({ queryKey: keys.repos(projectId ?? ""), queryFn: () => api.projectRepos(projectId!), enabled: !!projectId, staleTime: 0 });
}

export function useProjectResources(projectId: string | null | undefined) {
  return useQuery({ queryKey: keys.resources(projectId ?? ""), queryFn: () => api.projectResources(projectId!), enabled: !!projectId, staleTime: 0 });
}

export function useProjectItems(projectId: string | null | undefined) {
  return useQuery({ queryKey: keys.projectItems(projectId ?? ""), queryFn: () => api.projectItems(projectId!), enabled: !!projectId, staleTime: 0 });
}

export function usePressure() {
  return useQuery({ queryKey: keys.pressure, queryFn: api.ratePressure, refetchInterval: 4000, staleTime: 0 });
}

/** Every harness tab can drive, from its metadata. */
export function useHarnesses() {
  return useQuery({ queryKey: ["harnesses"], queryFn: api.agentHarnesses, staleTime: Infinity });
}

/** The models of every installed harness. */
export function useModels() {
  return useQuery({ queryKey: ["agent-models"], queryFn: api.agentModels, staleTime: 5 * 60_000 });
}
