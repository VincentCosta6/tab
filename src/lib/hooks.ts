// Query hooks. Components read through these so caching rules live in one place.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "./api";
import { keys } from "./query";
import type { Chat, ItemState, TabConfig } from "./types";

export function useItems(state: ItemState, query: string) {
  return useQuery({
    queryKey: keys.items(state, query.trim()),
    queryFn: () => (query.trim() ? api.searchItems(query, [state]) : api.listItems([state])),
    placeholderData: (prev) => prev,
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

export function usePressure() {
  return useQuery({ queryKey: keys.pressure, queryFn: api.ratePressure, refetchInterval: 4000, staleTime: 0 });
}
