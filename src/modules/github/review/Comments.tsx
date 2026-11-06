// Every review comment on the PR in one list: open threads up front, resolved
// ones folded away. The threads are the same cards the Changes view shows, so
// replying, resolving and "Add to fix list" work identically.

import { useMemo, useState } from "react";
import { Icon } from "../../../design/icons";
import type { ReviewThread } from "../../../lib/types";
import { useReview } from "./context";
import { ThreadCard } from "./Thread";

function group(threads: ReviewThread[]) {
  const by = new Map<string, ReviewThread[]>();
  for (const t of threads) by.set(t.path, [...(by.get(t.path) ?? []), t]);
  return [...by.entries()].sort(([a], [b]) => a.localeCompare(b));
}

function Files({ threads }: { threads: ReviewThread[] }) {
  const { showChange, openFile } = useReview();
  return (
    <>
      {group(threads).map(([path, list]) => (
        <section key={path} className="rv-comments-file">
          <button className="rv-comments-path" title="Show in Changes" onClick={() => (list[0].outdated ? openFile(path, list[0].original_line ?? 1) : showChange(path))}>
            <Icon name="file" size={13} />
            <span>{path}</span>
            <span className="subtle">{list.length}</span>
          </button>
          {list.map((t) => <ThreadCard key={t.id} thread={t} />)}
        </section>
      ))}
    </>
  );
}

export function Comments() {
  const { pr, ai } = useReview();
  const [showResolved, setShowResolved] = useState(false);
  const { open, resolved } = useMemo(() => ({
    open: pr.threads.filter((t) => !t.resolved),
    resolved: pr.threads.filter((t) => t.resolved),
  }), [pr.threads]);
  const queued = ai.fixes.filter((f) => f.state === "queued").length;

  return (
    <div className="rv-comments">
      <div className="rv-comments-head">
        <h2>{open.length ? `${open.length} unresolved` : "No unresolved comments"}</h2>
        <span className="subtle">
          {resolved.length} resolved · {queued} in fix list
        </span>
      </div>
      {!pr.threads.length && <p className="subtle rv-pad">No review comments yet.</p>}
      <Files threads={open} />
      {resolved.length > 0 && (
        <>
          <button className="rv-comments-resolved" onClick={() => setShowResolved((s) => !s)}>
            <Icon name={showResolved ? "chevron-down" : "chevron-right"} size={13} />
            {resolved.length} resolved thread{resolved.length === 1 ? "" : "s"}
          </button>
          {showResolved && <Files threads={resolved} />}
        </>
      )}
    </div>
  );
}
