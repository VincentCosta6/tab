// Pieces of a run in the Runs panel: an agent step's time split (waiting
// for a runner, the agent working, waiting on you), what it ran on and
// spent, and who stepped in; a failed run's "Run again"; and the link from
// a re-run to the run it re-ran.

import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Badge, Button } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import type { InterventionKind, StepRun, WorkflowRun } from "../../lib/types";
import { SplitBar } from "./insights/charts";
import { duration, planShare, tokens, usd } from "./insights/format";
import "./improve.css";

/** The person said no at an approval (mirror `Run::declined`): neither a success nor a failure. */
export const isDeclined = (r: Pick<WorkflowRun, "status" | "steps">) => r.status === "stopped" && r.steps[r.steps.length - 1]?.status === "blocked";

const interventionLabel: Record<InterventionKind, string> = {
  approval: "You approved it", question: "The agent asked you something", message: "You wrote to the agent", stopped: "You stopped it", rerun: "Re-run",
};

/** An agent step's time, model, spend and interventions; nothing for other steps (their time is in the head). */
export function StepMetricsView({ step }: { step: StepRun }) {
  const m = step.metrics;
  const asked = step.interventions ?? [];
  if (!m && !asked.length) return null;
  const parts = m ? [
    { value: m.queue_ms, color: "var(--fg-subtle)", label: "In the queue" },
    { value: Math.max(0, m.work_ms - m.human_wait_ms), color: "var(--chart-attended)", label: "Agent working" },
    { value: m.human_wait_ms, color: "var(--chart-unplanned)", label: "Waiting on you" },
  ] : [];
  const total = m ? m.queue_ms + m.work_ms : 0;
  const ran = m ? [m.model || (m.harness ? "Harness default" : ""), m.harness.replace(/_/g, " "), m.family && `${m.family} family`].filter(Boolean).join(" · ") : "";
  const spend = !m ? null
    : m.billing === "subscription" ? `${tokens(m.input_tokens + m.output_tokens)} plan tokens${m.plan_share > 0 ? ` · ${planShare(m.plan_share)}` : ""}`
    : m.cost_usd > 0 ? `${usd(m.cost_usd)} · ${tokens(m.input_tokens + m.output_tokens)} tokens`
    : m.input_tokens + m.output_tokens > 0 ? `${tokens(m.input_tokens + m.output_tokens)} tokens` : null;
  return (
    <div className="wf-step-metrics">
      {m && total > 0 && (
        <>
          <SplitBar max={total} parts={parts} height={7} label={parts.map((p) => `${p.label} ${duration(p.value)}`).join(", ")} />
          <dl className="wf-step-metrics-times">
            {parts.map((p) => (
              <div key={p.label}><dt><i style={{ background: p.color }} />{p.label}</dt><dd>{p.value > 0 ? duration(p.value) : <span className="subtle">none</span>}</dd></div>
            ))}
          </dl>
        </>
      )}
      {m && (ran || spend || m.fallbacks > 0) && (
        <div className="wf-step-metrics-meta">
          {ran && <span title="What ran it"><Icon name="bot" size={11} /> {ran}</span>}
          {spend && <span title={m.billing === "subscription" ? "On a subscription: usage, never priced" : "Billed through an API key"}><Icon name="gauge" size={11} /> {spend}</span>}
          {m.fallbacks > 0 && <span title="Times the runner moved on to the family's next model">{m.fallbacks} fallback{m.fallbacks === 1 ? "" : "s"}</span>}
          {m.attempts > 1 && <span title="Runners that claimed the job">{m.attempts} attempts</span>}
        </div>
      )}
      {asked.length > 0 && (
        <div className="wf-step-metrics-meta">
          {asked.map((i, n) => (
            <Badge key={n} tone={step.status === "blocked" && step.error ? "neutral" : i.kind === "approval" ? "info" : "warning"} icon="user" title={`${new Date(i.at).toLocaleString()}${i.kind === "approval" ? " · planned: the workflow asks for this" : " · unplanned"}`}>
              {step.status === "blocked" && i.kind === "approval" && step.error ? "You declined it" : interventionLabel[i.kind]}
            </Badge>
          ))}
        </div>
      )}
    </div>
  );
}

/** "Run again" on a failed run, and the new run once it starts. */
export function RerunButton({ run, onOpen }: { run: WorkflowRun; onOpen?: (id: string) => void }) {
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [started, setStarted] = useState<WorkflowRun | null>(null);
  if (run.status !== "failed" || run.dry_run) return null;
  const go = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await api.workflowRerun(run.id);
      setStarted(r);
      await qc.invalidateQueries({ queryKey: ["workflow-runs"] });
      await qc.invalidateQueries({ queryKey: ["workflow-insights"] });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="wf-rerun">
      {started ? (
        <span className="wf-rerun-done">
          <Icon name="check" size={13} className="text-success" /> Running it again
          {onOpen && <button className="link-button" onClick={() => onOpen(started.id)}>Open the new run</button>}
        </span>
      ) : (
        <Button size="sm" icon="refresh" onClick={go} disabled={busy} title="Run the workflow again as it's saved now, on the same item">
          {busy ? "Starting…" : "Run again"}
        </Button>
      )}
      {error && <span className="text-danger wf-rerun-error"><Icon name="alert" size={12} /> {error}</span>}
    </div>
  );
}

/** On a re-run: which run it re-ran, as a link. */
export function RerunOf({ run, onOpen }: { run: WorkflowRun; onOpen?: (id: string) => void }) {
  if (!run.rerun_of) return null;
  return (
    <p className="wf-rerun-of">
      <Icon name="refresh" size={12} /> A re-run of{" "}
      {onOpen ? <button className="link-button" onClick={() => onOpen(run.rerun_of!)}>an earlier failed run</button> : "an earlier failed run"}
    </p>
  );
}
