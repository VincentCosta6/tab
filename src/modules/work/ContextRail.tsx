import { useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Brand } from "../../design/brands";
import { cx } from "../../design/components";
import { brandFor } from "../../design/brands";
import { FilePreview } from "../../design/FileStage";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { useItemContext } from "../../lib/hooks";
import { parseLink } from "../../lib/links";
import { useStage } from "../../lib/stage";
import { keys } from "../../lib/query";
import type { ContextEntry, QueueItem } from "../../lib/types";
import { EntryCard, IconButton, ItemCard, sameLink } from "./ContextDrawer";

/** Files the rail shows before "Show more": the newest. */
const RAIL_FILES = 2;

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
  const stage = useStage();
  const [foundOpen, setFoundOpen] = useState(false);
  const [allFiles, setAllFiles] = useState(false);
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

  const found = (ctx?.entries ?? []).filter((e) => e.section === "found" && e.ref_item?.id !== item.id && !(e.url && item.url && sameLink(e.url, item.url)));
  const related = (ctx?.related ?? []).filter((r) => r.id !== item.id);
  const foundCount = found.length + related.length;
  const files = stage?.files ?? [];
  if (!groups.length && !foundCount && !files.length) return null;
  const refresh = () => qc.invalidateQueries({ queryKey: keys.context(item.id) });
  const pin = async (e: ContextEntry) => (await api.contextUpdate(e.id, { pinned: !e.pinned }), refresh());
  const remove = async (e: ContextEntry) => (await api.contextRemove(e.id), refresh());
  const promote = async (e: ContextEntry) => (await api.contextUpdate(e.id, { section: "context" }), refresh());
  const attach = async (r: QueueItem) => (await api.contextAdd(item.id, { section: "context", ref_item_id: r.id, title: r.title, added_by: "user" }), refresh());

  return (
    <aside className="ctx-rail" aria-label="Context">
      {stage && files.length > 0 && (
        <section className="ctx-rail-group">
          <h4 className="ctx-rail-head">
            <span className="ctx-pill-mark"><Icon name="files" size={13} /></span>
            Files
            <span className="ctx-count">{files.length}</span>
          </h4>
          <div className="rail-files">
            {files.map((f, i) => (allFiles || i >= files.length - RAIL_FILES) && (
              <button key={f.path} className={cx("rail-file", stage.updated.has(f.path) && "fresh")} title={f.path} onClick={() => stage.open(files, i)}>
                <FilePreview path={f.path} size="sm" />
                <span className="rail-file-text">
                  <span className="rail-file-name">{f.name}</span>
                  <span className="subtle">{stage.updated.has(f.path) ? "Updated just now" : f.original ? "Edited by the agent" : f.from === "you" ? "You attached" : "From the agent"}</span>
                </span>
              </button>
            ))}
          </div>
          {files.length > RAIL_FILES && (
            <button className="rail-files-more" aria-expanded={allFiles} onClick={() => setAllFiles((v) => !v)}>
              {allFiles ? "Show less" : `Show ${files.length - RAIL_FILES} more`}
            </button>
          )}
        </section>
      )}
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
      {foundCount > 0 && (
        <section className="ctx-rail-group ctx-rail-found">
          <button className="ctx-rail-head ctx-rail-toggle" aria-expanded={foundOpen} onClick={() => setFoundOpen((v) => !v)}>
            <Icon name="chevron-down" size={12} className="ctx-chevron" />
            Found
            <span className="ctx-count">{foundCount}</span>
            <span className="subtle">may be relevant</span>
          </button>
          {foundOpen && (
            <div className="cc-list">
              {found.map((e) => (
                <EntryCard key={e.id} entry={e} onSelect={onSelect}>
                  <IconButton icon="plus" label="Add to context" onClick={() => promote(e)} />
                  <IconButton icon="x" label="Dismiss" onClick={() => remove(e)} />
                </EntryCard>
              ))}
              {related.map((r) => (
                <ItemCard key={r.id} item={r} note="Related" onOpen={() => onSelect(r.id)}>
                  <IconButton icon="plus" label="Add to context" onClick={() => attach(r)} />
                </ItemCard>
              ))}
            </div>
          )}
        </section>
      )}
    </aside>
  );
}
