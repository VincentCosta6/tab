// The factory floor: every workflow at a glance. Hub-wide rings up top
// (how often work finishes with nobody stepping in, how often it succeeds,
// how often a person has to), then a card per workflow, what isn't
// working first.

import { Badge, cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import type { FactoryWorkflow, Workflow, WorkflowStats } from "../../../lib/types";
import { Delta, Legend, Ring, StackedBars } from "./charts";
import {
  duration, hasApi, spendParts, hasPlan, interventionRate, MIN_RUNS, pct, planShare, planTokens, plural, rate, rateTone, spendLong, successRate, tokens,
  unattendedRate, usd, windowDays,
} from "./format";
import { HealthBadge, outcomeSeries, outcomeValues, RateRings, useInsightsLive, WindowPicker } from "./parts";
import { toneColor } from "./charts";
import { useFactory } from "./useInsights";
import "./insights.css";

export function Factory({ workflows, onOpen }: { workflows: Workflow[]; onOpen: (id: string) => void }) {
  const { data: f, isLoading, error } = useFactory();
  useInsightsLive();
  const needLook = f?.workflows.filter((w) => w.health === "failing" || w.health === "degraded").length ?? 0;
  return (
    <section className="wf-main ins-factory">
      <header className="wf-bar">
        <h2 className="wf-allruns-title">Factory</h2>
        <span className="subtle ins-bar-note">How every workflow ran, in the last {f?.days ?? "…"} days</span>
        <span className="wf-bar-spacer" />
        <WindowPicker />
      </header>
      <div className="ins-scroll">
        {error && <p className="wf-error"><Icon name="alert" size={13} /> {String(error)}</p>}
        {isLoading && !f && <p className="subtle wf-pad">Adding up runs…</p>}
        {f && (
          <>
            <HubRow totals={f.totals} previous={f.previous_totals} from={f.from} days={f.days} />
            <div className="ins-floor-head">
              <h3>Workflows</h3>
              <span className={cx("subtle", needLook > 0 && "text-warning")}>
                {needLook ? `${plural(needLook, "needs a look", "need a look")}, worst first` : f.workflows.length ? "Everything with enough runs is healthy" : ""}
              </span>
              <span className="wf-bar-spacer" />
            </div>
            {!f.workflows.length && <p className="subtle wf-pad">No workflows yet. Build one and its runs show up here.</p>}
            <div className="ins-grid">
              {f.workflows.map((w) => <WorkflowCard key={w.id} card={w} from={f.from} days={f.days} def={workflows.find((x) => x.id === w.id)} onOpen={() => onOpen(w.id)} />)}
            </div>
          </>
        )}
      </div>
    </section>
  );
}

function HubRow({ totals: t, previous: p, from, days }: { totals: WorkflowStats; previous: WorkflowStats; from: string; days: number }) {
  const enough = p.runs >= MIN_RUNS;
  const thin = t.runs < MIN_RUNS;
  return (
    <div className="ins-hub card">
      <div className="ins-hub-rings">
        <RateRings counts={t} size={84} headlineSize={112} tone={toneColor[rateTone(unattendedRate(t), t.runs)]} deltas={[
          <Delta key="u" now={unattendedRate(t)} before={unattendedRate(p)} kind="rate" better="up" enough={enough && !thin} />,
          <Delta key="s" now={successRate(t)} before={successRate(p)} kind="rate" better="up" enough={enough && !thin} />,
          <Delta key="i" now={interventionRate(t)} before={interventionRate(p)} kind="rate" better="down" enough={enough && !thin} />,
        ]} />
      </div>
      <div className="ins-hub-chart">
        <span className="ins-stat-label">Every day</span>
        <StackedBars days={windowDays(from, days)} values={outcomeValues(t.days)} series={outcomeSeries} height={150}
          label={`Daily runs across every workflow: ${t.unattended} on their own, ${t.succeeded - t.unattended} with help, ${t.failed} failed`} />
        <Legend series={outcomeSeries} />
      </div>
      <div className="ins-hub-stats">
        <Stat label="Runs" value={String(t.runs)} hint={[t.in_progress && `${t.in_progress} running`, t.held_back && `${t.held_back} held back`].filter(Boolean).join(" · ")}>
          <Delta now={t.runs} before={p.runs} kind="ratio" better="none" enough={p.runs > 0} />
        </Stat>
        <Stat label="Median time, start to finish" value={t.duration_ms.n ? duration(t.duration_ms.p50) : "–"} hint={t.duration_ms.n ? `Slow end ${duration(t.duration_ms.p95)}` : undefined}>
          <Delta now={t.duration_ms.n ? t.duration_ms.p50 : null} before={p.duration_ms.n ? p.duration_ms.p50 : null} kind="ratio" better="down" enough={enough} />
        </Stat>
        <Stat label="Spend" value={hasApi(t.spend) ? usd(t.spend.cost_usd) : hasPlan(t.spend) ? `${tokens(planTokens(t.spend))}` : "$0.00"}
          unit={!hasApi(t.spend) && hasPlan(t.spend) ? "plan tokens" : undefined}
          hint={hasApi(t.spend) && hasPlan(t.spend) ? `+ ${tokens(planTokens(t.spend))} plan tokens (${planShare(t.spend.plan_share)})` : hasPlan(t.spend) ? planShare(t.spend.plan_share) + " of plan allowance" : "API keys"}
          title={spendLong(t.spend)}>
          {hasApi(t.spend)
            ? <Delta now={t.spend.cost_usd} before={p.spend.cost_usd} kind="ratio" better="down" enough={p.runs > 0} />
            : <Delta now={planTokens(t.spend)} before={planTokens(p.spend)} kind="ratio" better="down" enough={p.runs > 0} />}
        </Stat>
      </div>
    </div>
  );
}

function Stat({ label, value, unit, hint, title, children }: { label: string; value: string; unit?: string; hint?: string; title?: string; children?: React.ReactNode }) {
  return (
    <div className="ins-stat" title={title}>
      <span className="ins-stat-label">{label}</span>
      <span className="ins-stat-value">{value}{unit && <small> {unit}</small>}</span>
      {hint && <span className="ins-stat-hint">{hint}</span>}
      {children}
    </div>
  );
}

function WorkflowCard({ card: w, from, days, def, onOpen }: { card: FactoryWorkflow; from: string; days: number; def?: Workflow; onOpen: () => void }) {
  const s = w.stats;
  const thin = s.runs < MIN_RUNS;
  const un = unattendedRate(s);
  const ringColor = w.health === "unknown" ? toneColor[rateTone(un, s.runs)] : toneColor[w.health === "healthy" ? "success" : w.health === "degraded" ? "warning" : "danger"];
  const top = s.failures[0];
  const stepLabel = (id: string, fallback: string) => fallback || def?.steps.find((x) => x.id === id)?.label || id;
  const unplanned = rate(s.unplanned, s.runs);
  const slots = windowDays(from, days);
  return (
    <button className={cx("ins-card", `health-${w.health}`)} onClick={onOpen} aria-label={`${w.name}: open insights`}>
      <header className="ins-card-head">
        <span className="ins-card-name">{w.name}</span>
        {!w.enabled && <Badge>Off</Badge>}
        <HealthBadge health={w.health} />
      </header>
      <div className="ins-card-body">
        <Ring size={72} empty={thin}
          center={thin ? "–" : pct(un)} sub={thin ? (s.runs ? plural(s.runs, "run") : "no runs") : "on its own"}
          segments={[{ value: un ?? 0, color: ringColor, label: "On its own" }]}
          label={thin ? `${w.name}: not enough runs to rate` : `${w.name}: ${pct(un)} of runs succeeded with nobody stepping in`} />
        <div className="ins-card-chart">
          {s.runs > 0
            ? <StackedBars compact days={slots} values={outcomeValues(s.days)} series={outcomeSeries} height={58} label={`Daily runs of ${w.name}: ${s.unattended} on their own, ${s.succeeded - s.unattended} with help, ${s.failed} failed`} />
            : <div className="ins-card-none subtle">No runs in the last {days} days</div>}
          <span className="ins-card-caption subtle">
            {s.runs ? <>{plural(s.runs, "run")} · {pct(successRate(s))} succeeded</> : w.enabled ? "Waiting for its trigger" : "Turned off"}
          </span>
        </div>
      </div>
      <dl className="ins-card-stats">
        <div title={`${s.attended - s.unplanned} planned approvals, ${s.unplanned} unplanned`}><dt>Stepped in</dt><dd>{s.runs ? pct(interventionRate(s)) : "–"}{s.attended > 0 && !s.unplanned && <span className="subtle ins-dd-note"> planned</span>}</dd></div>
        <div><dt>Median</dt><dd>{s.duration_ms.n ? duration(s.duration_ms.p50) : "–"}</dd></div>
        <div title={spendLong(s.spend)}><dt>Spend</dt><dd>{spendParts(s.spend, true).length ? spendParts(s.spend, true).map((p, i) => <span key={i} className="ins-nowrap">{i > 0 && " + "}{p}</span>) : "–"}</dd></div>
      </dl>
      {w.health !== "healthy" && w.health !== "unknown" && top && (
        <p className={cx("ins-card-problem", w.health === "failing" ? "tone-danger" : "tone-warning")}>
          <Icon name={w.health === "failing" ? "octagon" : "alert"} size={13} />
          <span><strong>{stepLabel(top.step_id, top.label)}</strong> failed {plural(top.count, "time")}: {top.error}</span>
        </p>
      )}
      {(w.health === "healthy" || w.health === "unknown") && unplanned != null && unplanned >= 0.25 && !thin && (
        <p className="ins-card-problem tone-warning">
          <Icon name="user" size={13} />
          <span>A person had to step in on {pct(unplanned)} of runs</span>
        </p>
      )}
    </button>
  );
}
