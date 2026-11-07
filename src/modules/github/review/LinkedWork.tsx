// The tasks and agent sessions this PR belongs to: every chat that has it in
// its Context, with whether the agent is working, waiting on you, or done.

import { useQuery } from "@tanstack/react-query";
import { Badge, cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import { api } from "../../../lib/api";
import { timeAgo } from "../../../lib/format";
import { useAgentActivity } from "../../../lib/hooks";
import { openItem } from "../../../lib/links";
import { keys } from "../../../lib/query";
import type { SessionStatus, Tone } from "../../../lib/types";

const look: Record<SessionStatus, { label: string; tone: Tone }> = {
  working: { label: "Agent working", tone: "info" },
  needs_input: { label: "Waiting on you", tone: "warning" },
  failed: { label: "Agent failed", tone: "danger" },
  idle: { label: "Agent idle", tone: "neutral" },
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
        const s = look[a?.status ?? "idle"];
        return (
          <button key={t.id} className="rv-linked-task" onClick={() => openItem(t.id)} title={`${t.title}${a ? ` · updated ${timeAgo(a.updated_at)}` : ""}`}>
            <span className={cx("rv-linked-dot", a && `agent-${a.status}`)} />
            <span className="rv-linked-title">{t.title}</span>
            <Badge tone={s.tone}>{s.label}</Badge>
          </button>
        );
      })}
    </div>
  );
}
