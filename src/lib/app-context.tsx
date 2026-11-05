// App-wide data that many modules need: connectors (and so kind metadata).

import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { api, onEngineEvent } from "./api";
import type { ConnectorInfo, KindInfo, Tone } from "./types";

interface AppData {
  connectors: ConnectorInfo[];
  refreshConnectors: () => Promise<void>;
  kind: (kind: string) => KindInfo;
  /** Apps (kind prefixes) that have items, with labels and icons, for filters. */
  apps: { id: string; label: string; icon: string }[];
}

const AppContext = createContext<AppData | null>(null);

const appLabels: Record<string, { label: string; icon: string }> = {
  github: { label: "GitHub", icon: "github" },
  slack: { label: "Slack", icon: "slack" },
  email: { label: "Email", icon: "mail" },
  meeting: { label: "Meetings", icon: "video" },
  agent: { label: "Agents", icon: "bot" },
};

export function AppProvider({ children }: { children: ReactNode }) {
  const [connectors, setConnectors] = useState<ConnectorInfo[]>([]);
  const refreshConnectors = useCallback(async () => setConnectors(await api.listConnectors()), []);

  useEffect(() => {
    refreshConnectors();
    let off: (() => void) | undefined;
    onEngineEvent((e) => {
      if (e.type === "connector_changed")
        setConnectors((cs) => cs.map((c) => (c.id === e.connector.id ? e.connector : c)));
    }).then((u) => (off = u));
    return () => off?.();
  }, [refreshConnectors]);

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
    return { connectors, refreshConnectors, kind, apps };
  }, [connectors, refreshConnectors]);

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

export function useApp(): AppData {
  const v = useContext(AppContext);
  if (!v) throw new Error("useApp outside AppProvider");
  return v;
}
