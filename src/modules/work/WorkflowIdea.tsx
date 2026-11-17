// The workflow idea on a finished task: what a small model thinks it could
// have been, with a button that hands it to the workflow builder.

import { useQueryClient } from "@tanstack/react-query";
import { Button } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { keys } from "../../lib/query";
import type { QueueItem } from "../../lib/types";
import { ideaMessage, ideaOf, openIdea } from "../workflows/ideas";

export function WorkflowIdeaCard({ item }: { item: QueueItem }) {
  const qc = useQueryClient();
  const idea = ideaOf(item);
  if (!idea) return null;
  const dismiss = async () => {
    const next = await api.workflowIdeaDismiss(item.id).catch(() => null);
    if (next) qc.setQueryData(keys.item(item.id), next);
    qc.invalidateQueries({ queryKey: ["workflow-ideas"] });
  };
  return (
    <section className="item-workflows wf-idea" aria-label="Workflow idea">
      <div className="item-workflows-head">
        <Icon name="workflow" size={12} /> Could be a workflow
      </div>
      <div className="wf-idea-name">{idea.name}</div>
      {idea.why && <p className="wf-idea-why">{idea.why}</p>}
      <ol className="wf-idea-steps">
        <li><strong>When</strong> {idea.trigger}</li>
        {idea.steps.map((s, i) => <li key={i}>{s}</li>)}
      </ol>
      <div className="wf-idea-actions">
        <Button size="sm" variant="primary" icon="workflow" onClick={() => { openIdea(ideaMessage(item, idea)); void dismiss(); }}>Build it</Button>
        <Button size="sm" variant="ghost" onClick={dismiss}>Not now</Button>
      </div>
    </section>
  );
}
