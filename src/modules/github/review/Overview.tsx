// The PR at a glance: description, checks, timeline, and (on the right) the
// reviewers and labels, which can be changed here and are saved to GitHub.

import { useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { Icon } from "../../../design/icons";
import { api, openUrl } from "../../../lib/api";
import type { TimelineEvent } from "../../../lib/types";
import { useReview } from "./context";
import { checkCounts, checkSummary, openThreads, runLook, runState, sortedRuns, timeAgo } from "./model";
import { Avatar, RichBody } from "./Thread";

const SAMPLE_OPTIONS = { labels: [{ name: "payments", color: "0e8a16" }, { name: "bug", color: "d73a4a" }, { name: "needs-docs", color: "fbca04" }], users: ["you", "alex", "sam"] };

function Checks() {
  const { pr } = useReview();
  const c = checkCounts(pr);
  const runs = sortedRuns(pr.checks.runs);
  return (
    <section className="rv-card">
      <div className="rv-card-head"><strong>Checks</strong><span className="subtle">{runs.length ? checkSummary(c) : ""}</span></div>
      {runs.map((r, i) => (
        <button key={i} className="rv-check-row" disabled={!r.url} onClick={() => r.url && openUrl(r.url)}>
          <Icon name={runLook[runState(r)].icon} size={14} className={runLook[runState(r)].tone} />
          <span>{r.name}</span>
          <span className="rv-flex" />
          <span className="subtle">{r.status === "completed" ? r.conclusion : r.status.replace("_", " ")}</span>
          {r.url && <Icon name="external" size={12} />}
        </button>
      ))}
      {!runs.length && <p className="subtle rv-pad">No checks on this commit.</p>}
    </section>
  );
}

const verbs: Record<string, [string, string]> = {
  commit: ["git-branch", "committed"],
  labeled: ["dot", "added the label"],
  unlabeled: ["dot", "removed the label"],
  review_requested: ["eye", "requested a review from"],
  ready: ["git-pull-request", "marked this ready for review"],
  draft: ["git-pull-request-draft", "converted this to a draft"],
  merged: ["git-merge", "merged this"],
  closed: ["git-pull-request-closed", "closed this"],
  reopened: ["git-pull-request", "reopened this"],
};

function Event({ e }: { e: TimelineEvent }) {
  const who = <strong>{e.actor ?? "Someone"}</strong>;
  if (e.kind === "comment" || e.kind === "review") {
    const [state, ...rest] = e.kind === "review" ? e.text.split("\n") : [null, e.text];
    const body = rest.join("\n");
    const label = state === "approved" ? "approved these changes" : state === "changes_requested" ? "requested changes" : state === "commented" ? "reviewed" : state ? state.replace("_", " ") : "commented";
    return (
      <li className="rv-tl-card">
        <div className="rv-tl-head">
          <Avatar login={e.actor ?? "?"} url={e.avatar} size={20} />
          <span>{who} {label}</span>
          <span className="subtle">{timeAgo(e.at)}</span>
          <span className="rv-flex" />
          {e.url && <button className="rv-icon-btn" title="Open on GitHub" onClick={() => openUrl(e.url!)}><Icon name="external" size={12} /></button>}
        </div>
        {body.trim() && <div className="rv-tl-body"><RichBody body={body} /></div>}
      </li>
    );
  }
  const [icon, verb] = verbs[e.kind] ?? ["dot", e.kind];
  return (
    <li className="rv-tl-event">
      <Icon name={icon} size={13} />
      <span>
        {e.kind === "commit" ? <>{e.text}</> : <>{who} {verb}{e.text ? <> <strong>{e.text}</strong></> : null}</>}
      </span>
      <span className="subtle">{e.kind === "commit" ? `${e.actor ?? ""} · ` : ""}{timeAgo(e.at)}</span>
    </li>
  );
}

function Picker({ title, options, onPick, onClose }: { title: string; options: { id: string; label: string; color?: string }[]; onPick: (id: string) => void; onClose: () => void }) {
  const [q, setQ] = useState("");
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const on = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && onClose();
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("mousedown", on);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("mousedown", on);
      window.removeEventListener("keydown", key);
    };
  }, [onClose]);
  const list = options.filter((o) => o.label.toLowerCase().includes(q.toLowerCase()));
  return (
    <div ref={ref} className="rv-popover rv-picker">
      <div className="rv-popover-head"><strong>{title}</strong></div>
      <input className="rv-input" autoFocus placeholder="Filter…" value={q} onChange={(e) => setQ(e.target.value)} />
      <div className="rv-picker-list">
        {list.map((o) => (
          <button key={o.id} className="rv-menu-item" onClick={() => { onPick(o.id); onClose(); }}>
            {o.color ? <span className="rv-dot" style={{ background: `#${o.color}` }} /> : <Avatar login={o.label} size={16} />}
            <span>{o.label}</span>
          </button>
        ))}
        {!list.length && <p className="subtle rv-pad">Nothing to add.</p>}
      </div>
    </div>
  );
}

function Side() {
  const { pr, act, notify } = useReview();
  const [picker, setPicker] = useState<null | "reviewers" | "labels">(null);
  const options = useQuery({
    queryKey: ["review-options", pr.repo],
    queryFn: () => (pr.sample ? Promise.resolve(SAMPLE_OPTIONS) : api.githubReviewOptions(pr.repo)),
    enabled: !!picker,
    staleTime: 5 * 60_000,
  });
  const can = pr.viewer_can_update && pr.state !== "merged";
  const run = (p: Promise<void>) => p.catch((e) => notify(String(e), "danger"));
  const c = checkCounts(pr);
  return (
    <aside className="rv-overview-side">
      <section>
        <div className="label-sm rv-side-title">Reviewers
          {can && <button className="rv-icon-btn" title="Request a review" onClick={() => setPicker("reviewers")}><Icon name="plus" size={13} /></button>}
        </div>
        {picker === "reviewers" && (
          <Picker
            title="Request a review"
            options={(options.data?.users ?? []).filter((u) => u !== pr.author && !pr.reviewers.includes(u)).map((u) => ({ id: u, label: u }))}
            onPick={(u) => run(act({ type: "add_reviewers", logins: [u] }))}
            onClose={() => setPicker(null)}
          />
        )}
        {pr.reviewers.length ? pr.reviewers.map((r) => (
          <div key={r} className="rv-side-row">
            <Avatar login={r} size={18} /> <span>{r}</span>
            <span className="rv-flex" />
            {can && <button className="rv-icon-btn" title="Remove request" onClick={() => run(act({ type: "remove_reviewers", logins: [r] }))}><Icon name="x" size={12} /></button>}
          </div>
        )) : <p className="subtle">No one requested.</p>}
      </section>
      <section>
        <div className="label-sm rv-side-title">Labels
          {can && <button className="rv-icon-btn" title="Add a label" onClick={() => setPicker("labels")}><Icon name="plus" size={13} /></button>}
        </div>
        {picker === "labels" && (
          <Picker
            title="Add a label"
            options={(options.data?.labels ?? []).filter((l) => !pr.labels.some((x) => x.name === l.name)).map((l) => ({ id: l.name, label: l.name, color: l.color }))}
            onPick={(l) => run(act({ type: "add_labels", labels: [l] }))}
            onClose={() => setPicker(null)}
          />
        )}
        <div className="rv-labels">
          {pr.labels.map((l) => (
            <span key={l.name} className="rv-label" style={{ ["--label" as string]: `#${l.color}` }}>
              {l.name}
              {can && <button className="rv-label-x" title="Remove label" onClick={() => run(act({ type: "remove_label", label: l.name }))}><Icon name="x" size={10} /></button>}
            </span>
          ))}
        </div>
        {!pr.labels.length && <p className="subtle">None yet.</p>}
      </section>
      <section>
        <div className="label-sm rv-side-title">Checks · {checkSummary(c)}</div>
        <div className="rv-side-row"><Icon name="message" size={13} /> {openThreads(pr)} open of {pr.threads.length} threads</div>
      </section>
    </aside>
  );
}

export function Overview() {
  const { pr } = useReview();
  return (
    <div className="rv-overview">
      <div className="rv-overview-main">
        <section className="rv-card">
          <div className="rv-card-head">
            <Avatar login={pr.author} url={pr.author_avatar} size={22} />
            <span><strong>{pr.author}</strong> <span className="subtle">opened this {timeAgo(pr.created_at)}</span></span>
          </div>
          <div className="rv-overview-body"><RichBody body={pr.body} /></div>
        </section>
        <Checks />
        <section>
          <h3>Timeline</h3>
          <ul className="rv-timeline">
            {pr.timeline.map((e, i) => <Event key={i} e={e} />)}
          </ul>
          {!pr.timeline.length && <p className="subtle">{pr.sample ? "Sample PRs have no timeline." : "Nothing has happened yet."}</p>}
        </section>
      </div>
      <Side />
    </div>
  );
}
