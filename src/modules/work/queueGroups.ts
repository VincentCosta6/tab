// Queue grouping and dimming for the open Work list.
//
// Anything actively being worked on (a started chat or an active workflow)
// stays at the top in "In progress" until it is marked done or snoozed — never
// pushed back down into Today / This week / Whenever when an agent finishes or
// goes idle. Within "In progress", items that need your attention sit first at
// full opacity; items actively working in the background sit right below them,
// dimmed so it's obvious they don't need you yet, and return to the top spot at
// full opacity as soon as they do.

import type { AgentActivity, ContextStatus, ItemWorkflowStatus, QueueItem, Urgency } from "../../lib/types";
import { afterBlockers } from "./followups";

export type QueueGroupKind = "in_progress" | Urgency;

export interface QueueGroup {
  kind: QueueGroupKind | null;
  items: QueueItem[];
}

export interface GroupQueueContext {
  nested: boolean;
  activity: ReadonlyMap<string, AgentActivity>;
  workflows: ReadonlyMap<string, ItemWorkflowStatus>;
  contextStatus: ReadonlyMap<string, ContextStatus[]>;
  isUnread: (a: AgentActivity | undefined) => boolean;
  children: ReadonlyMap<string, QueueItem[]>;
  /** Items animating out keep the placement they had when acted on. */
  pinnedInProgress?: ReadonlySet<string>;
  pinnedQuiet?: ReadonlySet<string>;
}

const urgencyOrder: Urgency[] = ["now", "today", "this_week", "whenever"];

function hasActiveWorkflow(w: ItemWorkflowStatus | undefined): boolean {
  return w?.state === "waiting_for_you" || w?.state === "running" || w?.state === "waiting_on_agent";
}

/**
 * True when an item (or any nested follow-up under it) is actively being
 * worked on: it has a started chat session or an active workflow run.
 */
export function isInProgress(
  itemId: string,
  activity: ReadonlyMap<string, AgentActivity>,
  workflows: ReadonlyMap<string, ItemWorkflowStatus>,
  children?: ReadonlyMap<string, QueueItem[]>,
): boolean {
  if (activity.has(itemId) || hasActiveWorkflow(workflows.get(itemId))) return true;
  const kids = children?.get(itemId);
  if (!kids?.length) return false;
  return kids.some((k) => isInProgress(k.id, activity, workflows, children));
}

/**
 * True when an item is actively working in the background and does not need
 * your attention right now (agent working/monitoring, or a workflow running
 * without waiting on you). Returns false whenever it needs you or is idle
 * waiting for you to complete or snooze it.
 */
export function isWorkingQuietly(
  itemId: string,
  activity: ReadonlyMap<string, AgentActivity>,
  workflows: ReadonlyMap<string, ItemWorkflowStatus>,
  contextStatus: ReadonlyMap<string, ContextStatus[]>,
  isUnread: (a: AgentActivity | undefined) => boolean,
): boolean {
  const w = workflows.get(itemId);
  if (w?.state === "waiting_for_you") return false;
  const a = activity.get(itemId);
  if (a) {
    if (a.status === "working" || a.status === "monitoring") return true;
    if (a.status === "needs_input" || a.status === "failed") return false;
    if (isUnread(a)) return false;
    if (contextStatus.get(itemId)?.some((c) => c.waiting_on === "you")) return false;
    return w?.state === "running" || w?.state === "waiting_on_agent";
  }
  return w?.state === "running" || w?.state === "waiting_on_agent";
}

function hasDescendantNeedingAttention(itemId: string, ctx: GroupQueueContext): boolean {
  const kids = ctx.children.get(itemId);
  if (!kids?.length) return false;
  return kids.some(
    (k) =>
      (isInProgress(k.id, ctx.activity, ctx.workflows) &&
        !isWorkingQuietly(k.id, ctx.activity, ctx.workflows, ctx.contextStatus, ctx.isUnread)) ||
      hasDescendantNeedingAttention(k.id, ctx),
  );
}

/**
 * True when a row should be dimmed: it is actively working in the background
 * and none of its nested follow-ups need your attention.
 */
export function isRowQuiet(itemId: string, ctx: GroupQueueContext): boolean {
  if (ctx.pinnedQuiet?.has(itemId)) return true;
  if (ctx.pinnedInProgress?.has(itemId)) return false;
  return (
    isWorkingQuietly(itemId, ctx.activity, ctx.workflows, ctx.contextStatus, ctx.isUnread) &&
    !hasDescendantNeedingAttention(itemId, ctx)
  );
}

/**
 * Groups the open queue: active work at the top ("In progress", with items
 * needing attention ahead of dimmed items actively running), followed by the
 * remaining items grouped by urgency. Flat views stay ungrouped.
 */
export function groupQueue(top: QueueItem[], ctx: GroupQueueContext): QueueGroup[] {
  if (!ctx.nested) return [{ kind: null, items: top }];
  const inProgress: QueueItem[] = [];
  const rest: QueueItem[] = [];
  for (const item of top) {
    if (ctx.pinnedInProgress?.has(item.id) || isInProgress(item.id, ctx.activity, ctx.workflows, ctx.children)) {
      inProgress.push(item);
    } else {
      rest.push(item);
    }
  }
  const out: QueueGroup[] = [];
  if (inProgress.length) {
    const needsYou: QueueItem[] = [];
    const quiet: QueueItem[] = [];
    for (const item of inProgress) {
      if (isRowQuiet(item.id, ctx)) quiet.push(item);
      else needsYou.push(item);
    }
    out.push({ kind: "in_progress", items: [...afterBlockers(needsYou), ...afterBlockers(quiet)] });
  }
  for (const u of urgencyOrder) {
    const bucket = afterBlockers(rest.filter((i) => (i.ranking?.urgency ?? "today") === u));
    if (bucket.length) out.push({ kind: u, items: bucket });
  }
  return out;
}
