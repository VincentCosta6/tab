import type { QueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { keys } from "../../lib/query";
import type { ItemState, QueueItem } from "../../lib/types";
import { confirmDone } from "./doneGuard";

export type ItemAction = "done" | "reopen" | { snooze: number };

/**
 * Items acted on whose change hasn't been saved yet, by the state they're
 * moving to. List reads that land meanwhile (another item's event can
 * trigger one) still have the old state; `hidePending` keeps the row out.
 */
const pending = new Map<string, ItemState>();

/** For `useItems`' select: drops rows that are on their way to another list. */
export function hidePending(state: ItemState) {
  return (items: QueueItem[]) => (pending.size ? items.filter((i) => (pending.get(i.id) ?? state) === state) : items);
}

const target = (a: ItemAction): ItemState => (a === "done" ? "done" : a === "reopen" ? "open" : "snoozed");

/**
 * Done, reopen or snooze, shown at once: the row leaves its list and the
 * pane shows the new state before the engine answers. Done's guard check
 * (it asks connectors, up to a few seconds) runs after; if the person backs
 * out, or the call fails, the caches go back. False when nothing changed.
 */
export async function actOnItem(qc: QueryClient, item: QueueItem, action: ItemAction): Promise<boolean> {
  const state = target(action);
  const lists = qc.getQueriesData<QueueItem[]>({ queryKey: keys.allItems });
  const before = qc.getQueryData<QueueItem>(keys.item(item.id));
  const undo = () => {
    pending.delete(item.id);
    for (const [key, data] of lists) qc.setQueryData(key, data);
    qc.setQueryData(keys.item(item.id), before ?? item);
  };

  pending.set(item.id, state);
  qc.setQueriesData<QueueItem[]>({ queryKey: keys.allItems }, (xs) => xs?.filter((x) => x.id !== item.id));
  qc.setQueryData<QueueItem>(keys.item(item.id), { ...(before ?? item), state });

  try {
    if (action === "done") {
      if (!(await confirmDone(item))) return (undo(), false);
      await api.markDone(item.id);
    } else if (action === "reopen") await api.reopen(item.id);
    else await api.snooze(item.id, action.snooze);
    return true;
  } catch (e) {
    undo();
    throw e;
  } finally {
    // The engine's item_changed event refreshes everything; this covers a
    // change that emitted none.
    if (pending.get(item.id) === state) pending.delete(item.id);
    qc.invalidateQueries({ queryKey: keys.allItems });
  }
}
