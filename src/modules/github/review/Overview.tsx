// The PR's description, labels, reviewers and checks, as an editor tab.

import { Icon } from "../../../design/icons";
import { openUrl } from "../../../lib/api";
import { useReview } from "./context";
import { checkCounts, checkSummary, openThreads, runLook, runState, sortedRuns, timeAgo } from "./model";
import { Avatar, RichBody } from "./Thread";

export function Overview() {
  const { pr, showChange } = useReview();
  const c = checkCounts(pr);
  return (
    <div className="rv-overview">
      <div className="rv-overview-main">
        <div className="rv-overview-author">
          <Avatar login={pr.author} url={pr.author_avatar} size={28} />
          <div>
            <strong>{pr.author}</strong> <span className="subtle">opened this {timeAgo(pr.created_at)}</span>
          </div>
        </div>
        <div className="rv-overview-body"><RichBody body={pr.body} /></div>
        <h3>Files</h3>
        <div className="rv-overview-files">
          {pr.files.map((f) => (
            <button key={f.filename} className="rv-overview-file" onClick={() => showChange(f.filename)}>
              <Icon name="file" size={13} />
              <span>{f.filename}</span>
              <span className="rv-flex" />
              <span className="text-success">+{f.additions}</span> <span className="text-danger">−{f.deletions}</span>
            </button>
          ))}
        </div>
      </div>
      <aside className="rv-overview-side">
        <section>
          <div className="label-sm">Reviewers</div>
          {pr.reviewers.length ? pr.reviewers.map((r) => <div key={r} className="rv-side-row"><Avatar login={r} size={18} /> {r}</div>) : <p className="subtle">No one requested.</p>}
        </section>
        {pr.labels.length > 0 && (
          <section>
            <div className="label-sm">Labels</div>
            <div className="rv-labels">
              {pr.labels.map((l) => <span key={l.name} className="rv-label" style={{ ["--label" as string]: `#${l.color}` }}>{l.name}</span>)}
            </div>
          </section>
        )}
        <section>
          <div className="label-sm">Checks · {checkSummary(c)}</div>
          {sortedRuns(pr.checks.runs).map((r, i) => (
            <button key={i} className="rv-side-row rv-side-btn" disabled={!r.url} onClick={() => r.url && openUrl(r.url)}>
              <Icon name={runLook[runState(r)].icon} size={13} className={runLook[runState(r)].tone} />
              <span>{r.name}</span>
            </button>
          ))}
          {!pr.checks.runs.length && <p className="subtle">No checks.</p>}
        </section>
        <section>
          <div className="label-sm">Conversation</div>
          <div className="rv-side-row"><Icon name="message" size={13} /> {openThreads(pr)} open of {pr.threads.length} threads</div>
        </section>
      </aside>
    </div>
  );
}
