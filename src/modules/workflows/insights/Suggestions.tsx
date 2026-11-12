// Suggestions at the top of a workflow's Insights: what tab's rules read off
// its runs (a cheaper or stronger model family, more time, a higher hourly
// limit, a placement, approvals always granted), each with its evidence.
// "Apply as draft" loads the changed workflow into the builder unsaved, for
// the person to review and save; "Ask the assistant" hands it to the
// builder's assistant. Approvals are only pointed out: allowing a step on
// the workflow stays the person's own switch.

import { useQuery } from "@tanstack/react-query";
import { Badge, Button } from "../../../design/components";
import { Icon } from "../../../design/icons";
import { api } from "../../../lib/api";
import type { Suggestion, SuggestionFact, SuggestionKind, Tone, Workflow } from "../../../lib/types";
import { duration, pct, tokens, usd } from "./format";
import { useWindowDays } from "./useInsights";
import "../improve.css";

export function useWorkflowSuggestions(workflowId: string | null | undefined) {
  const days = useWindowDays();
  return useQuery({
    queryKey: ["workflow-suggestions", workflowId, days],
    queryFn: () => api.workflowSuggestions(workflowId!, days),
    enabled: !!workflowId,
    staleTime: 30_000,
  });
}

const look: Record<SuggestionKind, { icon: string; tone: Tone; tag: string }> = {
  stronger_model: { icon: "alert", tone: "warning", tag: "Steadier" },
  more_time: { icon: "clock", tone: "warning", tag: "Time limit" },
  rate_limit: { icon: "gauge", tone: "info", tag: "Hourly limit" },
  placement: { icon: "server", tone: "info", tag: "Placement" },
  cheaper_model: { icon: "zap", tone: "success", tag: "Cheaper" },
  always_approved: { icon: "shield", tone: "neutral", tag: "Only you can" },
};

function value(f: SuggestionFact, v: number): string {
  switch (f.unit) {
    case "rate": return pct(v);
    case "ms": return duration(v);
    case "usd": return usd(v);
    case "tokens": return tokens(Math.round(v));
    default: return String(Math.round(v));
  }
}

function Fact({ f }: { f: SuggestionFact }) {
  return (
    <span className="ins-fact">
      <span className="ins-fact-label">{f.label}</span>
      <strong>{value(f, f.value)}{f.of != null && <span className="subtle"> of {value(f, f.of)}</span>}</strong>
    </span>
  );
}

export function Suggestions({ workflow, onApply, onAsk }: {
  workflow: Workflow;
  onApply: (s: Suggestion) => void;
  onAsk: (s: Suggestion) => void;
}) {
  const { data = [], isLoading } = useWorkflowSuggestions(workflow.id || null);
  if (isLoading || !data.length) return null;
  const stepName = (id?: string | null) => (id ? workflow.steps.find((s) => s.id === id)?.label || id : null);
  return (
    <section className="ins-sec" aria-labelledby="ins-suggestions-h">
      <h4 className="ins-h" id="ins-suggestions-h">
        Suggestions <span className="subtle">from its runs; nothing changes until you save</span>
      </h4>
      <div className="ins-suggestions">
        {data.map((s) => {
          const l = look[s.kind];
          return (
            <article key={s.id} className={`ins-suggestion tone-${l.tone}`}>
              <span className="ins-suggestion-icon" aria-hidden><Icon name={l.icon} size={14} /></span>
              <div className="ins-suggestion-body">
                <div className="ins-suggestion-head">
                  <strong>{s.title}</strong>
                  <Badge tone={l.tone}>{l.tag}</Badge>
                  {stepName(s.step_id) && <span className="subtle ins-suggestion-step">{stepName(s.step_id)}</span>}
                </div>
                <p className="ins-suggestion-why">{s.because}</p>
                <div className="ins-facts">{s.facts.map((f) => <Fact key={f.label} f={f} />)}</div>
                <div className="ins-suggestion-actions">
                  {s.change && (
                    <Button size="sm" variant="primary" icon="edit" onClick={() => onApply(s)} title="Open the changed workflow in the builder, unsaved, to review and save">
                      Apply as draft
                    </Button>
                  )}
                  {s.kind === "always_approved" ? (
                    <span className="subtle ins-suggestion-note"><Icon name="lock" size={11} /> Allow it in the trigger settings if you want; agents and suggestions can't.</span>
                  ) : (
                    <Button size="sm" icon="sparkles" onClick={() => onAsk(s)} title="Hand this to the builder's assistant, with the evidence and the workflow">
                      Ask the assistant
                    </Button>
                  )}
                </div>
              </div>
            </article>
          );
        })}
      </div>
    </section>
  );
}
