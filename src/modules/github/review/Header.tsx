// The workspace header: what the PR is, where it stands (CI, threads,
// conflicts, readiness), an Actions menu, and Submit review.

import { useEffect, useRef, useState, type ReactNode } from "react";
import { Badge, Button, cx } from "../../../design/components";
import { Brand } from "../../../design/brands";
import { Icon } from "../../../design/icons";
import { api, openUrl } from "../../../lib/api";
import { useReview } from "./context";
import { checkCounts, pendingComments, statePill, statusPills, type Pill } from "./model";
import { Avatar, Composer } from "./Thread";

function useOutside(open: boolean, close: () => void) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const on = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && close();
    const key = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("mousedown", on);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("mousedown", on);
      window.removeEventListener("keydown", key);
    };
  }, [open, close]);
  return ref;
}

function StatusPill({ p, onClick }: { p: Pill; onClick?: () => void }) {
  return (
    <button className={cx("rv-pill", `tone-${p.tone}`, onClick && "clickable")} title={p.title} onClick={onClick} disabled={!onClick}>
      <Icon name={p.icon} size={12} />
      {p.label}
    </button>
  );
}

function Confirm({ title, body, confirm, danger, onConfirm, onClose }: { title: string; body: ReactNode; confirm: string; danger?: boolean; onConfirm: () => Promise<void>; onClose: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="rv-modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="rv-modal" role="dialog">
        <h3>{title}</h3>
        <div className="muted">{body}</div>
        {error && <p className="rv-error">{error}</p>}
        <div className="rv-modal-actions">
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button
            variant="primary"
            className={danger ? "rv-btn-danger" : undefined}
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              setError(null);
              try {
                await onConfirm();
                onClose();
              } catch (e) {
                setError(String(e));
              } finally {
                setBusy(false);
              }
            }}
          >
            {busy ? "Working…" : confirm}
          </Button>
        </div>
      </div>
    </div>
  );
}

function ChecksPopover({ onClose }: { onClose: () => void }) {
  const { pr } = useReview();
  const ref = useOutside(true, onClose);
  const c = checkCounts(pr);
  const icon = (r: (typeof pr.checks.runs)[number]) =>
    r.status !== "completed" ? <Icon name="clock" size={13} className="text-warning" /> : ["success", "neutral", "skipped"].includes(r.conclusion ?? "") ? <Icon name="check" size={13} className="text-success" /> : <Icon name="x-circle" size={13} className="text-danger" />;
  return (
    <div ref={ref} className="rv-popover rv-checks">
      <div className="rv-popover-head"><strong>Checks</strong><span className="subtle">{c.passed} passed · {c.failed} failed · {c.pending} running</span></div>
      {pr.checks.runs.map((r, i) => (
        <button key={i} className="rv-check-row" disabled={!r.url} onClick={() => r.url && openUrl(r.url)}>
          {icon(r)}
          <span>{r.name}</span>
          <span className="rv-flex" />
          <span className="subtle">{r.status === "completed" ? r.conclusion : r.status.replace("_", " ")}</span>
          {r.url && <Icon name="external" size={12} />}
        </button>
      ))}
      {!pr.checks.runs.length && <p className="subtle">No checks on this commit.</p>}
    </div>
  );
}

function SubmitReview({ onClose }: { onClose: () => void }) {
  const { pr, act, notify } = useReview();
  const ref = useOutside(true, onClose);
  const [event, setEvent] = useState<"comment" | "approve" | "request_changes">("comment");
  const [body, setBody] = useState(pr.pending_review?.body ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pending = pendingComments(pr);
  const own = pr.author === pr.viewer;
  const needsBody = event === "request_changes" || (event === "comment" && pending === 0);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      await act({ type: "submit", event, body });
      notify(event === "approve" ? "Approved" : event === "request_changes" ? "Requested changes" : "Review submitted", "success");
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const option = (v: typeof event, label: string, help: string, disabled = false) => (
    <label className={cx("rv-radio", disabled && "disabled")}>
      <input type="radio" name="review-event" checked={event === v} disabled={disabled} onChange={() => setEvent(v)} />
      <span>
        <strong>{label}</strong>
        <span className="subtle">{help}</span>
      </span>
    </label>
  );

  return (
    <div ref={ref} className="rv-popover rv-submit">
      <div className="rv-popover-head"><strong>Finish your review</strong>{pending > 0 && <Badge tone="warning">{pending} pending comment{pending === 1 ? "" : "s"}</Badge>}</div>
      <Composer initial={body} placeholder="Leave a summary (optional for comments)" buttons={[]} hideButtons minRows={5} onChange={setBody} />
      <div className="rv-radios">
        {option("comment", "Comment", "General feedback without explicit approval.")}
        {option("approve", "Approve", own ? "You can't approve your own pull request." : "Give your approval to merge these changes.", own)}
        {option("request_changes", "Request changes", own ? "You can't request changes on your own pull request." : "Feedback that must be addressed before merging.", own)}
      </div>
      {error && <p className="rv-error">{error}</p>}
      <div className="rv-modal-actions">
        {pr.pending_review && (
          <Button variant="ghost" className="text-danger" onClick={() => act({ type: "discard" }).then(() => { notify("Pending review discarded"); onClose(); }).catch((e) => setError(String(e)))}>
            Discard review
          </Button>
        )}
        <span className="rv-flex" />
        <Button variant="primary" disabled={busy || (needsBody && !body.trim())} onClick={submit}>{busy ? "Submitting…" : "Submit review"}</Button>
      </div>
    </div>
  );
}

function copy(text: string) {
  navigator.clipboard?.writeText(text).catch(() => {});
}

export function Header({ onBack, local, onRefresh, refreshing }: { onBack: () => void; local: boolean; onRefresh: () => void; refreshing: boolean }) {
  const { pr, act, notify } = useReview();
  const [menu, setMenu] = useState(false);
  const [submit, setSubmit] = useState(false);
  const [checks, setChecks] = useState(false);
  const [confirm, setConfirm] = useState<null | { title: string; body: ReactNode; confirm: string; danger?: boolean; run: () => Promise<void> }>(null);
  const [vscode, setVscode] = useState(false);
  const menuRef = useOutside(menu, () => setMenu(false));
  const pending = pendingComments(pr);
  const pills = statusPills(pr);
  const state = statePill(pr);
  const open = pr.state === "open";

  useEffect(() => {
    if (!pr.sample) api.vscodeAvailable().then(setVscode).catch(() => setVscode(false));
  }, [pr.sample]);

  const run = (f: () => Promise<void>, done?: string) => () => {
    setMenu(false);
    f().then(() => done && notify(done, "success")).catch((e) => notify(String(e), "danger"));
  };
  const item = (icon: string, label: string, onClick: () => void, opts: { disabled?: boolean; danger?: boolean; hint?: string } = {}) => (
    <button className={cx("rv-menu-item", opts.danger && "text-danger")} disabled={opts.disabled} onClick={onClick}>
      <Icon name={icon} size={14} />
      <span>{label}</span>
      {opts.hint && <span className="subtle rv-menu-hint">{opts.hint}</span>}
    </button>
  );
  const merge = (method: "squash" | "merge" | "rebase", label: string) => () => {
    setMenu(false);
    setConfirm({
      title: `${label}?`,
      body: <>This merges <code>{pr.head_ref}</code> into <code>{pr.base_ref}</code> on GitHub. It can't be undone from here.</>,
      confirm: label,
      run: () => act({ type: "merge", method }).then(() => notify("Merged", "success")),
    });
  };

  return (
    <header className="rv-header">
      <div className="rv-header-row">
        <Button size="sm" variant="ghost" icon="arrow-left" onClick={onBack} title="Back to reviews" />
        <Brand name="github" size={18} />
        <div className="rv-title">
          <h1 title={pr.title}>{pr.title} <span className="subtle">#{pr.number}</span></h1>
        </div>
        {pr.sample && <Badge tone="warning">Sample data</Badge>}
        <span className="rv-flex" />
        <div className="rv-menu-wrap" ref={menuRef}>
          <Button size="sm" icon="more" onClick={() => setMenu((m) => !m)}>Actions</Button>
          {menu && (
            <div className="rv-menu rv-menu-right">
              {item("external", "View on GitHub", run(() => openUrl(pr.url)))}
              {item("code", "Open in VS Code", run(() => api.workspaceOpen(pr.repo, pr.number, "vscode")), { disabled: !local || pr.sample || !vscode, hint: !vscode && !pr.sample ? "code not found" : undefined })}
              {item("folder", "Show checkout folder", run(() => api.workspaceOpen(pr.repo, pr.number, "folder")), { disabled: !local || pr.sample })}
              {item("copy", "Copy branch name", run(async () => copy(pr.head_ref), "Copied"))}
              {item("terminal", "Copy checkout command", run(async () => copy(`gh pr checkout ${pr.number} --repo ${pr.repo}`), "Copied"))}
              <div className="rv-menu-sep" />
              {item("refresh", "Refresh", () => { setMenu(false); onRefresh(); })}
              {open && pr.merge_state === "behind" && item("arrow-down", `Update branch from ${pr.base_ref}`, run(() => act({ type: "update_branch" }), "Branch update started"))}
              {open && (pr.draft
                ? item("eye", "Mark ready for review", run(() => act({ type: "set_draft", draft: false }), "Ready for review"))
                : item("git-pull-request-draft", "Convert to draft", run(() => act({ type: "set_draft", draft: true }), "Converted to draft")))}
              {open && !pr.draft && (
                <>
                  <div className="rv-menu-sep" />
                  {item("git-merge", "Squash and merge", merge("squash", "Squash and merge"))}
                  {item("git-merge", "Create a merge commit", merge("merge", "Create a merge commit"))}
                  {item("git-merge", "Rebase and merge", merge("rebase", "Rebase and merge"))}
                </>
              )}
              <div className="rv-menu-sep" />
              {pr.state === "closed" && item("undo", "Reopen pull request", run(() => act({ type: "set_state", state: "open" }), "Reopened"))}
              {open && item("git-pull-request-closed", "Close pull request", () => {
                setMenu(false);
                setConfirm({ title: "Close this pull request?", body: "You can reopen it later.", confirm: "Close pull request", danger: true, run: () => act({ type: "set_state", state: "closed" }) });
              }, { danger: true })}
            </div>
          )}
        </div>
        <div className="rv-menu-wrap">
          <Button size="sm" variant="primary" icon="send" onClick={() => setSubmit((s) => !s)} disabled={pr.state === "merged"}>
            Submit review{pending > 0 ? ` (${pending})` : ""}
          </Button>
          {submit && <SubmitReview onClose={() => setSubmit(false)} />}
        </div>
      </div>
      <div className="rv-header-row rv-header-meta">
        <StatusPill p={state} />
        {pills.map((p) => (
          <span key={p.key} className="rv-menu-wrap">
            <StatusPill p={p} onClick={p.key === "ci" ? () => setChecks((c) => !c) : undefined} />
            {p.key === "ci" && checks && <ChecksPopover onClose={() => setChecks(false)} />}
          </span>
        ))}
        <span className="rv-meta-sep" />
        <Avatar login={pr.author} url={pr.author_avatar} size={18} />
        <span className="rv-meta">
          <strong>{pr.author}</strong> wants to merge {pr.commits} commit{pr.commits === 1 ? "" : "s"} into{" "}
          <button className="rv-branch" title="Copy" onClick={() => { copy(pr.base_ref); notify("Copied"); }}>{pr.base_ref}</button> from{" "}
          <button className="rv-branch" title="Copy" onClick={() => { copy(pr.head_ref); notify("Copied"); }}>{pr.head_repo && pr.head_repo !== pr.repo ? `${pr.head_repo.split("/")[0]}:` : ""}{pr.head_ref}</button>
        </span>
        <span className="rv-meta subtle rv-tabular"><span className="text-success">+{pr.additions}</span> <span className="text-danger">−{pr.deletions}</span> · {pr.files.length} files</span>
        {refreshing && <span className="subtle rv-meta">Refreshing…</span>}
      </div>
      {confirm && <Confirm {...confirm} onConfirm={confirm.run} onClose={() => setConfirm(null)} />}
    </header>
  );
}
