import { useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { Button, Meter, cx } from "../../design/components";
import { timeUntil } from "../../lib/format";
import { api } from "../../lib/api";
import { useConfig } from "../../lib/hooks";
import type { PlanWindow, Tone } from "../../lib/types";

const windowLabel: Record<string, string> = {
  five_hour: "5-hour window",
  seven_day: "Weekly",
  seven_day_opus: "Weekly (Opus)",
  seven_day_sonnet: "Weekly (Sonnet)",
};
const shortLabel = (w: string) => (w === "five_hour" ? "5h" : w.startsWith("seven_day") ? "7d" : w.slice(0, 3));

export const money = (n: number) => (Number.isInteger(n) ? `$${n}` : n >= 100 ? `$${n.toFixed(0)}` : n >= 10 ? `$${n.toFixed(1)}` : `$${n.toFixed(2)}`);
const tone = (u: number): Tone => (u >= 0.9 ? "danger" : u >= 0.75 ? "warning" : "accent");

/** Windows Claude Code reported that haven't reset yet. */
function liveWindows(ws: PlanWindow[]) {
  const now = Date.now();
  return ws.filter((w) => w.utilization != null && (!w.resets_at || new Date(w.resets_at).getTime() > now));
}

export function useUsage() {
  return useQuery({ queryKey: ["usage"], queryFn: api.usageSummary, refetchInterval: 10_000, staleTime: 0 });
}

/** Bottom-left of the nav: today's agent spend and the Claude plan's usage windows. */
export function UsageMeter({ onSettings }: { onSettings: () => void }) {
  const { data: u } = useUsage();
  const { data: config } = useConfig();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  if (!u) return null;
  const budget = config?.agent.budget;
  const windows = liveWindows(u.windows);
  const daily = budget?.daily_usd || null;
  const monthly = budget?.monthly_usd || null;
  const overDaily = daily != null && u.today_usd >= daily;
  const sub = u.billing === "subscription";
  // On a Claude plan the plan's limits matter more than dollars: lead with the window closest to its cap.
  const tightest = windows.reduce<PlanWindow | null>((a, w) => (!a || w.utilization! > a.utilization! ? w : a), null);

  return (
    <div className="usage" ref={ref}>
      <button className={cx("usage-chip", open && "active", overDaily && "over")} onClick={() => setOpen(!open)} title="Agent spend and plan limits">
        {sub && tightest ? (
          <>
            <span className={cx("usage-amount", `text-${tone(tightest.utilization!)}`)}>{Math.max(0, Math.round((1 - tightest.utilization!) * 100))}%</span>
            <span className="usage-caption">{shortLabel(tightest.window)} left</span>
          </>
        ) : (
          <>
            <span className="usage-amount">{money(u.today_usd)}</span>
            <span className="usage-caption">today</span>
          </>
        )}
        {windows.slice(0, 2).map((w) => (
          <span key={w.window} className="usage-mini" title={`${windowLabel[w.window] ?? w.window}: ${Math.round(w.utilization! * 100)}% used`}>
            <span className="usage-mini-label">{shortLabel(w.window)}</span>
            <span className="usage-mini-bar"><span className={`tone-${tone(w.utilization!)}`} style={{ width: `${Math.min(100, w.utilization! * 100)}%` }} /></span>
          </span>
        ))}
      </button>

      {open && (
        <div className="usage-pop">
          <div className="usage-pop-head">
            <strong>Agent spend</strong>
            <span className="subtle">{sub ? "Claude plan, shown as API-equivalent cost" : u.billing === "api" ? "Billed to your API key" : ""}</span>
          </div>
          <div className="usage-stats">
            <div><span className="usage-big">{money(u.today_usd)}</span><span className="subtle">today{daily ? ` of ${money(daily)}` : ""}</span></div>
            <div><span className="usage-big">{money(u.month_usd)}</span><span className="subtle">this month{monthly ? ` of ${money(monthly)}` : ""}</span></div>
          </div>
          {daily ? <Meter value={Math.min(u.today_usd, daily)} max={daily} tone={overDaily ? "danger" : u.today_usd / daily > 0.8 ? "warning" : "accent"} /> : null}
          <div className="usage-split subtle">
            Chats {money(u.today_agent_usd)} over {u.today_turns} turn{u.today_turns === 1 ? "" : "s"} · Queue reading {money(u.today_ranker_usd)} · {(u.today_tokens / 1000).toFixed(0)}k tokens
          </div>

          <div className="usage-pop-head"><strong>Claude plan limits</strong></div>
          {windows.length ? (
            windows.map((w) => (
              <div key={w.window} className="usage-window">
                <div className="usage-window-head">
                  <span>{windowLabel[w.window] ?? w.window}</span>
                  <span className={`text-${tone(w.utilization!)}`}>{Math.max(0, Math.round((1 - w.utilization!) * 100))}% left</span>
                </div>
                <Meter value={w.utilization! * 100} max={100} tone={tone(w.utilization!)} />
                {w.resets_at && <span className="subtle">Resets in {timeUntil(w.resets_at)}</span>}
              </div>
            ))
          ) : (
            <p className="subtle">Appears after Claude Code's next reply: Claude reports your plan's usage with each turn.</p>
          )}
          <Button size="sm" variant="ghost" icon="settings" onClick={() => { setOpen(false); onSettings(); }}>Set limits</Button>
        </div>
      )}
    </div>
  );
}
