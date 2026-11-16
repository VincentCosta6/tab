import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Badge, Button } from "../../design/components";
import { SourceGlyph } from "../../design/brands";
import { useApp } from "../../lib/app-context";
import { api } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import { keys } from "../../lib/query";
import type { Chat } from "../../lib/types";
import type { ItemAction } from "./optimistic";
import { SnoozeMenu } from "./SnoozeMenu";
import type { Stack } from "./stacks";
import { QueueRow } from "./WorkTab";

/** "Sentry issue" → "Sentry issues". */
export const plural = (label: string, n: number) => (n === 1 ? label : `${label}s`);

/**
 * A stack, open: what's in it, and what to do with all of it at once. Triage
 * starts one chat with every member in its Context, rather than a chat each.
 */
export function StackPane({ stack: s, project, onAct, onSelect, onStarted }: {
  stack: Stack;
  /** The queue's project filter, for the triage chat when the members don't agree on one. */
  project: string | null;
  onAct: (action: ItemAction) => void;
  onSelect: (id: string) => void;
  onStarted: (itemId: string) => void;
}) {
  const { kind } = useApp();
  const qc = useQueryClient();
  const k = kind(s.kind);
  const [snoozeOpen, setSnoozeOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const what = plural(k.label, s.items.length);

  const triage = async () => {
    setBusy(true);
    setError(null);
    try {
      const projects = new Set(s.items.map((i) => i.project_id ?? null));
      const into = projects.size === 1 ? [...projects][0] : project;
      const list = s.items.map((i) => `- ${i.url ? `[${i.title}](${i.url})` : i.title}`).join("\n");
      const text = `Triage ${s.items.length} ${what} in ${s.label}\n\nFind which share a cause, which are noise, and what to fix first.\n\n${list}`;
      const { item, session } = await api.chatSend(null, text, into);
      qc.setQueryData(keys.item(item.id), item);
      qc.setQueryData<Chat>(keys.chat(item.id), { session, messages: [], approvals: [], running: true });
      await Promise.all(s.items.map((i) => api.contextAdd(item.id, { section: "context", ref_item_id: i.id, title: i.title, added_by: "user" })));
      qc.invalidateQueries({ queryKey: keys.allItems });
      onStarted(item.id);
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  return (
    <div className="item-pane stack-pane">
      <header className="item-head">
        <div className="item-head-glyph stack-glyph">
          <SourceGlyph kind={s.kind} source={s.newest.source} icon={k.icon} tone={k.tone} size={34} />
        </div>
        <h2 className="item-title">{s.label}</h2>
        <div className="item-meta">
          <Badge tone={k.tone}>{s.items.length} {what}</Badge>
          <span className="item-when">· latest {timeAgo(s.newest.occurred_at)}</span>
        </div>
        <div className="item-actions">
          <Button size="sm" icon="sparkles" disabled={busy} title="One chat about all of them" onClick={triage}>Triage with agent</Button>
          <span className="menu-anchor">
            <Button size="sm" icon="clock" title="Snooze all" onClick={() => setSnoozeOpen((v) => !v)}>Snooze all</Button>
            {snoozeOpen && <SnoozeMenu style={{ right: 0 }} onPick={(m) => (setSnoozeOpen(false), onAct({ snooze: m }))} onClose={() => setSnoozeOpen(false)} />}
          </span>
          <Button size="sm" variant="primary" icon="check" className="item-done" onClick={() => onAct("done")}>All done</Button>
        </div>
      </header>
      <div className="stack-body">
        {error && <div className="stack-error">{error}</div>}
        {s.items.map((i) => (
          <QueueRow key={i.id} item={i} selected={false} compact onSelect={() => onSelect(i.id)} />
        ))}
      </div>
    </div>
  );
}
