// Pieces the Factory overview, the Insights panel and the canvas share.

import { useEffect, type ReactNode } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Badge, cx } from "../../../design/components";
import { onEngineEvent } from "../../../lib/api";
import type { DayStats, RunCounts, WorkflowHealth } from "../../../lib/types";
import { Ring, type Series } from "./charts";
import { healthIcon, healthLabel, healthTone, MIN_RUNS, pct, plural, rate } from "./format";
import { setWindowDays, useWindowDays, WINDOW_DAYS } from "./useInsights";

export const outcomeSeries: Series[] = [
  { key: "unattended", label: "Succeeded on its own", color: "var(--chart-ok)" },
  { key: "attended", label: "Succeeded with help", color: "var(--chart-attended)" },
  { key: "failed", label: "Failed", color: "var(--chart-failed)" },
];

/** Per day: unattended success, attended success, failed (the order of `outcomeSeries`). */
export function outcomeValues(days: DayStats[]): Record<string, number[]> {
  return Object.fromEntries(days.map((d) => [d.day, [d.unattended, d.succeeded - d.unattended, d.failed]]));
}

export function WindowPicker() {
  const days = useWindowDays();
  return (
    <div className="wf-seg ins-seg" role="radiogroup" aria-label="Window">
      {WINDOW_DAYS.map((d) => (
        <button key={d} role="radio" aria-checked={days === d} className={cx(days === d && "active")} onClick={() => setWindowDays(d)}
          title={d > 30 ? "Older days come from daily totals: counts and spend are exact, step times close" : undefined}>
          {d === 365 ? "1 year" : `${d} days`}
        </button>
      ))}
    </div>
  );
}

export function HealthBadge({ health }: { health: WorkflowHealth }) {
  return <Badge tone={healthTone[health]} icon={healthIcon[health]} title={health === "unknown" ? "Fewer than 3 finished runs" : "From its latest 20 runs"}>{healthLabel[health]}</Badge>;
}

/**
 * The three rings every view shows: on its own (the headline), succeeded,
 * and people stepping in split into planned and unplanned. Under
 * `MIN_RUNS` runs they draw an empty track and say so.
 */
export function RateRings({ counts, size = 76, tone, headlineSize, deltas = [] }: { counts: RunCounts; size?: number; tone?: string; headlineSize?: number; deltas?: ReactNode[] }) {
  const thin = counts.runs < MIN_RUNS;
  const sub = thin ? (counts.runs ? plural(counts.runs, "run") : "no runs") : undefined;
  const un = rate(counts.unattended, counts.runs), ok = rate(counts.succeeded, counts.runs);
  const planned = rate(counts.attended - counts.unplanned, counts.runs) ?? 0, unplanned = rate(counts.unplanned, counts.runs) ?? 0;
  return (
    <>
      <figure className="ins-ring-fig headline">
        <Ring size={headlineSize ?? size + 20} empty={thin} center={thin ? "–" : pct(un)} sub={sub}
          segments={[{ value: un ?? 0, color: tone ?? "var(--chart-ok)", label: "On its own" }]}
          label={thin ? `Not enough runs to rate (${counts.runs})` : `${pct(un)} of runs succeeded with nobody stepping in`} />
        <figcaption>Succeeded on its own</figcaption>
        {deltas[0]}
      </figure>
      <figure className="ins-ring-fig">
        <Ring size={size} empty={thin} center={thin ? "–" : pct(ok)} sub={sub}
          segments={[{ value: ok ?? 0, color: "var(--chart-ok)", label: "Succeeded" }]}
          label={thin ? `Not enough runs to rate (${counts.runs})` : `${pct(ok)} of runs succeeded`} />
        <figcaption>Succeeded</figcaption>
        {deltas[1]}
      </figure>
      <figure className="ins-ring-fig">
        <Ring size={size} empty={thin} center={thin ? "–" : pct(planned + unplanned)} sub={sub}
          segments={[
            { value: planned, color: "var(--chart-planned)", label: "Planned" },
            { value: unplanned, color: "var(--chart-unplanned)", label: "Unplanned" },
          ]}
          label={thin ? `Not enough runs to rate (${counts.runs})` : `A person stepped in on ${pct(planned + unplanned)} of runs: ${pct(planned)} planned approvals, ${pct(unplanned)} unplanned`} />
        <figcaption>
          People stepped in
          {!thin && counts.attended > 0 && (
            <span className="ins-split-legend">
              <span><i style={{ background: "var(--chart-planned)" }} />{pct(planned)} planned</span>
              <span><i style={{ background: "var(--chart-unplanned)" }} />{pct(unplanned)} unplanned</span>
            </span>
          )}
        </figcaption>
        {deltas[2]}
      </figure>
    </>
  );
}

/** Refreshes the numbers when a run finishes (at most every few seconds). */
export function useInsightsLive() {
  const qc = useQueryClient();
  useEffect(() => {
    let off: (() => void) | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    onEngineEvent((e) => {
      if (e.type !== "workflow_run" || timer) return;
      timer = setTimeout(() => {
        timer = undefined;
        qc.invalidateQueries({ queryKey: ["workflow-factory"] });
        qc.invalidateQueries({ queryKey: ["workflow-insights"] });
      }, 3000);
    }).then((u) => (off = u));
    return () => { off?.(); if (timer) clearTimeout(timer); };
  }, [qc]);
}
