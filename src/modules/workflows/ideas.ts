// Workflow ideas: when a chat task is marked done, a small model asks
// whether the work could have been a workflow and leaves the idea on the
// task (`metadata.workflow_idea`, see `agents::workflow_idea`). Building one
// opens a new workflow in the Workflows tab with a message for the builder's
// assistant, which the person reads and sends themselves.

import { showTab } from "../../lib/nav";
import type { QueueItem } from "../../lib/types";
import type { Prepared } from "./fixMessage";

export interface WorkflowIdea {
  verdict: "good" | "maybe";
  name: string;
  why: string;
  trigger: string;
  steps: string[];
  similar: string[];
  at: string;
}

export function ideaOf(item: QueueItem): WorkflowIdea | null {
  const idea = (item.metadata as { workflow_idea?: WorkflowIdea } | null)?.workflow_idea;
  return idea?.name && idea.steps?.length ? idea : null;
}

export function ideaMessage(item: QueueItem, idea: WorkflowIdea): Prepared {
  const lines = [
    `I finished a task by hand that tab thinks could be a workflow next time. Please draft "${idea.name}" ` +
      "and save it with tab_save_workflow. Check the trigger and each step against the connectors and actions I have, " +
      "and dry-run it if you can. If a part can't be done, say so and do the rest. " +
      "Don't turn it on or allow destructive steps or commands; I'll review it and do that.",
    "",
    "## The idea",
    `- Why: ${idea.why}`,
    `- Trigger: ${idea.trigger}`,
    "- Steps:",
    ...idea.steps.map((s, i) => `  ${i + 1}. ${s}`),
  ];
  if (idea.similar?.length) lines.push("", "## Similar tasks I did by hand", ...idea.similar.map((t) => `- ${t}`));
  lines.push("", "## The task it came from", `- ${item.title} (tab item ${item.id})`);
  return { label: idea.name, text: lines.join("\n") };
}

let pendingIdea: Prepared | null = null;

/** The idea the Workflows tab should open when it next mounts or hears `tab:open-idea`. */
export function takePendingIdea() {
  const p = pendingIdea;
  pendingIdea = null;
  return p;
}

export function openIdea(p: Prepared) {
  pendingIdea = p;
  showTab("workflows");
  window.dispatchEvent(new CustomEvent("tab:open-idea"));
}
