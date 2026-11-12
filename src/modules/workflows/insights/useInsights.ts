// The window (7 or 30 days) the Factory, Insights and canvas badges share,
// remembered per person, and the queries behind them.

import { useSyncExternalStore } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../../lib/api";

export type WindowDays = 7 | 30;

const KEY = "tab.workflows.insights.days";
let days: WindowDays = (() => {
  try { return localStorage.getItem(KEY) === "30" ? 30 : 7; } catch { return 7; }
})();
const listeners = new Set<() => void>();

export function setWindowDays(d: WindowDays) {
  days = d;
  try { localStorage.setItem(KEY, String(d)); } catch { /* private window */ }
  listeners.forEach((l) => l());
}

export function useWindowDays(): WindowDays {
  return useSyncExternalStore((l) => { listeners.add(l); return () => listeners.delete(l); }, () => days);
}

export function useWorkflowInsights(workflowId: string | null | undefined) {
  const d = useWindowDays();
  return useQuery({
    queryKey: ["workflow-insights", workflowId, d],
    queryFn: () => api.workflowInsights(workflowId!, d),
    enabled: !!workflowId,
    staleTime: 30_000,
    placeholderData: (p) => (p?.workflow_id === workflowId ? p : undefined),
  });
}

export function useFactory() {
  const d = useWindowDays();
  return useQuery({ queryKey: ["workflow-factory", d], queryFn: () => api.workflowFactory(d), staleTime: 30_000, placeholderData: (p) => p });
}
