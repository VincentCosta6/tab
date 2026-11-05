// The screen shown while a PR's workspace is prepared: clone, fetch,
// checkout, index, each with live progress from git.

import { useEffect, useState } from "react";
import { Button, cx } from "../../../design/components";
import { Brand } from "../../../design/brands";
import { Icon } from "../../../design/icons";
import type { PullReview } from "../../../lib/types";
import { STEPS, type Boot as BootState, type StepId } from "./data";

const LABEL: Record<StepId, string> = { clone: "Clone repository", fetch: "Fetch pull request", checkout: "Check out branch", index: "Index files" };

export function Boot({ pr, boot, onBack }: { pr: PullReview | undefined; boot: BootState; onBack: () => void }) {
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    const t = window.setTimeout(() => setSlow(true), 2500);
    return () => clearTimeout(t);
  }, []);
  const current = STEPS.find((s) => boot.steps[s].state === "running") ?? STEPS.find((s) => boot.steps[s].state === "waiting");
  const done = STEPS.filter((s) => boot.steps[s].state === "done").length;
  const overall = Math.round(((done + (current ? (boot.steps[current].percent ?? 0) / 100 : 0)) / STEPS.length) * 100);

  return (
    <div className="rv-boot">
      <div className="rv-boot-glow" />
      <div className="rv-boot-card">
        <div className="rv-boot-brand">
          <span className="rv-boot-mark"><Brand name="github" size={22} /></span>
          <div>
            <div className="label-sm">{pr ? pr.repo : "Loading pull request"}</div>
            <h2>{pr ? pr.title : <span className="skeleton rv-skel-line" />}</h2>
          </div>
        </div>
        {pr && (
          <div className="rv-boot-branches">
            <span className="rv-branch static"><Icon name="git-branch" size={12} /> {pr.base_ref}</span>
            <Icon name="arrow-left" size={13} />
            <span className="rv-branch static"><Icon name="git-branch" size={12} /> {pr.head_ref}</span>
            <span className="subtle">#{pr.number} · {pr.files.length} files · <span className="text-success">+{pr.additions}</span> <span className="text-danger">−{pr.deletions}</span></span>
          </div>
        )}
        <ol className="rv-boot-steps">
          <li className={cx("rv-boot-step", pr ? "done" : "running")}>
            <span className="rv-boot-icon">{pr ? <Icon name="check" size={13} /> : <span className="rv-spinner" />}</span>
            <span className="rv-boot-label">Load pull request</span>
            <span className="subtle rv-boot-msg">{pr ? `${pr.threads.length} threads · ${pr.checks.runs.length} checks` : "Asking GitHub…"}</span>
          </li>
          {STEPS.map((s) => {
            const st = boot.steps[s];
            return (
              <li key={s} className={cx("rv-boot-step", st.state)}>
                <span className="rv-boot-icon">
                  {st.state === "done" ? <Icon name="check" size={13} /> : st.state === "running" ? <span className="rv-spinner" /> : st.state === "error" ? <Icon name="x" size={13} /> : <span className="rv-boot-dot" />}
                </span>
                <span className="rv-boot-label">{LABEL[s]}</span>
                <span className="subtle rv-boot-msg" title={st.message}>{st.message}</span>
                {st.state === "running" && st.percent != null && (
                  <span className="rv-boot-bar"><span style={{ width: `${st.percent}%` }} /></span>
                )}
              </li>
            );
          })}
        </ol>
        <div className="rv-boot-progress"><span style={{ width: `${boot.phase === "error" ? 100 : overall}%` }} className={cx(boot.phase === "error" && "error")} /></div>
        {boot.phase === "error" ? (
          <div className="rv-boot-error">
            <p><strong>Couldn't make a local copy.</strong> {boot.error}</p>
            <p className="subtle">You can still review: files load through the GitHub API instead, without the full explorer, search or cross-file navigation.</p>
            <div className="rv-boot-actions">
              <Button variant="ghost" onClick={onBack}>Back</Button>
              <Button onClick={boot.retry} icon="refresh">Try again</Button>
              <Button variant="primary" onClick={boot.goRemote}>Review without a local copy</Button>
            </div>
          </div>
        ) : (
          <div className="rv-boot-foot subtle">
            <span>First open clones the repository on this computer; after that it's a quick fetch.</span>
            {slow && pr && <button className="rv-link" onClick={boot.goRemote}>Skip and review without it</button>}
          </div>
        )}
      </div>
    </div>
  );
}
