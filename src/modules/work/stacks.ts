import type { QueueItem, Urgency } from "../../lib/types";

/** Fewer than this many stay as rows of their own. */
export const STACK_MIN = 3;

/** Items of one kind that share an entity, shown as one row. */
export interface Stack {
  /** `stack:<kind>|<container key>`, also what's selected when the stack is. */
  id: string;
  kind: string;
  /** The shared container's label ("mannaform-web"). */
  label: string;
  /** In queue order. */
  items: QueueItem[];
  /** The newest member, for the row's subtitle and time. */
  newest: QueueItem;
}

export const isStackId = (id: string | null): id is string => !!id?.startsWith("stack:");

const urgencyRank: Record<Urgency, number> = { now: 0, today: 1, this_week: 2, whenever: 3 };
const rank = (i: QueueItem) => urgencyRank[i.ranking?.urgency ?? "today"];

/**
 * Folds top-level rows into stacks where their kind says to (`stack` on the
 * kind), by the container they share upstream. A stack takes the place of
 * its most urgent member, the lead, so it ranks and groups like that one;
 * `stacks` is keyed by the lead's id. Other rows keep their order.
 */
export function stack(top: QueueItem[], stacks: (kind: string) => boolean): { top: QueueItem[]; stacks: Map<string, Stack> } {
  const groups = new Map<string, { kind: string; label: string; items: QueueItem[] }>();
  for (const i of top) {
    const e = stacks(i.kind) && i.entities.find((x) => x.rel === "container");
    if (!e) continue;
    const id = `stack:${i.kind}|${e.key}`;
    const g = groups.get(id) ?? { kind: i.kind, label: e.label, items: [] };
    g.items.push(i);
    groups.set(id, g);
  }
  const out = new Map<string, Stack>();
  const folded = new Set<string>();
  for (const [id, g] of groups) {
    if (g.items.length < STACK_MIN) continue;
    const lead = g.items.reduce((a, b) => (rank(b) < rank(a) ? b : a));
    const newest = g.items.reduce((a, b) => (b.occurred_at > a.occurred_at ? b : a));
    out.set(lead.id, { id, kind: g.kind, label: g.label, items: g.items, newest });
    for (const i of g.items) if (i !== lead) folded.add(i.id);
  }
  return { top: out.size ? top.filter((i) => !folded.has(i.id)) : top, stacks: out };
}
