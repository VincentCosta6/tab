// App-wide lookups many modules need: connectors and kind metadata.

import { createContext, useContext, useMemo, type ReactNode } from "react";
import { useConnectors } from "./hooks";
import type { ConnectorInfo, KindInfo, Tone } from "./types";

interface AppData {
  connectors: ConnectorInfo[];
  kind: (kind: string) => KindInfo;
  /** Apps (kind prefixes) that have items, with labels and brand marks, for filters. */
  apps: { id: string; label: string; icon: string }[];
}

const AppContext = createContext<AppData | null>(null);

const appLabels: Record<string, { label: string; icon: string }> = {
  tab: { label: "Tasks", icon: "tab" },
  github: { label: "GitHub", icon: "github" },
  slack: { label: "Slack", icon: "slack" },
  email: { label: "Email", icon: "gmail" },
  meeting: { label: "Meetings", icon: "zoom" },
  jira: { label: "Jira", icon: "jira" },
  agent: { label: "Agents", icon: "bot" },
};

export function AppProvider({ children }: { children: ReactNode }) {
  const { data: connectors = [] } = useConnectors();

  const value = useMemo<AppData>(() => {
    const kinds = new Map<string, KindInfo>();
    for (const c of connectors) for (const k of c.kinds) kinds.set(k.kind, k);
    const kind = (k: string): KindInfo => {
      const hit = kinds.get(k);
      if (hit) return hit;
      const app = k.split(".")[0];
      const fallback = appLabels[app];
      const tone: Tone = "neutral";
      return { kind: k, label: k.split(".").slice(1).join(" ").replace(/_/g, " ") || k, icon: fallback?.icon ?? "dot", tone };
    };
    const apps = Object.entries(appLabels).map(([id, v]) => ({ id, ...v }));
    return { connectors, kind, apps };
  }, [connectors]);

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

export function useApp(): AppData {
  const v = useContext(AppContext);
  if (!v) throw new Error("useApp outside AppProvider");
  return v;
}
