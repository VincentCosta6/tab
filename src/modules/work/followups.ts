import type { QueueItem } from "../../lib/types";

/**
 * Puts each task after the tasks it waits on that are in the same list,
 * otherwise keeping the list's order. Loops can't be stored, but anything
 * left over keeps its place at the end.
 */
export function afterBlockers(items: QueueItem[]): QueueItem[] {
  const here = new Set(items.map((i) => i.id));
  const placed = new Set<string>();
  const out: QueueItem[] = [];
  let rest = items;
  while (rest.length) {
    const next = rest.find((i) => (i.blocked_by ?? []).every((b) => !here.has(b) || placed.has(b)));
    if (!next) return [...out, ...rest];
    out.push(next);
    placed.add(next.id);
    rest = rest.filter((i) => i !== next);
  }
  return out;
}

/**
 * Splits the queue into top-level rows and the follow-ups shown under them.
 * A follow-up nests only while its parent is in the list; otherwise it is a
 * row of its own. Follow-ups keep the order they were made in, after the
 * ones they wait on.
 */
export function nest(items: QueueItem[]): { top: QueueItem[]; children: Map<string, QueueItem[]> } {
  const ids = new Set(items.map((i) => i.id));
  const children = new Map<string, QueueItem[]>();
  const top: QueueItem[] = [];
  for (const i of items) {
    if (i.parent_id && ids.has(i.parent_id)) children.set(i.parent_id, [...(children.get(i.parent_id) ?? []), i]);
    else top.push(i);
  }
  for (const [p, c] of children) children.set(p, afterBlockers([...c].sort((a, b) => a.created_at.localeCompare(b.created_at))));
  return { top, children };
}
