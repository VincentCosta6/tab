import { lazy, Suspense, useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Badge, Button, UrgencyLabel } from "../../design/components";
import { SourceGlyph } from "../../design/brands";
import { api, openUrl } from "../../lib/api";
import { useApp } from "../../lib/app-context";
import { timeAgo, timeUntil } from "../../lib/format";
import { useItem } from "../../lib/hooks";
import { keys } from "../../lib/query";
import type { QueueItem } from "../../lib/types";
import { parsePullUrl } from "../github/pulls";
import { ChatView } from "./Chat";
import { ContextDrawer } from "./ContextDrawer";
import { SnoozeMenu } from "./SnoozeMenu";

const ReviewView = lazy(() => import("../github/ReviewView"));

/** An item: header and actions, its rolled-up context, and a chat with an agent about it. */
export function ItemPane({ id, onActed, onSelect }: { id: string; onActed: (id: string) => void; onSelect: (id: string) => void }) {
  const { kind } = useApp();
  const qc = useQueryClient();
  const { data: item } = useItem(id);
  const [snoozeOpen, setSnoozeOpen] = useState(false);
  const [reviewing, setReviewing] = useState(false);

  const pull = item ? parsePullUrl(item.url) : null;

  const settle = (id: string) => {
    onActed(id);
    qc.invalidateQueries({ queryKey: keys.allItems });
  };
  const done = async (i: QueueItem) => (settle(i.id), await api.markDone(i.id));
  const snooze = async (i: QueueItem, minutes: number) => (setSnoozeOpen(false), settle(i.id), await api.snooze(i.id, minutes));
  const reopen = async (i: QueueItem) => (await api.reopen(i.id), qc.invalidateQueries({ queryKey: keys.allItems }));

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (!item || t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t?.isContentEditable || e.metaKey || e.ctrlKey) return;
      if (snoozeOpen) return;
      if (e.key === "e") item.state === "done" ? reopen(item) : done(item);
      if (e.key === "s") setSnoozeOpen(true);
      if (e.key === "o" && item.url) openUrl(item.url);
      if (e.key === "r" && pull) setReviewing(true);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  if (!item) return <div className="pane-loading"><div className="skeleton" /></div>;

  if (reviewing && pull)
    return (
      <Suspense fallback={<div className="pane-loading"><div className="skeleton" /></div>}>
        <ReviewView repo={pull.repo} number={pull.number} sample={item.source === "demo"} onBack={() => setReviewing(false)} />
      </Suspense>
    );

  const k = kind(item.kind);
  const r = item.ranking;
  return (
    <div className="item-pane">
      <header className="item-head">
        <div className="item-head-main">
          <SourceGlyph kind={item.kind} source={item.source} icon={k.icon} tone={k.tone} size={34} />
          <div className="item-head-text">
            <h2 className="item-title" title={item.title}>{item.title}</h2>
            <div className="item-meta">
              <Badge tone={k.tone}>{k.label}</Badge>
              {r && <UrgencyLabel urgency={r.urgency} />}
              {item.actor && <span>{item.actor}</span>}
              {item.context && <span>· {item.context}</span>}
              <span>· {timeAgo(item.occurred_at)}</span>
              {item.state === "snoozed" && item.snoozed_until && <Badge icon="clock">back in {timeUntil(item.snoozed_until)}</Badge>}
              {item.state === "done" && <Badge tone="success" icon="check">Done</Badge>}
            </div>
          </div>
        </div>
        <div className="item-actions">
          {pull && <Button size="sm" icon="code" shortcut="R" onClick={() => setReviewing(true)}>Review</Button>}
          {item.url && <Button size="sm" icon="external" shortcut="O" onClick={() => openUrl(item.url!)}>Open</Button>}
          {item.state !== "done" && (
            <span className="menu-anchor">
              <Button size="sm" icon="clock" shortcut="S" onClick={() => setSnoozeOpen((v) => !v)}>Snooze</Button>
              {snoozeOpen && <SnoozeMenu style={{ right: 0 }} onPick={(m) => snooze(item, m)} onClose={() => setSnoozeOpen(false)} />}
            </span>
          )}
          {item.state === "done" ? (
            <Button size="sm" icon="undo" shortcut="E" onClick={() => reopen(item)}>Reopen</Button>
          ) : (
            <Button size="sm" variant="primary" icon="check" shortcut="E" onClick={() => done(item)}>Done</Button>
          )}
        </div>
      </header>
      <ContextDrawer item={item} onSelect={onSelect} />
      <ChatView item={item} />
    </div>
  );
}
