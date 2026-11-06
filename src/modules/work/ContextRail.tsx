import { useMemo } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Brand } from "../../design/brands";
import { brandFor } from "../../design/brands";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { useItemContext } from "../../lib/hooks";
import { parseLink } from "../../lib/links";
import { keys } from "../../lib/query";
import type { ContextEntry, QueueItem } from "../../lib/types";
import { EntryCard, IconButton, sameLink } from "./ContextDrawer";

const groupNames: Record<string, string> = {
  github: "GitHub", slack: "Slack", gmail: "Email", zoom: "Meetings", jira: "Jira", linear: "Linear", tab: "tab",
};

/** The connector an entry belongs to: the link's module, the queue item's source, else the open web. */
function connectorOf(e: ContextEntry): string {
  const link = parseLink(e.url ?? e.ref_item?.url);
  const kind = link?.kind ?? e.ref_item?.kind ?? "";
  return brandFor(kind, link?.source ?? e.ref_item?.source) ?? "web";
}

/**
 * Beside the chat: the entries given to the agent as Context, as rich cards
 * grouped by connector (GitHub, Slack, Email, ...). Pinned first within a
 * group. Shows only on wide windows; narrow ones keep the pills in the bar.
 */
export function ContextRail({ item, onSelect }: { item: QueueItem; onSelect: (id: string) => void }) {
  const qc = useQueryClient();
  const { data: ctx } = useItemContext(item.id);
  const groups = useMemo(() => {
    const own = (e: ContextEntry) => e.ref_item?.id === item.id || (!!e.url && !!item.url && sameLink(e.url, item.url));
    const entries = (ctx?.entries ?? []).filter((e) => e.section === "context" && !own(e));
    const by = new Map<string, ContextEntry[]>();
    for (const e of entries) {
      const key = connectorOf(e);
      by.set(key, [...(by.get(key) ?? []), e]);
    }
    for (const list of by.values()) list.sort((a, b) => Number(b.pinned) - Number(a.pinned));
    // Groups holding a pinned entry lead, then by size.
    return [...by.entries()].sort(([, a], [, b]) => Number(b.some((e) => e.pinned)) - Number(a.some((e) => e.pinned)) || b.length - a.length);
  }, [ctx, item.id, item.url]);

  if (!groups.length) return null;
  const refresh = () => qc.invalidateQueries({ queryKey: keys.context(item.id) });
  const pin = async (e: ContextEntry) => (await api.contextUpdate(e.id, { pinned: !e.pinned }), refresh());
  const remove = async (e: ContextEntry) => (await api.contextRemove(e.id), refresh());

  return (
    <aside className="ctx-rail" aria-label="Context">
      {groups.map(([key, list]) => (
        <section key={key} className="ctx-rail-group">
          <h4 className="ctx-rail-head">
            <span className="ctx-pill-mark">{key === "web" ? <Icon name="link" size={13} /> : <Brand name={key} size={14} />}</span>
            {groupNames[key] ?? (key === "web" ? "Links" : key)}
            <span className="ctx-count">{list.length}</span>
          </h4>
          <div className="cc-list">
            {list.map((e) => (
              <EntryCard key={e.id} entry={e} onSelect={onSelect}>
                <IconButton icon="pin" label={e.pinned ? "Unpin" : "Pin"} active={e.pinned} onClick={() => pin(e)} />
                <IconButton icon="x" label="Remove" onClick={() => remove(e)} />
              </EntryCard>
            ))}
          </div>
        </section>
      ))}
    </aside>
  );
}
