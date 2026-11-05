import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Badge, Button, ScorePill, cx } from "../../design/components";
import { Blocks } from "../../design/blocks";
import { SourceGlyph } from "../../design/brands";
import { Icon } from "../../design/icons";
import { api, openUrl } from "../../lib/api";
import { useApp } from "../../lib/app-context";
import { timeAgo } from "../../lib/format";
import { useItemContext, usePreview } from "../../lib/hooks";
import { keys } from "../../lib/query";
import type { ContextEntry, QueueItem } from "../../lib/types";

/**
 * Everything around an item, rolled up into one bar above the chat. Open it
 * to see rich previews: the item itself, what's attached as Context (what the
 * agent is given, pinned first), and what's been Found (by the agent or the
 * knowledge graph), which can be promoted into Context.
 */
export function ContextDrawer({ item, onSelect }: { item: QueueItem; onSelect: (id: string) => void }) {
  const qc = useQueryClient();
  const { data: ctx } = useItemContext(item.id);
  const [open, setOpen] = useState(false);
  const entries = ctx?.entries ?? [];
  const context = entries.filter((e) => e.section === "context").sort((a, b) => Number(b.pinned) - Number(a.pinned));
  const found = entries.filter((e) => e.section === "found");
  const related = ctx?.related ?? [];
  const foundCount = found.length + related.length;
  const agentFound = found.filter((f) => f.added_by === "agent").length;

  const refresh = () => qc.invalidateQueries({ queryKey: keys.context(item.id) });
  const promote = async (e: ContextEntry) => (await api.contextUpdate(e.id, { section: "context" }), refresh());
  const pin = async (e: ContextEntry) => (await api.contextUpdate(e.id, { pinned: !e.pinned }), refresh());
  const remove = async (e: ContextEntry) => (await api.contextRemove(e.id), refresh());
  const attach = async (r: QueueItem) => (
    await api.contextAdd(item.id, { section: "context", ref_item_id: r.id, title: r.title, added_by: "user" }), refresh()
  );

  return (
    <div className={cx("ctx", open && "open")}>
      <button className="ctx-bar" onClick={() => setOpen((v) => !v)} aria-expanded={open}>
        <Icon name="graph" size={14} />
        <span className="ctx-label">Context</span>
        <span className="ctx-count">{context.length + 1}</span>
        <span className="ctx-chips">
          <span className="ctx-chip"><Icon name="dot" size={10} />This item</span>
          {context.slice(0, 3).map((c) => (
            <span key={c.id} className="ctx-chip" title={c.title}>
              {c.pinned && <Icon name="pin" size={10} />}
              {c.title}
            </span>
          ))}
          {context.length > 3 && <span className="ctx-chip">+{context.length - 3}</span>}
        </span>
        <span className="ctx-found">
          Found <span className={cx("ctx-count", agentFound > 0 && "fresh")}>{foundCount}</span>
        </span>
        <Icon name="chevron-down" size={14} className="ctx-chevron" />
      </button>
      {open && (
        <div className="ctx-panel">
          <section>
            <h4 className="label-sm">Context <span className="subtle">· given to the agent with every message</span></h4>
            <ThisItem item={item} />
            {context.map((e) => (
              <EntryCard key={e.id} entry={e} onSelect={onSelect}>
                <IconButton icon="pin" label={e.pinned ? "Unpin" : "Pin"} active={e.pinned} onClick={() => pin(e)} />
                <IconButton icon="x" label="Remove" onClick={() => remove(e)} />
              </EntryCard>
            ))}
            {!context.length && <p className="subtle ctx-empty">Promote something from Found to give the agent more to work with, or ask it to attach a ticket or PR.</p>}
          </section>
          <section>
            <h4 className="label-sm">Found <span className="subtle">· from the agent and the knowledge graph</span></h4>
            {!foundCount && <p className="subtle ctx-empty">Nothing yet. The agent files what it finds here as it works.</p>}
            {found.map((e) => (
              <EntryCard key={e.id} entry={e} onSelect={onSelect}>
                <Button size="sm" icon="plus" onClick={() => promote(e)}>Add to context</Button>
                <IconButton icon="x" label="Dismiss" onClick={() => remove(e)} />
              </EntryCard>
            ))}
            {related.map((r) => (
              <div key={r.id} className="ctx-entry">
                <RefItem item={r} onSelect={onSelect} by="graph" />
                <div className="ctx-entry-actions">
                  <Button size="sm" icon="plus" onClick={() => attach(r)}>Add to context</Button>
                </div>
              </div>
            ))}
          </section>
        </div>
      )}
    </div>
  );
}

function IconButton({ icon, label, onClick, active }: { icon: string; label: string; onClick: () => void; active?: boolean }) {
  return (
    <button className={cx("icon-btn", active && "active")} title={label} aria-label={label} onClick={onClick}>
      <Icon name={icon} size={14} />
    </button>
  );
}

function ThisItem({ item }: { item: QueueItem }) {
  const { data: preview } = usePreview(item.id);
  const [full, setFull] = useState(false);
  const r = item.ranking;
  return (
    <div className="ctx-this">
      <div className="subtle">This item · always included</div>
      {r && (
        <div className="why">
          <div className="why-head">
            <ScorePill score={r.score} urgency={r.urgency} />
            <span className="why-reason">{r.reason}</span>
          </div>
          <div className="why-math">
            <span>Rules {r.base_score}</span>
            {r.ai_adjustment !== null && <span>AI {r.ai_adjustment >= 0 ? "+" : ""}{r.ai_adjustment}</span>}
            <span>via {r.ranked_by}</span>
          </div>
        </div>
      )}
      {item.blocks.length > 0 && <Blocks blocks={item.blocks} />}
      {full && preview && preview.blocks.length > 0 && <Blocks blocks={preview.blocks} />}
      {preview && preview.blocks.length > 0 && (
        <button className="link-btn subtle ctx-this-toggle" onClick={() => setFull((v) => !v)}>{full ? "Hide rich preview" : "Show rich preview"}</button>
      )}
      {full && preview?.fetched_at && (
        <div className="subtle cache-note">
          <Icon name="clock" size={11} /> Preview cached {timeAgo(preview.fetched_at)}
          {preview.stale && " · refreshing when rate limits allow"}
        </div>
      )}
      {item.entities.length > 0 && (
        <div className="entities">
          {item.entities.map((e) => (
            <Badge key={`${e.kind}:${e.key}:${e.rel}`} title={`${e.rel} · ${e.key}`}>{e.label}</Badge>
          ))}
        </div>
      )}
    </div>
  );
}

function EntryCard({ entry: e, onSelect, children }: { entry: ContextEntry; onSelect: (id: string) => void; children: React.ReactNode }) {
  return (
    <div className={cx("ctx-entry", e.pinned && "pinned")}>
      {e.ref_item ? (
        <RefItem item={e.ref_item} onSelect={onSelect} by={e.added_by} summary={e.summary} />
      ) : (
        <div className="ctx-link">
          <span className="source-glyph tone-neutral" style={{ width: 24, height: 24 }}><Icon name="link" size={13} /></span>
          <div className="ctx-link-main">
            <button className="link-btn" onClick={() => e.url && openUrl(e.url)}>{e.title}</button>
            {e.summary && <p className="ctx-summary">{e.summary}</p>}
            <span className="subtle">{e.url} · added by {e.added_by}</span>
          </div>
        </div>
      )}
      <div className="ctx-entry-actions">{children}</div>
    </div>
  );
}

/** A queue item used as context, with its cached rich preview. */
function RefItem({ item, onSelect, by, summary }: { item: QueueItem; onSelect: (id: string) => void; by: string; summary?: string | null }) {
  const { kind } = useApp();
  const k = kind(item.kind);
  const [expanded, setExpanded] = useState(false);
  const { data: preview } = usePreview(expanded ? item.id : null);
  return (
    <div className="ctx-ref">
      <SourceGlyph kind={item.kind} source={item.source} icon={k.icon} tone={k.tone} size={24} />
      <div className="ctx-ref-main">
        <button className="link-btn" onClick={() => onSelect(item.id)} title="Open this item">{item.title}</button>
        <div className="subtle">{[item.actor, item.context, `added by ${by}`].filter(Boolean).join(" · ")}</div>
        {summary && <p className="ctx-summary">{summary}</p>}
        {!expanded && item.body && <p className="ctx-snippet">{item.body}</p>}
        {expanded && (
          <div className="ctx-preview">
            {item.blocks.length > 0 && <Blocks blocks={item.blocks} />}
            {preview?.blocks && preview.blocks.length > 0 && <Blocks blocks={preview.blocks} />}
          </div>
        )}
        <button className="link-btn subtle" onClick={() => setExpanded((v) => !v)}>{expanded ? "Less" : "Preview"}</button>
      </div>
    </div>
  );
}
