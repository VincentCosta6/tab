// What happened to an item, in order: it arrived, an agent session started,
// a workflow ran, a PR linked into its context merged, it was done. Read
// from the backend's append-only activity log.

import { useQuery } from "@tanstack/react-query";
import { cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import type { Activity, Tone } from "../../lib/types";
import "./timeline.css";

const look: Record<string, [icon: string, tone: Tone]> = {
  arrived: ["inbox", "neutral"],
  updated: ["refresh", "neutral"],
  snoozed: ["clock", "warning"],
  woke: ["clock", "info"],
  done: ["check", "success"],
  reopened: ["undo", "neutral"],
  finished_upstream: ["git-merge", "merged"],
  project: ["folder", "neutral"],
  linked: ["link", "accent"],
  session_started: ["bot", "accent"],
  session_needs_input: ["bot", "warning"],
  session_failed: ["bot", "danger"],
  workflow_run: ["workflow", "info"],
};

function who(by: string | null) {
  if (!by) return null;
  if (by.startsWith("workflow:")) return `workflow ${by.slice(9)}`;
  return by;
}

function Row({ a }: { a: Activity }) {
  const [icon, tone0] = look[a.kind] ?? ["dot", "neutral"];
  const failed = a.kind === "workflow_run" && ["failed", "blocked", "loop_stopped", "throttled"].includes(String(a.detail?.status));
  const tone: Tone = failed ? "warning" : tone0;
  const by = who(a.by);
  return (
    <div className="timeline-row">
      <span className={cx("timeline-dot", `tone-${tone}`)}><Icon name={icon} size={12} /></span>
      <div className="timeline-text">
        <strong>{a.kind === "linked" ? `Linked ${a.title}` : a.title}</strong>
        {a.via && <span className="timeline-via">on {a.via}</span>}
        <span className="subtle" title={new Date(a.at).toLocaleString()}>{[timeAgo(a.at), by && `by ${by}`].filter(Boolean).join(" · ")}</span>
        {typeof a.detail?.error === "string" && <span className="timeline-via">{a.detail.error as string}</span>}
      </div>
    </div>
  );
}

export function Timeline({ itemId }: { itemId: string }) {
  const { data } = useQuery({ queryKey: ["activity", itemId], queryFn: () => api.itemActivity(itemId), staleTime: 0 });
  if (!data) return <div className="timeline-empty">Loading…</div>;
  if (!data.length) return <div className="timeline-empty">Nothing recorded yet. Changes, agent sessions and workflow runs show here as they happen.</div>;
  return <div className="timeline">{data.map((a) => <Row key={`${a.id}-${a.at}`} a={a} />)}</div>;
}
