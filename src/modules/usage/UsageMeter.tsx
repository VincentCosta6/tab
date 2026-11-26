import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { Button, Meter, cx } from "../../design/components";
import { timeUntil } from "../../lib/format";
import { api } from "../../lib/api";
import { useConfig, useHarnesses } from "../../lib/hooks";
import type { AgentKind, Harness, PlanWindow, Tone } from "../../lib/types";
import { HarnessIcon } from "../work/ModelMenu";

const windowLabel: Record<string, string> = {
  five_hour: "5-hour window",
  seven_day: "Weekly",
  seven_day_opus: "Weekly (Opus)",
  seven_day_sonnet: "Weekly (Sonnet)",
};

const labelFor = (w: PlanWindow) => w.label || windowLabel[w.window] || w.window;

export const money = (n: number) => (Number.isInteger(n) ? `$${n}` : n >= 100 ? `$${n.toFixed(0)}` : n >= 10 ? `$${n.toFixed(1)}` : `$${n.toFixed(2)}`);
const tone = (u: number): Tone => (u >= 0.9 ? "danger" : u >= 0.75 ? "warning" : "accent");
const remainingPct = (u: number) => Math.max(0, Math.min(100, Math.round((1 - u) * 100)));

/** Plan windows that haven't reset yet. */
function liveWindows(ws: PlanWindow[]) {
  const now = Date.now();
  return ws.filter((w) => w.utilization != null && (!w.resets_at || new Date(w.resets_at).getTime() > now));
}

interface HarnessUsage {
  agent: AgentKind;
  harness: Harness | undefined;
  name: string;
  windows: PlanWindow[];
  tightest: PlanWindow;
  leftPct: number;
}

function groupByHarness(windows: PlanWindow[], harnesses: Harness[]): HarnessUsage[] {
  const byAgent = new Map<AgentKind, PlanWindow[]>();
  for (const w of windows) {
    const list = byAgent.get(w.agent) ?? [];
    list.push(w);
    byAgent.set(w.agent, list);
  }
  const order = new Map<string, number>(harnesses.map((h, i) => [h.id, i]));
  const harnessById = new Map<string, Harness>(harnesses.map((h) => [h.id, h]));
  return [...byAgent.entries()]
    .sort(([a], [b]) => (order.get(a) ?? 999) - (order.get(b) ?? 999) || a.localeCompare(b))
    .map(([agent, ws]) => {
      const tightest = ws.reduce((a, b) => {
        if (b.utilization! !== a.utilization!) return b.utilization! > a.utilization! ? b : a;
        const ta = a.resets_at ? new Date(a.resets_at).getTime() : Infinity;
        const tb = b.resets_at ? new Date(b.resets_at).getTime() : Infinity;
        return tb < ta ? b : a;
      });
      const harness = harnessById.get(agent);
      return {
        agent,
        harness,
        name: harness?.name ?? agent,
        windows: ws,
        tightest,
        leftPct: remainingPct(tightest.utilization!),
      };
    });
}

export function useUsage() {
  return useQuery({ queryKey: ["usage"], queryFn: api.usageSummary, refetchInterval: 10_000, staleTime: 0 });
}

/**
 * Bottom-left of the nav: one compact row per harness with active usage
 * (icon, remaining %, reset time). Harnesses at 100% or with no usage stay hidden.
 */
export function UsageMeter({ onSettings }: { onSettings: () => void }) {
  const { data: u } = useUsage();
  const { data: config } = useConfig();
  const { data: harnesses = [] } = useHarnesses();
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

  const groups = useMemo(() => (u ? groupByHarness(liveWindows(u.windows), harnesses) : []), [u, harnesses]);
  if (!u) return null;

  const budget = config?.agent.budget;
  const daily = budget?.daily_usd || null;
  const monthly = budget?.monthly_usd || null;
  const overDaily = daily != null && u.today_usd >= daily;
  const sub = u.billing === "subscription" || groups.length > 0;
  // Only show harnesses that actually have usage (< 100% remaining).
  const activeRows = groups.filter((g) => g.leftPct < 100);
  const showApiSpend = !sub && u.today_usd >= 0.005;

  if (!activeRows.length && !showApiSpend) return null;

  return (
    <div className="usage" ref={ref}>
      <button
        className={cx("usage-chip", open && "active", overDaily && "over")}
        onClick={() => setOpen(!open)}
        title="Usage limits and agent spend"
      >
        {activeRows.length ? (
          activeRows.map((g) => {
            const reset = g.tightest.resets_at ? timeUntil(g.tightest.resets_at) : null;
            return (
              <span
                key={g.agent}
                className="usage-row"
                title={`${g.name} · ${labelFor(g.tightest)}: ${g.leftPct}% left${reset ? ` (resets in ${reset})` : ""}`}
              >
                <span className="usage-row-icon">
                  <HarnessIcon harness={g.harness} size={11} />
                </span>
                <span className={cx("usage-row-pct", `text-${tone(g.tightest.utilization!)}`)}>
                  {g.leftPct}%
                </span>
                {reset && <span className="usage-row-reset">{reset}</span>}
              </span>
            );
          })
        ) : (
          <span className="usage-row usage-row-spend">
            <span className="usage-row-pct">{money(u.today_usd)}</span>
            <span className="usage-row-reset">today</span>
          </span>
        )}
      </button>

      {open && (
        <div className="usage-pop">
          {groups.length > 0 && (
            <div className="usage-section">
              <div className="usage-pop-head">
                <strong>Plan limits</strong>
              </div>
              {groups.map((g) => (
                <div key={g.agent} className="usage-harness">
                  <div className="usage-harness-head">
                    <HarnessIcon harness={g.harness} size={13} />
                    <span>{g.name}</span>
                  </div>
                  {g.windows.map((w) => (
                    <div key={`${w.agent}:${w.window}`} className="usage-window">
                      <div className="usage-window-head">
                        <span>{labelFor(w)}</span>
                        <span className={`text-${tone(w.utilization!)}`}>{remainingPct(w.utilization!)}% left</span>
                      </div>
                      <Meter value={w.utilization! * 100} max={100} tone={tone(w.utilization!)} />
                      {w.resets_at && <span className="subtle">Resets in {timeUntil(w.resets_at)}</span>}
                    </div>
                  ))}
                </div>
              ))}
            </div>
          )}

          <div className="usage-section">
            <div className="usage-pop-head">
              <strong>Agent spend</strong>
              <span className="subtle">{sub ? "Shown as API-equivalent cost" : u.billing === "api" ? "Billed to your API key" : ""}</span>
            </div>
            <div className="usage-stats">
              <div><span className="usage-big">{money(u.today_usd)}</span><span className="subtle">today{daily ? ` of ${money(daily)}` : ""}</span></div>
              <div><span className="usage-big">{money(u.month_usd)}</span><span className="subtle">this month{monthly ? ` of ${money(monthly)}` : ""}</span></div>
            </div>
            {daily ? <Meter value={Math.min(u.today_usd, daily)} max={daily} tone={overDaily ? "danger" : u.today_usd / daily > 0.8 ? "warning" : "accent"} /> : null}
            <div className="usage-split subtle">
              Chats {money(u.today_agent_usd)} over {u.today_turns} turn{u.today_turns === 1 ? "" : "s"} · Queue reading {money(u.today_ranker_usd)} · {(u.today_tokens / 1000).toFixed(0)}k tokens
            </div>
          </div>

          <Button size="sm" variant="ghost" icon="settings" onClick={() => { setOpen(false); onSettings(); }}>Set limits</Button>
        </div>
      )}
    </div>
  );
}
