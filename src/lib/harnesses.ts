import type { AgentKind, Harness } from "./types";

/**
 * Harnesses in the order to offer them: the ones installed on this computer
 * first, then the rest. Each part keeps `harnesses.json` order, which puts
 * Claude Code first and OpenCode second.
 */
export function orderHarnesses(harnesses: Harness[], installed: Set<AgentKind>): Harness[] {
  return [...harnesses.filter((h) => installed.has(h.id)), ...harnesses.filter((h) => !installed.has(h.id))];
}

/** Whether a harness matches what the person typed: its name, command or pitch. */
export function harnessMatches(h: Harness, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [h.name, h.id, h.command, h.pitch].some((s) => s.toLowerCase().includes(q));
}
