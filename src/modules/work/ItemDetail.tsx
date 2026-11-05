import { useEffect, useState } from "react";
import { Badge, Button, Card, ScorePill, SourceIcon, UrgencyLabel } from "../../design/components";
import { Blocks } from "../../design/blocks";
import { api, openUrl } from "../../lib/api";
import { useApp } from "../../lib/app-context";
import { timeAgo, timeUntil } from "../../lib/format";
import type { Block, ItemDetail } from "../../lib/types";
import { QueueRow } from "./WorkTab";

const snoozeOptions = [
  { label: "1 hour", minutes: 60, key: "1" },
  { label: "4 hours", minutes: 240, key: "2" },
  { label: "Tomorrow morning", minutes: () => minutesUntilHour(9, 1), key: "3" },
  { label: "Next week", minutes: () => minutesUntilHour(9, 7), key: "4" },
];

function minutesUntilHour(hour: number, days: number) {
  const t = new Date();
  t.setDate(t.getDate() + days);
  t.setHours(hour, 0, 0, 0);
  return Math.max(1, Math.round((t.getTime() - Date.now()) / 60_000));
}

export function ItemDetailPane({ id, onActed, onSelect }: { id: string; onActed: (id: string) => void; onSelect: (id: string) => void }) {
  const { kind } = useApp();
  const [detail, setDetail] = useState<ItemDetail | null>(null);
  const [preview, setPreview] = useState<Block[] | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [snoozeOpen, setSnoozeOpen] = useState(false);

  useEffect(() => {
    let live = true;
    setPreview(null);
    setPreviewError(null);
    setSnoozeOpen(false);
    api.itemDetail(id).then((d) => live && setDetail(d));
    // Previews come through the gateway cache, so re-opening an item is free.
    api.itemPreview(id).then(
      (p) => live && setPreview(p),
      (e) => live && (setPreview([]), setPreviewError(String(e))),
    );
    return () => {
      live = false;
    };
  }, [id]);

  const item = detail?.item.id === id ? detail.item : null;

  const done = async () => {
    if (!item) return;
    onActed(item.id);
    await api.markDone(item.id);
  };
  const snooze = async (minutes: number) => {
    if (!item) return;
    setSnoozeOpen(false);
    onActed(item.id);
    await api.snooze(item.id, minutes);
  };
  const reopen = async () => item && (await api.reopen(item.id), onActed(item.id));

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!item || e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement || e.metaKey || e.ctrlKey) return;
      if (snoozeOpen) {
        const opt = snoozeOptions.find((o) => o.key === e.key);
        if (opt) snooze(typeof opt.minutes === "function" ? opt.minutes() : opt.minutes);
        if (e.key === "Escape") setSnoozeOpen(false);
        return;
      }
      if (e.key === "e") item.state === "done" ? reopen() : done();
      if (e.key === "s") setSnoozeOpen(true);
      if (e.key === "o" && item.url) openUrl(item.url);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  if (!item) return <div className="detail-inner"><div className="skeleton" /></div>;
  const k = kind(item.kind);
  const r = item.ranking;

  return (
    <article className="detail-inner">
      <div className="detail-kicker">
        <SourceIcon icon={k.icon} tone={k.tone} size={22} />
        <Badge tone={k.tone}>{k.label}</Badge>
        {r && <UrgencyLabel urgency={r.urgency} />}
        {item.state === "snoozed" && item.snoozed_until && <Badge icon="clock">Snoozed, back in {timeUntil(item.snoozed_until)}</Badge>}
        {item.state === "done" && <Badge tone="success" icon="check">Done</Badge>}
      </div>
      <h2 className="detail-title">{item.title}</h2>
      <div className="detail-meta">
        {item.actor && <span>{item.actor}</span>}
        {item.context && <span>· {item.context}</span>}
        <span>· {timeAgo(item.occurred_at)}</span>
      </div>

      <div className="detail-actions">
        {item.state === "done" ? (
          <Button icon="undo" shortcut="E" onClick={reopen}>Reopen</Button>
        ) : (
          <Button variant="primary" icon="check" shortcut="E" onClick={done}>Done</Button>
        )}
        <Button icon="clock" shortcut="S" onClick={() => setSnoozeOpen((v) => !v)}>Snooze</Button>
        {item.url && <Button icon="external" shortcut="O" onClick={() => openUrl(item.url!)}>Open</Button>}
        {snoozeOpen && (
          <div className="menu" style={{ left: item.state === "done" ? 100 : 86 }}>
            {snoozeOptions.map((o) => (
              <button key={o.key} onClick={() => snooze(typeof o.minutes === "function" ? o.minutes() : o.minutes)}>
                {o.label} <span className="kbd">{o.key}</span>
              </button>
            ))}
          </div>
        )}
      </div>

      {r && (
        <Card>
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
        </Card>
      )}

      {item.blocks.length > 0 && <Blocks blocks={item.blocks} />}

      {preview === null && <div className="skeleton" />}
      {preview && preview.length > 0 && (
        <div>
          <h3 className="label-sm">Preview</h3>
          <Blocks blocks={preview} />
        </div>
      )}
      {previewError && <p className="subtle">Preview unavailable: {previewError}</p>}

      {item.entities.length > 0 && (
        <div>
          <h3 className="label-sm">In the graph</h3>
          <div className="entities">
            {item.entities.map((e) => (
              <Badge key={`${e.kind}:${e.key}:${e.rel}`} icon={entityIcon(e.kind)} title={`${e.rel} · ${e.key}`}>{e.label}</Badge>
            ))}
          </div>
        </div>
      )}

      {detail && detail.related.length > 0 && (
        <div>
          <h3 className="label-sm">Related</h3>
          <div className="related">
            {detail.related.map((rel) => (
              <QueueRow key={rel.id} item={rel} selected={false} onSelect={() => onSelect(rel.id)} kindIcon={kind(rel.kind).icon} kindTone={kind(rel.kind).tone} />
            ))}
          </div>
        </div>
      )}
    </article>
  );
}

function entityIcon(kind: string) {
  return ({ person: "user", repo: "github", channel: "slack", pull_request: "git-pull-request", meeting: "video", agent_session: "bot" } as Record<string, string>)[kind] ?? "link";
}
