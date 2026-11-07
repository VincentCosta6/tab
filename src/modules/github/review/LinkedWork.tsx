// The tasks and agent sessions this PR belongs to: every chat that has it in
// its Context, with whether the agent is working, waiting on you, or done.

import { useQuery } from "@tanstack/react-query";
import { cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import { api } from "../../../lib/api";
import { timeAgo } from "../../../lib/format";
import { useAgentActivity } from "../../../lib/hooks";
import { openItem } from "../../../lib/links";
import { keys } from "../../../lib/query";
import type { SessionStatus, Tone } from "../../../lib/types";

// Tone per status; working and needs-you animate so live sessions stand out.
const look: Record<SessionStatus, { label: string; tone: Tone }> = {
  working: { label: "Working", tone: "info" },
  needs_input: { label: "Needs you", tone: "warning" },
  failed: { label: "Failed", tone: "danger" },
  idle: { label: "Idle", tone: "neutral" },
};

export function LinkedWork({ url }: { url: string }) {
  const { data: tasks = [] } = useQuery({ queryKey: keys.linkedTasks(url), queryFn: () => api.itemsForLink(url), staleTime: 0, refetchInterval: 30_000 });
  const activity = useAgentActivity();
  if (!tasks.length) return null;
  return (
    <div className="rv-linked" aria-label="Tasks linked to this pull request">
      <span className="rv-linked-label"><Icon name="graph" size={12} /> Linked work</span>
      {tasks.map((t) => {
        // No activity entry means the agent isn't busy: idle or never started.
        const a = activity.get(t.id);
        const status = a?.status ?? "idle";
        const s = look[status];
        return (
          <button key={t.id} className={cx("rv-linked-task", `is-${status}`)} onClick={() => openItem(t.id)} title={`${t.title} · agent ${s.label.toLowerCase()}${a ? ` · updated ${timeAgo(a.updated_at)}` : ""}`}>
            <span className="rv-linked-agent" aria-hidden="true"><Icon name="bot" size={12} /></span>
            <span className="rv-linked-title">{t.title}</span>
            <span className={cx("rv-linked-status", `tone-${s.tone}`)}>
              <i className="rv-linked-pulse" />
              {s.label}
            </span>
          </button>
        );
      })}
    </div>
  );
}
