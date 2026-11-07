import { api } from "../../lib/api";
import type { DoneGuard, QueueItem } from "../../lib/types";

const line = (g: DoneGuard) => `• ${g.title}${g.detail ? ` (${g.detail})` : ""}`;

/**
 * Asks before marking done when tab or a connector says something is still in
 * flight (connectors raise these in `done_guards`). A `block` guard refuses
 * outright. True to go ahead. If the check itself fails, Done goes ahead.
 */
export async function confirmDone(item: QueueItem): Promise<boolean> {
  const guards = await api.doneGuards(item.id).catch(() => [] as DoneGuard[]);
  if (!guards.length) return true;
  const blocks = guards.filter((g) => g.level === "block");
  if (blocks.length) {
    window.alert(`"${item.title}" can't be marked done yet.\n\n${blocks.map(line).join("\n")}`);
    return false;
  }
  return window.confirm(`Mark "${item.title}" done?\n\n${guards.map(line).join("\n")}`);
}
