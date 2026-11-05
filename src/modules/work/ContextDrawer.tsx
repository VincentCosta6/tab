import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Badge, ScorePill, cx } from "../../design/components";
import { Blocks } from "../../design/blocks";
import { SourceGlyph } from "../../design/brands";
import { Icon } from "../../design/icons";
import { api, openUrl } from "../../lib/api";
import { useApp } from "../../lib/app-context";
import { timeAgo, timeUntil } from "../../lib/format";
import { useItemContext, usePreview } from "../../lib/hooks";
import { keys } from "../../lib/query";
import type { ContextEntry, QueueItem, Tone } from "../../lib/types";

/**
 * Everything around an item, rolled up into one bar above the chat. Open it
 * to see rich previews: the item itself, what's attached as Context (what the
 * agent is given, pinned first), and what's been Found (by the agent or the
 * knowledge graph), which can be promoted into Context.
 */
export function ContextDrawer({ item, onSelect, left, right }: { item: QueueItem; onSelect: (id: string) => void; left?: React.ReactNode; right?: React.ReactNode }) {
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
      <div className="pane-toolbar">
        <div className="pane-toolbar-left">{left}</div>
        <div className="pane-toolbar-right">
          {right}
          <button
            className={cx("ctx-toggle", open && "active")}
            onClick={() => setOpen((v) => !v)}
            aria-expanded={open}
            title={`Context: ${context.length + 1} given to the agent, ${foundCount} found`}
          >
            <Icon name="graph" size={13} />
            Context
            <span className="ctx-count">{context.length + 1}</span>
            {foundCount > 0 && <span className={cx("ctx-count", agentFound > 0 && "fresh")} title="Found">+{foundCount}</span>}
            <Icon name="chevron-down" size={12} className="ctx-chevron" />
          </button>
        </div>
      </div>
      {open && (
        <div className="ctx-panel">
          <section>
            <h4 className="label-sm">Context <span className="subtle">· given to the agent with every message</span></h4>
            <div className="cc-list">
              <ItemCard item={item} note="This item" noDetails />
              {context.map((e) => (
                <EntryCard key={e.id} entry={e} onSelect={onSelect}>
                  <IconButton icon="pin" label={e.pinned ? "Unpin" : "Pin"} active={e.pinned} onClick={() => pin(e)} />
                  <IconButton icon="x" label="Remove" onClick={() => remove(e)} />
                </EntryCard>
              ))}
            </div>
            {!context.length && <p className="subtle ctx-empty">Add something from Found below, or ask the agent to attach a ticket or PR.</p>}
          </section>
          <section>
            <h4 className="label-sm">Found <span className="subtle">· from the agent and the knowledge graph</span></h4>
            {!foundCount && <p className="subtle ctx-empty">Nothing yet. The agent files what it finds here as it works.</p>}
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

const urgencyLabel: Record<string, { label: string; tone: Tone }> = {
  now: { label: "Now", tone: "danger" },
  today: { label: "Today", tone: "warning" },
  this_week: { label: "This week", tone: "info" },
  whenever: { label: "Whenever", tone: "neutral" },
};

/** The one status that says most about an item: PR state, done/snoozed, or urgency. */
function statusOf(item: QueueItem): { label: string; tone: Tone } | null {
  const pr = (item.metadata as { pr_state?: string } | null)?.pr_state;
  if (pr === "draft") return { label: "Draft", tone: "neutral" };
  if (item.state === "done") return { label: "Done", tone: "success" };
  if (item.state === "snoozed") return { label: item.snoozed_until ? `Snoozed ${timeUntil(item.snoozed_until)}` : "Snoozed", tone: "neutral" };
  if (pr === "open") return { label: "Open", tone: "success" };
  const u = item.ranking?.urgency;
  return u ? urgencyLabel[u] ?? null : null;
}

/**
 * One line of context: what it is, its title, where it's from, its status.
 * Click the title to open it; expand for the rich preview.
 */
function Card({ glyph, type, title, subtitle, status, note, pinned, onOpen, actions, details }: {
  glyph: React.ReactNode;
  type: string;
  title: string;
  subtitle?: string | null;
  status?: { label: string; tone: Tone } | null;
  note?: string | null;
  pinned?: boolean;
  onOpen?: () => void;
  actions?: React.ReactNode;
  details?: () => React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className={cx("cc", pinned && "pinned", open && "open")}>
      <div className="cc-row">
        {glyph}
        <div className="cc-main">
          <div className="cc-top">
            <span className="cc-type">{type}</span>
            {onOpen ? <button className="cc-title link-btn" onClick={onOpen} title={title}>{title}</button> : <span className="cc-title" title={title}>{title}</span>}
          </div>
          {(subtitle || note) && <div className="cc-sub">{[note, subtitle].filter(Boolean).join(" · ")}</div>}
        </div>
        {status && <Badge tone={status.tone}>{status.label}</Badge>}
        <div className="cc-actions">
          {pinned && <Icon name="pin" size={12} />}
          {details && <IconButton icon="chevron-down" label={open ? "Hide details" : "Details"} active={open} onClick={() => setOpen((v) => !v)} />}
          {actions}
        </div>
      </div>
      {open && details && <div className="cc-details">{details()}</div>}
    </div>
  );
}

function ItemCard({ item, note, onOpen, pinned, summary, noDetails, children }: { item: QueueItem; note?: string; onOpen?: () => void; pinned?: boolean; summary?: string | null; noDetails?: boolean; children?: React.ReactNode }) {
  const { kind } = useApp();
  const k = kind(item.kind);
  return (
    <Card
      glyph={<SourceGlyph kind={item.kind} source={item.source} icon={k.icon} tone={k.tone} size={22} />}
      type={k.label}
      title={item.title}
      subtitle={[item.actor, item.context, timeAgo(item.occurred_at)].filter(Boolean).join(" · ")}
      note={summary ?? note}
      status={statusOf(item)}
      pinned={pinned}
      onOpen={onOpen}
      actions={children}
      details={noDetails ? undefined : () => <ItemDetails item={item} />}
    />
  );
}

function ItemDetails({ item }: { item: QueueItem }) {
  const { data: preview } = usePreview(item.id);
  const r = item.ranking;
  return (
    <>
      {r && (
        <div className="why-math">
          <ScorePill score={r.score} urgency={r.urgency} />
          <span>{r.reason}</span>
        </div>
      )}
      <Blocks blocks={preview?.blocks.length ? preview.blocks : item.blocks} />
    </>
  );
}

function EntryCard({ entry: e, onSelect, children }: { entry: ContextEntry; onSelect: (id: string) => void; children: React.ReactNode }) {
  if (e.ref_item)
    return (
      <ItemCard item={e.ref_item} pinned={e.pinned} summary={e.summary} note={e.added_by === "agent" ? "Found by agent" : undefined} onOpen={() => onSelect(e.ref_item!.id)}>
        {children}
      </ItemCard>
    );
  let host = "";
  try {
    host = e.url ? new URL(e.url).host.replace(/^www\./, "") : "";
  } catch {
    host = e.url ?? "";
  }
  return (
    <Card
      glyph={<span className="source-glyph tone-neutral" style={{ width: 22, height: 22 }}><Icon name="link" size={12} /></span>}
      type="Link"
      title={e.title}
      subtitle={host}
      note={e.summary}
      pinned={e.pinned}
      onOpen={e.url ? () => openUrl(e.url!) : undefined}
      actions={children}
    />
  );
}
