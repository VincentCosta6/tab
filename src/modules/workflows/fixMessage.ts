// The message the builder's assistant gets when the person asks it to fix
// a failure or work on a suggestion: what went wrong (or what's suggested),
// the evidence, the failing step's input, output and error from the latest
// run it happened in, and the workflow, asking for a new version to review.
// The person sees the whole message and sends it themselves.

import { api } from "../../lib/api";
import type { FailureGroup, StepRun, Suggestion, SuggestionFact, Workflow, WorkflowRun } from "../../lib/types";

/** What a prepared message is about, for the card that shows it before sending. */
export interface Prepared { label: string; text: string }

const MAX = 4000;

function block(v: unknown): string {
  if (v == null || (typeof v === "object" && !Array.isArray(v) && !Object.keys(v as object).length)) return "(nothing)";
  let s = typeof v === "string" ? v : JSON.stringify(v, null, 2);
  if (s.length > MAX) s = `${s.slice(0, MAX)}\n… (cut at ${MAX} characters)`;
  return "```\n" + s + "\n```";
}

const when = (iso: string) => new Date(iso).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });

function workflowJson(w: Workflow): string {
  return "```json\n" + JSON.stringify(w, null, 2) + "\n```";
}

const ask = (w: Workflow) =>
  `Please propose a fix as a new version of "${w.name}" and save it with tab_save_workflow (id ${w.id}). ` +
  "Keep everything that works, change only what this needs, and tell me in a sentence or two what you changed and why. " +
  "Don't turn it on or allow destructive steps or commands; I'll review it and do that.";

/** The latest run the failure happened in, to show the step as it ran. */
async function sampleRun(runId: string, workflowId: string): Promise<WorkflowRun | null> {
  const view = await api.workflowRunView(runId).catch(() => null);
  if (view) return view.run;
  const runs = await api.workflowRuns(workflowId, 500).catch(() => []);
  return runs.find((r) => r.id === runId) ?? null;
}

export async function failureMessage(w: Workflow, f: FailureGroup, days: number): Promise<Prepared> {
  const run = await sampleRun(f.sample_run, w.id);
  const step: StepRun | undefined = run?.steps.find((s) => s.id === f.step_id);
  const label = f.label || w.steps.find((s) => s.id === f.step_id)?.label || f.step_id;
  const lines = [
    `This workflow keeps failing at one step. ${ask(w)}`,
    "",
    "## The failure",
    `- Step: ${label} (id \`${f.step_id}\`)`,
    `- Error (numbers and ids taken out): ${f.error}`,
    `- ${f.count} time${f.count === 1 ? "" : "s"} in the last ${days} days; first ${when(f.first_seen)}, last ${when(f.last_seen)}`,
    "",
    `## The latest run it happened in (${f.sample_run}${run ? `, v${run.version}, ${when(run.started_at)}` : ""})`,
  ];
  if (step) {
    lines.push("Error:", block(step.error), "Input:", block(step.input), "Output:", block(step.output));
  } else {
    lines.push("That run isn't kept any more; read others with tab_workflow_failures and tab_get_workflow_run.");
  }
  lines.push("", `## The workflow now (v${w.version})`, workflowJson(w), "", "tab_workflow_failures and tab_workflow_stats have the rest of its numbers.");
  return { label: `Fix: ${label} — ${f.error}`, text: lines.join("\n") };
}

function factText(f: SuggestionFact): string {
  const n = (v: number) => {
    switch (f.unit) {
      case "rate": return `${Math.round(v * 100)}%`;
      case "ms": return `${Math.round(v / 1000)} s`;
      case "usd": return `$${v.toFixed(2)}`;
      case "tokens": return `${Math.round(v)} tokens`;
      default: return String(Math.round(v));
    }
  };
  return `- ${f.label}: ${n(f.value)}${f.of != null ? ` of ${n(f.of)}` : ""}`;
}

export function suggestionMessage(w: Workflow, s: Suggestion, days: number): Prepared {
  const step = s.step_id ? w.steps.find((x) => x.id === s.step_id) : undefined;
  const lines = [
    `tab's insights suggest a change to this workflow. ${ask(w)}`,
    "",
    "## The suggestion",
    `- ${s.title}${step ? ` (step \`${step.id}\`)` : ""}`,
    `- Why: ${s.because}`,
    ...s.facts.map(factText),
    `- From its runs over the last ${days} days.`,
  ];
  if (s.change) lines.push("", "The suggested version, for reference (you may do better):", workflowJson(s.change));
  lines.push("", `## The workflow now (v${w.version})`, workflowJson(w));
  return { label: s.title, text: lines.join("\n") };
}
