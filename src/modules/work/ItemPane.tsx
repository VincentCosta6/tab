import { lazy, Suspense, useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Badge, Button, UrgencyLabel, cx } from "../../design/components";
import { OpenMenu } from "../../design/OpenMenu";
import { SourceGlyph } from "../../design/brands";
import { api } from "../../lib/api";
import { useApp } from "../../lib/app-context";
import { timeAgo, timeUntil } from "../../lib/format";
import { useChat, useItem } from "../../lib/hooks";
import { keys } from "../../lib/query";
import type { QueueItem } from "../../lib/types";
import { parsePullUrl } from "../github/pulls";
import { ChatView } from "./Chat";
import { ContextDrawer } from "./ContextDrawer";
import { ContextRail } from "./ContextRail";
import { ReadModeSwitch, Reader, useHasRich, useReadMode } from "./Reader";
import { ProjectPicker, useProjectTint } from "./Projects";
import { SnoozeMenu } from "./SnoozeMenu";
import { Timeline } from "./Timeline";

const ReviewView = lazy(() => import("../github/review/Workspace"));

/** An item: header and actions, its rolled-up context, and a chat with an agent about it. */
export function ItemPane({ id, onActed, onSelect }: { id: string; onActed: (id: string) => void; onSelect: (id: string) => void }) {
  const { kind } = useApp();
  const qc = useQueryClient();
  const { data: item } = useItem(id);
  const [snoozeOpen, setSnoozeOpen] = useState(false);
  const [reviewing, setReviewing] = useState(false);
  const { data: chat } = useChat(id);
  const [readMode, setReadMode] = useReadMode();
  const tint = useProjectTint(item?.project_id);
  const chatCount = chat?.messages.filter((m) => m.role === "user" || (m.role === "assistant" && m.kind === "text")).length ?? 0;
  // Mail is for reading: open on the message. Everything else opens on the chat.
  const [tab, setTab] = useState<"content" | "chat" | "timeline">(() => (id && item?.kind.startsWith("email.") ? "content" : "chat"));
  useEffect(() => {
    if (item?.kind.startsWith("email.") && !chatCount) setTab("content");
    // Only when the item first loads.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [item?.id]);

  const pull = item ? parsePullUrl(item.url) : null;

  const settle = (id: string) => {
    onActed(id);
    qc.invalidateQueries({ queryKey: keys.allItems });
  };
  const done = async (i: QueueItem) => (settle(i.id), await api.markDone(i.id));
  const snooze = async (i: QueueItem, minutes: number) => (setSnoozeOpen(false), settle(i.id), await api.snooze(i.id, minutes));
  const move = async (i: QueueItem, projectId: string | null) => {
    const next = await api.itemSetProject(i.id, projectId);
    if (next) qc.setQueryData(keys.item(i.id), next);
    qc.invalidateQueries({ queryKey: keys.allItems });
  };
  const reopen = async (i: QueueItem) => (await api.reopen(i.id), qc.invalidateQueries({ queryKey: keys.allItems }));

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (!item || t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t?.isContentEditable || e.metaKey || e.ctrlKey) return;
      if (snoozeOpen) return;
      if (e.key === "e") item.state === "done" ? reopen(item) : done(item);
      if (e.key === "s") setSnoozeOpen(true);
      if (e.key === "r" && pull) setReviewing(true);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  if (!item) return <div className="pane-loading"><div className="skeleton" /></div>;

  if (reviewing && pull)
    return (
      <div className="rv-takeover">
        <Suspense fallback={<div className="pane-loading"><div className="skeleton" /></div>}>
          <ReviewView repo={pull.repo} number={pull.number} sample={item.source === "demo"} onBack={() => setReviewing(false)} />
        </Suspense>
      </div>
    );

  const k = kind(item.kind);
  const r = item.ranking;
  return (
    <div className={cx("item-pane", tint.className)} style={tint.style}>
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
          <ProjectPicker value={item.project_id ?? null} onChange={(p) => move(item, p)} align="right" compact />
          {pull && <Button size="sm" icon="code" shortcut="R" onClick={() => setReviewing(true)}>Review</Button>}
          {item.url && <OpenMenu url={item.url} onOpenInTab={pull ? () => setReviewing(true) : undefined} tabLabel="Open in tab (review)" />}
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
      <ContextDrawer
        item={item}
        onSelect={onSelect}
        left={
          <div className="pane-tabs" role="tablist">
            {(["content", "chat", "timeline"] as const).map((t) => (
              <button key={t} role="tab" aria-selected={tab === t} className={cx("pane-tab", tab === t && "active")} onClick={() => setTab(t)}>
                {t === "chat" ? "Chat" : t === "timeline" ? "Timeline" : item.kind.startsWith("email.") ? "Email" : "Details"}
                {t === "chat" && chatCount > 0 && <span className="pane-tab-count">{chatCount}</span>}
              </button>
            ))}
          </div>
        }
        right={tab === "content" && <RichSwitch item={item} mode={readMode} onChange={setReadMode} />}
      />
      <div className="item-body">
        <ChatView item={item} body={tab === "timeline" ? <Timeline itemId={item.id} /> : <Reader item={item} mode={readMode} />} showBody={tab !== "chat"} onSent={() => setTab("chat")} />
        <ContextRail key={item.id} item={item} onSelect={onSelect} />
      </div>
    </div>
  );
}

function RichSwitch({ item, mode, onChange }: { item: QueueItem; mode: "rich" | "plain"; onChange: (m: "rich" | "plain") => void }) {
  return useHasRich(item) ? <ReadModeSwitch mode={mode} onChange={onChange} /> : null;
}
