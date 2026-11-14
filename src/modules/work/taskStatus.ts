// One status per task for its queue row: what the agent is doing, whether
// it has a reply you haven't read, and where the things in its Context stand
// (a PR in review, merged...). The first that applies wins:
//
//   1. the agent is working, needs you, monitoring or failed
//   2. something in Context is waiting on you ("Changes requested")
//   3. the agent finished a turn you haven't opened ("Ready for review")
//   4. something in Context is waiting on others ("In review")
//   5. everything in Context is finished ("Merged")
//   6. idle

import { useEffect, useSyncExternalStore } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { keys } from "../../lib/query";
import type { AgentActivity, ContextStatus, SessionStatus, Tone } from "../../lib/types";

// --- Which agent replies you've seen -------------------------------------
// Per device, in localStorage: item id → the chat's updated_at when you last
// had it open. Replies from before this shipped count as seen.

const SEEN_KEY = "tab.chat-seen";
const SINCE_KEY = "tab.chat-seen-since";

function load(): Record<string, string> {
  try { return JSON.parse(localStorage.getItem(SEEN_KEY) ?? "{}") ?? {}; } catch { return {}; }
}

const since = (() => {
  try {
    const v = localStorage.getItem(SINCE_KEY);
    if (v) return v;
    const now = new Date().toISOString();
    localStorage.setItem(SINCE_KEY, now);
    return now;
  } catch { return new Date().toISOString(); }
})();

let seen = load();
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((l) => l());

const seenStore = {
  subscribe(l: () => void) {
    listeners.add(l);
    // Another window marked something seen.
    const onStorage = (e: StorageEvent) => { if (e.key === SEEN_KEY) (seen = load(), l()); };
    window.addEventListener("storage", onStorage);
    return () => (listeners.delete(l), window.removeEventListener("storage", onStorage));
  },
  get: () => seen,
};

export function markSeen(itemId: string, at: string) {
  if (seen[itemId] && seen[itemId] >= at) return;
  seen = { ...seen, [itemId]: at };
  // Keep the newest 500 so this never grows without bound.
  const entries = Object.entries(seen);
  if (entries.length > 500) seen = Object.fromEntries(entries.sort((a, b) => b[1].localeCompare(a[1])).slice(0, 500));
  try { localStorage.setItem(SEEN_KEY, JSON.stringify(seen)); } catch { /* not remembered */ }
  emit();
}

export const useSeen = () => useSyncExternalStore(seenStore.subscribe, seenStore.get);

/** True when the agent finished a turn after you last had this chat open. */
export function unread(a: AgentActivity | undefined, seenMap: Record<string, string>): boolean {
  if (!a || a.status !== "idle") return false;
  const last = seenMap[a.item_id] ?? since;
  return a.updated_at > last;
}

/** Marks the open item's latest reply seen while the window has focus. */
export function useMarkSeen(itemId: string | null, a: AgentActivity | undefined) {
  const at = a?.status === "idle" ? a.updated_at : null;
  useEffect(() => {
    if (!itemId || !at) return;
    const mark = () => { if (document.hasFocus()) markSeen(itemId, at); };
    mark();
    window.addEventListener("focus", mark);
    return () => window.removeEventListener("focus", mark);
  }, [itemId, at]);
}

// --- Context statuses -----------------------------------------------------

/** Where each item's Context stands, for the given items, kept fresh. */
export function useContextStatus(ids: string[]): Map<string, ContextStatus[]> {
  const sorted = [...ids].sort();
  const { data = {} } = useQuery({
    queryKey: keys.contextStatus(sorted),
    queryFn: () => api.contextStatus(sorted),
    enabled: sorted.length > 0,
    staleTime: 30_000,
    refetchInterval: 60_000,
    placeholderData: (prev) => prev,
  });
  return new Map(Object.entries(data));
}

// --- The row's one status -------------------------------------------------

export interface TaskStatus {
  /** Drives the pill's animation; `ready` and `context` are still. */
  kind: SessionStatus | "ready" | "context";
  label: string;
  tone: Tone;
  title: string;
}

const agentLook: Record<SessionStatus, { label: string; tone: Tone }> = {
  working: { label: "Working", tone: "info" },
  needs_input: { label: "Needs you", tone: "warning" },
  monitoring: { label: "Monitoring", tone: "accent" },
  failed: { label: "Failed", tone: "danger" },
  idle: { label: "Idle", tone: "neutral" },
};

/** Danger before warning before the rest, so the most pressing shows. */
const toneRank: Partial<Record<Tone, number>> = { danger: 0, warning: 1 };
const pressing = (xs: ContextStatus[]) => [...xs].sort((a, b) => (toneRank[a.tone] ?? 2) - (toneRank[b.tone] ?? 2))[0];

function fromContext(c: ContextStatus, more: number): TaskStatus {
  return { kind: "context", label: c.label, tone: c.tone, title: [c.detail ?? c.label, more ? `+${more} more` : null].filter(Boolean).join(" · ") };
}

export function taskStatus(a: AgentActivity | undefined, ctx: ContextStatus[] | undefined, isUnread: boolean): TaskStatus | null {
  const cs = ctx ?? [];
  if (a && a.status !== "idle") {
    const l = agentLook[a.status];
    return { kind: a.status, label: l.label, tone: l.tone, title: `Chat ${l.label.toLowerCase()}` };
  }
  const you = cs.filter((c) => c.waiting_on === "you");
  if (you.length) return fromContext(pressing(you), you.length - 1);
  if (isUnread) return { kind: "ready", label: "Ready for review", tone: "accent", title: "The agent finished and you haven't read its reply yet" };
  const others = cs.filter((c) => c.waiting_on === "others");
  if (others.length) return fromContext(pressing(others), others.length - 1);
  if (cs.length && cs.every((c) => c.finished)) return fromContext(cs[0], cs.length - 1);
  if (a) return { kind: "idle", label: "Idle", tone: "neutral", title: "Chat idle" };
  return null;
}
