import type { QueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { keys } from "../../lib/query";
import type { AgentActivity, LinkSummary, QueueItem, SessionStatus } from "../../lib/types";
import { parsePullUrl } from "../github/pulls";

const busy: Partial<Record<SessionStatus, string>> = {
  working: "is still working",
  needs_input: "is waiting on you",
  monitoring: "is still monitoring",
};

/** Resolves to null if the lookup is slow, so Done never hangs on the network. */
const within = <T,>(p: Promise<T>, ms = 2500): Promise<T | null> =>
  Promise.race([p.catch(() => null), new Promise<null>((r) => setTimeout(() => r(null), ms))]);

/** What's left on an open PR, e.g. "2 unresolved threads, checks failing". */
function prNotes(s: LinkSummary): string {
  const notes = [
    s.state === "draft" && "draft",
    s.open_threads ? `${s.open_threads} unresolved thread${s.open_threads === 1 ? "" : "s"}` : null,
    s.review_decision?.toLowerCase() === "changes_requested" && "changes requested",
    s.checks === "failing" && "checks failing",
  ].filter(Boolean);
  return notes.length ? ` (${notes.join(", ")})` : "";
}

/**
 * Why marking this item done might be premature: an agent still busy on it,
 * or an open PR in its Context. Empty when it's safe.
 */
export async function doneWarnings(item: QueueItem, qc: QueryClient): Promise<string[]> {
  const cached = qc.getQueryData<AgentActivity[]>(keys.agentActivity);
  const [activity, ctx] = await Promise.all([cached ? Promise.resolve(cached) : within(api.agentActivity()), within(api.itemContext(item.id))]);
  const out: string[] = [];

  const a = activity?.find((x) => x.item_id === item.id);
  const why = a && busy[a.status];
  if (why) out.push(`The agent ${why}.`);

  const self = parsePullUrl(item.url);
  const pulls = new Map<string, { repo: string; number: number }>();
  for (const e of ctx?.entries ?? []) {
    if (e.section !== "context") continue;
    const p = parsePullUrl(e.url ?? e.ref_item?.url);
    // The item itself being a PR is what Done is for; only flag PRs linked to it.
    if (p && e.ref_item?.id !== item.id && !(self?.repo === p.repo && self.number === p.number)) pulls.set(`${p.repo}#${p.number}`, p);
  }
  const summaries = await Promise.all([...pulls].map(async ([key, p]) => [key, await within(api.githubLinkSummary(p.repo, "pull", String(p.number)))] as const));
  for (const [key, s] of summaries) if (s && (s.state === "open" || s.state === "draft")) out.push(`${key} is still open${prNotes(s)}.`);

  return out;
}

/** Asks before marking done when something is still in flight. True to go ahead. */
export async function confirmDone(item: QueueItem, qc: QueryClient): Promise<boolean> {
  const warnings = await doneWarnings(item, qc);
  if (!warnings.length) return true;
  return window.confirm(`Mark "${item.title}" done?\n\n${warnings.map((w) => `• ${w}`).join("\n")}`);
}
