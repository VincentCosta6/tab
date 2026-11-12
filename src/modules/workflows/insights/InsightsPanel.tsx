// One workflow's Insights: its rates against the window before, a daily
// chart with version changes marked, spend, the assembly line of its steps
// (where time and money go, and which station is slowest or costliest),
// models compared per agent step, two versions side by side, and what fails.

import { useMemo, useState } from "react";
import { Badge, Button, cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import { timeAgo } from "../../../lib/format";
import type { ModelStats, StepStats, VersionStats, Workflow, WorkflowInsights, WorkflowStats } from "../../../lib/types";
import { Delta, Legend, Ring, SplitBar, StackedBars, toneColor, type Series } from "./charts";
import {
  dayLabel, duration, hasApi, hasPlan, interventionRate, localDay, MIN_RUNS, pct, planShare, planTokens, plural, rate, rateTone, spendLong, spendShort,
  successRate, tokens, unattendedRate, usd, windowDays,
} from "./format";
import { HealthBadge, outcomeSeries, outcomeValues, RateRings, useInsightsLive, WindowPicker } from "./parts";
import { useWorkflowInsights } from "./useInsights";
import "./insights.css";

export function InsightsPanel({ workflow, onOpenRun }: { workflow: Workflow; onOpenRun: (runId: string) => void }) {
  const { data, isLoading, error } = useWorkflowInsights(workflow.id || null);
  useInsightsLive();
  if (!workflow.id) return <p className="subtle wf-pad">Save the workflow to start keeping its runs. Insights add them up once it has run.</p>;
  return (
    <div className="ins-panel">
      <div className="ins-panel-bar">
        {data && <HealthBadge health={data.health} />}
        <span className="subtle">Last {data?.days ?? "…"} days</span>
        <span className="wf-bar-spacer" />
        <WindowPicker />
      </div>
      {error && <p className="wf-error"><Icon name="alert" size={13} /> {String(error)}</p>}
      {isLoading && !data && <p className="subtle wf-pad">Adding up runs…</p>}
      {data && <Body data={data} workflow={workflow} onOpenRun={onOpenRun} />}
    </div>
  );
}

function Body({ data, workflow, onOpenRun }: { data: WorkflowInsights; workflow: Workflow; onOpenRun: (id: string) => void }) {
  const s = data.stats, p = data.previous;
  const label = (id: string, fallback = "") => fallback || workflow.steps.find((x) => x.id === id)?.label || id;
  if (s.runs === 0) {
    return (
      <div className="ins-empty">
        <Icon name="gauge" size={26} />
        <h4>No finished runs in the last {data.days} days</h4>
        <p className="subtle">
          {s.in_progress ? `${plural(s.in_progress, "run")} still going. ` : ""}
          {p.runs ? `It ran ${plural(p.runs, "time")} in the ${data.days} days before. ` : ""}
          {workflow.enabled ? "Rates, the assembly line and costs show here once it has run." : "It's turned off. Turn it on and its runs add up here."}
        </p>
      </div>
    );
  }
  const thin = s.runs < MIN_RUNS;
  const enough = p.runs >= MIN_RUNS && !thin;
  return (
    <>
      {thin && (
        <p className="ins-note"><Icon name="info" size={13} /> Only {plural(s.runs, "finished run")} so far. Rates show once there are {MIN_RUNS}; the numbers below are what there is.</p>
      )}
      <section className="ins-sec">
        <div className="ins-rings">
          <RateRings counts={s} size={70} headlineSize={88} tone={toneColor[rateTone(unattendedRate(s), s.runs)]} deltas={[
            <Delta key="u" now={unattendedRate(s)} before={unattendedRate(p)} kind="rate" better="up" enough={enough} />,
            <Delta key="s" now={successRate(s)} before={successRate(p)} kind="rate" better="up" enough={enough} />,
            <Delta key="i" now={interventionRate(s)} before={interventionRate(p)} kind="rate" better="down" enough={enough} />,
          ]} />
        </div>
        <div className="ins-mini-stats">
          <MiniStat label="Runs" value={String(s.runs)} hint={[s.in_progress && `${s.in_progress} running`, s.held_back && `${s.held_back} held back`, s.loop_stopped && `${s.loop_stopped} loop-stopped`].filter(Boolean).join(" · ") || undefined} />
          <MiniStat label="Median time" value={s.duration_ms.n ? duration(s.duration_ms.p50) : "–"} hint={s.duration_ms.n ? `slow end ${duration(s.duration_ms.p95)}` : undefined}>
            <Delta now={s.duration_ms.n ? s.duration_ms.p50 : null} before={p.duration_ms.n ? p.duration_ms.p50 : null} kind="ratio" better="down" enough={p.duration_ms.n >= MIN_RUNS} />
          </MiniStat>
          <MiniStat label="Spend" value={spendShort(s.spend) === "Nothing" ? "–" : spendShort(s.spend, true)} hint={hasPlan(s.spend) ? `${planShare(s.spend.plan_share)} of plan allowance` : hasApi(s.spend) ? `${usd(s.spend.cost_usd / s.runs)} a run` : "No agent steps ran"} title={spendLong(s.spend)} />
        </div>
      </section>

      <section className="ins-sec">
        <h4 className="ins-h">Every day</h4>
        <StackedBars days={windowDays(data.from, data.days)} values={outcomeValues(s.days)} series={outcomeSeries} height={140}
          markers={versionMarkers(s.versions, data.from)} format={(n) => String(n)}
          label={`Daily runs: ${s.unattended} succeeded on their own, ${s.succeeded - s.unattended} with help, ${s.failed} failed`} />
        <Legend series={[...outcomeSeries, { key: "v", label: "New version first ran", color: "transparent" }]} />
      </section>

      {(hasApi(s.spend) || hasPlan(s.spend)) && <SpendTrend data={data} />}

      <section className="ins-sec">
        <h4 className="ins-h">Assembly line <span className="subtle">median time per step, and where it goes</span></h4>
        <AssemblyLine workflow={workflow} stats={s} label={label} />
      </section>

      {s.models.length > 0 && <Models stats={s} label={label} />}
      {s.versions.length > 1 && <Versions versions={s.versions} />}
      <Failures stats={s} label={label} onOpenRun={onOpenRun} />
    </>
  );
}

function MiniStat({ label, value, hint, title, children }: { label: string; value: string; hint?: string; title?: string; children?: React.ReactNode }) {
  return (
    <div className="ins-stat small" title={title}>
      <span className="ins-stat-label">{label}</span>
      <span className="ins-stat-value">{value}</span>
      {hint && <span className="ins-stat-hint">{hint}</span>}
      {children}
    </div>
  );
}

/** Where each version first ran inside the window (the oldest one is left out when it was already running before). */
function versionMarkers(versions: VersionStats[], from: string) {
  const start = localDay(from);
  return versions
    .filter((v) => v.first_run && versions.some((o) => o.version < v.version))
    .map((v) => ({ day: localDay(v.first_run!), label: `v${v.version}`, title: `v${v.version} first ran ${new Date(v.first_run!).toLocaleString()}` }))
    .filter((m) => m.day >= start);
}

// ---------------------------------------------------------------------------
// Spend
// ---------------------------------------------------------------------------

function SpendTrend({ data }: { data: WorkflowInsights }) {
  const s = data.stats;
  const days = windowDays(data.from, data.days);
  const api: Series[] = [{ key: "usd", label: "API keys", color: "var(--chart-attended)" }];
  const plan: Series[] = [{ key: "tok", label: "Plan tokens", color: "var(--chart-planned)" }];
  return (
    <section className="ins-sec">
      <h4 className="ins-h">Spend <span className="subtle">API keys cost dollars; subscriptions use plan allowance, never priced</span></h4>
      <div className={cx("ins-spend", hasApi(s.spend) && hasPlan(s.spend) && "two")}>
        {hasApi(s.spend) && (
          <div>
            <div className="ins-spend-head"><span className="ins-swatch" style={{ background: "var(--chart-attended)" }} /><span>Billed through API keys</span><span><strong>{usd(s.spend.cost_usd)}</strong> <span className="subtle">· {usd(s.spend.cost_usd / Math.max(1, s.runs))} a run</span></span></div>
            <StackedBars days={days} height={70} series={api} format={usd}
              values={Object.fromEntries(s.days.map((d) => [d.day, [d.spend.cost_usd]]))} label={`Daily API spend, ${usd(s.spend.cost_usd)} in all`} />
          </div>
        )}
        {hasPlan(s.spend) && (
          <div>
            <div className="ins-spend-head"><span className="ins-swatch" style={{ background: "var(--chart-planned)" }} /><span>On a subscription</span><span><strong>{tokens(planTokens(s.spend))} tokens</strong> <span className="subtle">· {planShare(s.spend.plan_share)}</span></span></div>
            <StackedBars days={days} height={70} series={plan} format={tokens}
              values={Object.fromEntries(s.days.map((d) => [d.day, [planTokens(d.spend)]]))} label={`Daily subscription tokens, ${tokens(planTokens(s.spend))} in all`} />
          </div>
        )}
      </div>
    </section>
  );
}

// ---------------------------------------------------------------------------
// Assembly line
// ---------------------------------------------------------------------------

const timeParts = (st: StepStats) => {
  if (st.queue_ms.n) {
    const human = st.human_wait_ms.p50;
    return [
      { value: st.queue_ms.p50, color: "var(--fg-subtle)", label: "Waiting for a runner" },
      { value: Math.max(0, st.work_ms.p50 - human), color: "var(--chart-attended)", label: "Agent working" },
      { value: human, color: "var(--chart-unplanned)", label: "Waiting on a person" },
    ];
  }
  return [{ value: st.ms.p50, color: "var(--chart-attended)", label: "Working" }];
};
const total = (st: StepStats) => timeParts(st).reduce((a, p) => a + p.value, 0);
const costKey = (st: StepStats, byDollars: boolean) => (byDollars ? st.spend.cost_usd : planTokens(st.spend));

function AssemblyLine({ workflow, stats, label }: { workflow: Workflow; stats: WorkflowStats; label: (id: string, f?: string) => string }) {
  // The workflow's own order; steps it no longer has (from older versions) go last.
  const byId = new Map(stats.steps.map((s) => [s.id, s]));
  const ids = [...workflow.steps.map((s) => s.id), ...stats.steps.map((s) => s.id).filter((id) => !workflow.steps.some((s) => s.id === id))];
  const ran = stats.steps.filter((s) => s.runs >= MIN_RUNS);
  const maxTime = Math.max(...ran.map(total), 1);
  const slowest = ran.length > 1 ? ran.reduce((a, b) => (total(b) > total(a) ? b : a)).id : null;
  const byDollars = stats.steps.some((s) => s.spend.cost_usd > 0);
  const costly = ran.filter((s) => costKey(s, byDollars) > 0);
  const costliest = costly.length > 1 ? costly.reduce((a, b) => (costKey(b, byDollars) > costKey(a, byDollars) ? b : a)).id : null;
  const kinds = new Map(workflow.steps.map((s) => [s.id, s.type]));
  return (
    <>
      <div className="ins-line" role="list">
        {ids.map((id, i) => {
          const st = byId.get(id);
          const kind = kinds.get(id);
          const ok = st ? rate(st.ok, st.runs) : null;
          const failRate = st ? rate(st.failed, st.runs) : null;
          return (
            <div key={id} className="ins-line-item" role="listitem">
              {i > 0 && <span className="ins-line-arrow" aria-hidden><Icon name="dot" size={8} /></span>}
              <div className={cx("ins-station", id === slowest && "slowest", id === costliest && "costliest", !st?.runs && "idle", !kind && "gone")}>
                <div className="ins-station-head">
                  <span className="ins-station-n">{i + 1}</span>
                  <span className="ins-station-name" title={label(id, st?.label)}>{label(id, st?.label)}</span>
                </div>
                {st && st.runs > 0 ? (
                  <>
                    <div className="ins-station-mid">
                      <Ring size={48} stroke={5} empty={st.runs < MIN_RUNS} center={st.runs < MIN_RUNS ? "–" : pct(ok)}
                        segments={[{ value: ok ?? 0, color: toneColor[rateTone(ok, st.runs)], label: "Done" }]}
                        label={`${label(id, st.label)}: ${st.ok} of ${st.runs} done`} />
                      <div className="ins-station-nums">
                        <strong>{duration(total(st))}</strong>
                        <span className={cx(failRate && failRate >= 0.2 ? "text-danger" : failRate && failRate > 0.05 ? "text-warning" : "subtle")}>
                          {!st.failed ? "No failures" : st.runs < MIN_RUNS ? `${st.failed} of ${st.runs} failed` : `${pct(failRate)} failed`}
                        </span>
                      </div>
                    </div>
                    <SplitBar max={maxTime} parts={timeParts(st)} label={timeParts(st).map((p) => `${p.label} ${duration(p.value)}`).join(", ")} />
                    <div className="ins-station-foot">
                      <span title={spendLong(st.spend)}>{spendShort(st.spend) === "Nothing" ? <span className="subtle">No spend</span> : spendShort(st.spend)}</span>
                      {st.interventions > 0 && <span className="text-warning" title={`${st.unplanned_interventions} unplanned`}><Icon name="user" size={11} /> {st.interventions} stepped in</span>}
                      {st.fallbacks > 0 && <span className="subtle" title="Times a runner moved on to the family's next model">{plural(st.fallbacks, "fallback")}</span>}
                    </div>
                    {(id === slowest || id === costliest) && (
                      <div className="ins-station-tags">
                        {id === slowest && <Badge tone="warning" icon="clock">Slowest</Badge>}
                        {id === costliest && <Badge tone="accent" icon="gauge">Costliest</Badge>}
                      </div>
                    )}
                  </>
                ) : (
                  <span className="subtle ins-station-idle">{st?.skipped ? `Skipped ${plural(st.skipped, "time")}` : "Not reached"}</span>
                )}
              </div>
            </div>
          );
        })}
      </div>
      <Legend series={[
        { key: "q", label: "Waiting for a runner", color: "var(--fg-subtle)" },
        { key: "w", label: "Working", color: "var(--chart-attended)" },
        { key: "h", label: "Waiting on a person", color: "var(--chart-unplanned)" },
      ]} />
    </>
  );
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

function Models({ stats, label }: { stats: WorkflowStats; label: (id: string, f?: string) => string }) {
  const byStep = new Map<string, ModelStats[]>();
  for (const m of stats.models) byStep.set(m.step_id, [...(byStep.get(m.step_id) ?? []), m]);
  return (
    <section className="ins-sec">
      <h4 className="ins-h">Models <span className="subtle">what each choice got, per agent step</span></h4>
      {[...byStep.entries()].map(([step, models]) => {
        const rated = models.filter((m) => m.runs >= MIN_RUNS);
        const best = rated.length > 1 ? rated.reduce((a, b) => ((rate(b.unattended, b.runs) ?? 0) > (rate(a.unattended, a.runs) ?? 0) ? b : a)) : null;
        return (
          <div key={step} className="ins-models">
            <span className="ins-sub">{label(step)}</span>
            <table className="table ins-table">
              <thead><tr><th>Model</th><th className="num">Runs</th><th className="num">Succeeded</th><th className="num">On its own</th><th className="num">Median work</th><th className="num">Spend</th></tr></thead>
              <tbody>
                {models.sort((a, b) => b.runs - a.runs).map((m) => {
                  const un = rate(m.unattended, m.runs);
                  return (
                    <tr key={m.harness + m.model}>
                      <td>
                        <span className="ins-model">{m.model || "Default"}{m === best && <Badge tone="success" icon="check">Best</Badge>}</span>
                        <span className="subtle ins-model-sub">{[m.harness.replace(/_/g, " "), m.family].filter(Boolean).join(" · ")}</span>
                      </td>
                      <td className="num">{m.runs}</td>
                      <td className="num">{m.runs < MIN_RUNS ? "–" : pct(rate(m.ok, m.runs))}</td>
                      <td className="num"><RateCell r={un} runs={m.runs} /></td>
                      <td className="num">{m.work_ms.n ? duration(m.work_ms.p50) : "–"}</td>
                      <td className="num" title={spendLong(m.spend)}>{spendShort(m.spend) === "Nothing" ? "–" : spendShort(m.spend, true)}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        );
      })}
    </section>
  );
}

function RateCell({ r, runs }: { r: number | null; runs: number }) {
  if (runs < MIN_RUNS) return <span className="subtle" title="Fewer than 3 runs">–</span>;
  return (
    <span className="ins-ratecell">
      <span className="ins-ratecell-bar"><span style={{ width: `${(r ?? 0) * 100}%`, background: toneColor[rateTone(r, runs)] }} /></span>
      {pct(r)}
    </span>
  );
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

function Versions({ versions }: { versions: VersionStats[] }) {
  const [a, setA] = useState(versions[1].version);
  const [b, setB] = useState(versions[0].version);
  const va = versions.find((v) => v.version === a) ?? versions[1];
  const vb = versions.find((v) => v.version === b) ?? versions[0];
  const perRun = (v: VersionStats, f: (v: VersionStats) => number) => (v.runs ? f(v) / v.runs : 0);
  const rows: { label: string; a: string; b: string; delta: React.ReactNode }[] = useMemo(() => {
    const enough = va.runs >= MIN_RUNS && vb.runs >= MIN_RUNS;
    const vs = `vs v${va.version}`;
    const rateRow = (label: string, f: (v: VersionStats) => number | null, better: "up" | "down") =>
      ({ label, a: va.runs < MIN_RUNS ? "–" : pct(f(va)), b: vb.runs < MIN_RUNS ? "–" : pct(f(vb)), delta: <Delta now={f(vb)} before={f(va)} kind="rate" better={better} enough={enough} vs={vs} /> });
    const out = [
      { label: "Runs", a: String(va.runs), b: String(vb.runs), delta: null },
      rateRow("Succeeded on its own", unattendedRate, "up"),
      rateRow("Succeeded", successRate, "up"),
      rateRow("People stepped in", interventionRate, "down"),
      { label: "Median time", a: va.duration_ms.n ? duration(va.duration_ms.p50) : "–", b: vb.duration_ms.n ? duration(vb.duration_ms.p50) : "–", delta: <Delta now={vb.duration_ms.p50} before={va.duration_ms.p50} kind="ratio" better="down" enough={enough} vs={vs} /> },
      { label: "Slow end (p95)", a: va.duration_ms.n ? duration(va.duration_ms.p95) : "–", b: vb.duration_ms.n ? duration(vb.duration_ms.p95) : "–", delta: <Delta now={vb.duration_ms.p95} before={va.duration_ms.p95} kind="ratio" better="down" enough={enough} vs={vs} /> },
    ];
    if (hasApi(va.spend) || hasApi(vb.spend))
      out.push({ label: "API spend a run", a: usd(perRun(va, (v) => v.spend.cost_usd)), b: usd(perRun(vb, (v) => v.spend.cost_usd)), delta: <Delta now={perRun(vb, (v) => v.spend.cost_usd)} before={perRun(va, (v) => v.spend.cost_usd)} kind="ratio" better="down" enough={enough} vs={vs} /> });
    if (hasPlan(va.spend) || hasPlan(vb.spend))
      out.push({ label: "Plan tokens a run", a: tokens(Math.round(perRun(va, (v) => planTokens(v.spend)))), b: tokens(Math.round(perRun(vb, (v) => planTokens(v.spend)))), delta: <Delta now={perRun(vb, (v) => planTokens(v.spend))} before={perRun(va, (v) => planTokens(v.spend))} kind="ratio" better="down" enough={enough} vs={vs} /> });
    return out;
  }, [va, vb]);
  const pick = (value: number, set: (v: number) => void, name: string) => (
    <select className="wf-select" value={value} onChange={(e) => set(Number(e.target.value))} aria-label={name}>
      {versions.map((v) => <option key={v.version} value={v.version}>v{v.version} · {plural(v.runs, "run")}</option>)}
    </select>
  );
  return (
    <section className="ins-sec">
      <h4 className="ins-h">Versions <span className="subtle">side by side</span></h4>
      <table className="table ins-table ins-versions">
        <thead>
          <tr>
            <th />
            <th className="num">{pick(a, setA, "First version")}</th>
            <th className="num">{pick(b, setB, "Second version")}</th>
            <th className="num">Change</th>
          </tr>
        </thead>
        <tbody>
          <tr className="subtle"><td>First run in window</td><td className="num">{va.first_run ? dayLabel(localDay(va.first_run)) : "–"}</td><td className="num">{vb.first_run ? dayLabel(localDay(vb.first_run)) : "–"}</td><td /></tr>
          {rows.map((r) => <tr key={r.label}><td>{r.label}</td><td className="num">{r.a}</td><td className="num">{r.b}</td><td className="num">{r.delta}</td></tr>)}
        </tbody>
      </table>
    </section>
  );
}

// ---------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------

const ago = (iso: string) => { const t = timeAgo(iso); return /^\d+[mhd]$/.test(t) ? `${t} ago` : t; };

function Failures({ stats, label, onOpenRun }: { stats: WorkflowStats; label: (id: string, f?: string) => string; onOpenRun: (id: string) => void }) {
  return (
    <section className="ins-sec">
      <h4 className="ins-h">What fails <span className="subtle">{stats.failures.length ? "most often first" : ""}</span></h4>
      {!stats.failures.length && <p className="subtle ins-ok"><Icon name="check" size={13} /> Nothing failed in this window.</p>}
      <div className="ins-failures">
        {stats.failures.map((f) => (
          <div key={f.step_id + f.error} className="ins-failure">
            <span className="ins-failure-count" title={`${f.count} times`}>×{f.count}</span>
            <div className="ins-failure-body">
              <span className="ins-failure-step"><Icon name="octagon" size={12} className="text-danger" /> {label(f.step_id, f.label)}</span>
              <code className="ins-failure-error">{f.error}</code>
              <span className="subtle ins-failure-when">Last {ago(f.last_seen)}{f.first_seen !== f.last_seen && ` · first ${ago(f.first_seen)}`}</span>
            </div>
            <Button size="sm" icon="external" onClick={() => onOpenRun(f.sample_run)} title="Open the latest run it happened in">Run</Button>
          </div>
        ))}
      </div>
    </section>
  );
}
