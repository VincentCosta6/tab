// The agent CLI's updates: whether the configured harness is behind, and
// running its update command. Settings, the nav badge and the new-chat
// prompt share this, so an update started in one shows its progress in the
// others.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useSyncExternalStore } from "react";
import { api, onAgentUpgrade } from "../../lib/api";
import { HOUR } from "./appUpdate";

export const agentName: Record<string, string> = { claude_code: "Claude Code", opencode: "OpenCode", codex: "Codex", pi: "Pi" };

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

let state: UpgradeState = { phase: "idle" };
const listeners = new Set<() => void>();
const set = (next: UpgradeState) => {
  state = next;
  listeners.forEach((l) => l());
};

/** The configured agent, whether a newer release is out, and updating it. */
export function useAgentUpdate() {
  const qc = useQueryClient();
  const health = useQuery({ queryKey: ["agent-health"], queryFn: api.agentHealth, staleTime: 30 * 60_000, refetchInterval: HOUR, retry: false });
  const upgrade = useSyncExternalStore(
    (l) => (listeners.add(l), () => void listeners.delete(l)),
    () => state,
  );
  const h = health.data;
  const behind = !!h?.found && !!h.latest && isOlder(h.version, h.latest);
  const name = agentName[h?.kind ?? "claude_code"] ?? "The agent";

  const update = async () => {
    if (state.phase === "running") return;
    let lines: string[] = [];
    set({ phase: "running", lines });
    const off = await onAgentUpgrade((l) => {
      lines = [...lines.slice(-199), l];
      if (state.phase === "running") set({ phase: "running", lines });
    });
    try {
      await api.agentUpgrade();
      await qc.refetchQueries({ queryKey: ["agent-health"] });
      set({ phase: "done" });
      setTimeout(() => state.phase === "done" && set({ phase: "idle" }), 5000);
    } catch (e) {
      set({ phase: "failed", error: String((e as { message?: string })?.message ?? e), lines });
    } finally {
      off();
    }
  };

  return { health, behind, name, have: version(h?.version), latest: version(h?.latest), upgrade, update };
}
