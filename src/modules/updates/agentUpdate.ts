// The agent CLIs' updates: whether a harness is behind, and running its
// update command. Settings, the nav badge and the new-chat prompt share
// this, so an update started in one shows its progress in the others.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useSyncExternalStore } from "react";
import { api, onAgentUpgrade } from "../../lib/api";
import type { AgentKind } from "../../lib/types";
import { HOUR } from "./appUpdate";

export const agentName: Record<string, string> = { claude_code: "Claude Code", opencode: "OpenCode", codex: "Codex", antigravity: "Antigravity CLI", pi: "Pi" };

/** "2.1.91 (Claude Code)" → [2, 1, 91]. */
function parts(v: string | null | undefined): number[] | null {
  const m = v?.match(/\d+(?:\.\d+)+/);
  return m ? m[0].split(".").map(Number) : null;
}

/** "2.1.91 (Claude Code)" → "2.1.91". */
export const version = (v: string | null | undefined) => parts(v)?.join(".") ?? v ?? null;

export function isOlder(have: string | null | undefined, latest: string | null | undefined) {
  const a = parts(have), b = parts(latest);
  if (!a || !b) return false;
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    if ((a[i] ?? 0) !== (b[i] ?? 0)) return (a[i] ?? 0) < (b[i] ?? 0);
  }
  return false;
}

export type UpgradeState =
  | { phase: "idle" }
  | { phase: "running"; lines: string[] }
  | { phase: "done" }
  | { phase: "failed"; error: string; lines: string[] };

const idle: UpgradeState = { phase: "idle" };
/** Each agent's update, by kind. Updater output isn't tagged with its agent, so one runs at a time. */
const states = new Map<AgentKind, UpgradeState>();
const listeners = new Set<() => void>();
const get = (kind: AgentKind | undefined) => (kind && states.get(kind)) || idle;
const set = (kind: AgentKind, next: UpgradeState) => {
  states.set(kind, next);
  listeners.forEach((l) => l());
};
const running = () => [...states.values()].some((s) => s.phase === "running");

/** An agent (the default one without `kind`), whether a newer release is out, and updating it. */
export function useAgentUpdate(kind?: AgentKind, enabled = true) {
  const qc = useQueryClient();
  const health = useQuery({ queryKey: ["agent-health", kind ?? "default"], queryFn: () => api.agentHealth(kind), staleTime: 30 * 60_000, refetchInterval: HOUR, retry: false, enabled });
  const h = health.data;
  const which = kind ?? h?.kind;
  const upgrade = useSyncExternalStore(
    (l) => (listeners.add(l), () => void listeners.delete(l)),
    () => get(which),
  );
  const behind = !!h?.found && !!h.latest && isOlder(h.version, h.latest);
  const name = agentName[which ?? "claude_code"] ?? "The agent";

  const update = async () => {
    if (!which || running()) return;
    let lines: string[] = [];
    set(which, { phase: "running", lines });
    const off = await onAgentUpgrade((l) => {
      lines = [...lines.slice(-199), l];
      if (get(which).phase === "running") set(which, { phase: "running", lines });
    });
    try {
      await api.agentUpgrade(which);
      await qc.refetchQueries({ queryKey: ["agent-health"] });
      set(which, { phase: "done" });
      setTimeout(() => get(which).phase === "done" && set(which, idle), 5000);
    } catch (e) {
      set(which, { phase: "failed", error: String((e as { message?: string })?.message ?? e), lines });
    } finally {
      off();
    }
  };

  return { health, behind, name, have: version(h?.version), latest: version(h?.latest), upgrade, update };
}
