import { useEffect } from "react";
import { Button, ScorePill } from "../../design/components";
import type { QueueItem } from "../../lib/types";

/** Turns a rules `fired` entry (`keyword "outage" +8`) into a label and signed value. */
function parseFired(f: string): { label: string; value: string } {
  const kind = f.match(/^kind (.+) = (\d+)$/);
  if (kind) return { label: `Base for ${kind[1]}`, value: kind[2] };
  const delta = f.match(/^(.*?) ([+-]\d+)$/);
  if (delta) return { label: delta[1].replace(/^vip$/, "VIP sender").replace(/^priority /, "Priority: "), value: delta[2] };
  return { label: f, value: "" };
}

/** Why an item has the score it has: rules that fired, the AI nudge, the total. */
export function ScoreBreakdown({ item, onClose }: { item: QueueItem; onClose: () => void }) {
  const r = item.ranking;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && (e.stopPropagation(), onClose());
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);
  return (
    <div className="score-overlay" onMouseDown={onClose}>
      <div className="score-card" role="dialog" aria-label="Score breakdown" onMouseDown={(e) => e.stopPropagation()} onClick={(e) => e.stopPropagation()}>
        <div className="score-card-head">
          <div className="score-card-title">{item.title}</div>
          {r && <ScorePill score={r.score} urgency={r.urgency} />}
        </div>
        {!r ? (
          <p className="score-note">This item hasn't been ranked yet.</p>
        ) : (
          <>
            <p className="score-note">{r.reason}</p>
            <div className="score-lines">
              {r.fired.length === 0 && <div className="score-line muted"><span>Rule detail wasn't recorded for this item (re-rank to see it)</span></div>}
              {r.fired.map((f, i) => {
                const p = parseFired(f);
                return <div className="score-line" key={i}><span>{p.label}</span><b>{p.value}</b></div>;
              })}
              <div className="score-line total"><span>Rules score</span><b>{r.base_score}</b></div>
              {r.ai_adjustment !== null && (
                <div className="score-line"><span>AI adjustment ({r.ranked_by.replace(/^rules\+/, "")})</span><b>{r.ai_adjustment > 0 ? "+" : ""}{r.ai_adjustment}</b></div>
              )}
              <div className="score-line total"><span>Final score</span><b>{r.score}</b></div>
            </div>
          </>
        )}
        <div className="score-card-foot"><Button onClick={onClose}>Close</Button></div>
      </div>
    </div>
  );
}
