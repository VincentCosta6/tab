import { useQuery } from "@tanstack/react-query";
import { lazy, Suspense, useEffect, useMemo, useState } from "react";
import { Badge, Button, EmptyState, PageHeader, Tabs } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import { useItems } from "../../lib/hooks";
import { goBack, offerDone } from "../../lib/nav";
import type { MyPull } from "../../lib/types";
import { QueueRow } from "../work/WorkTab";
import { linkStatus } from "./preview";
import { parsePullUrl, takePendingPull, type LinkedPull } from "./pulls";

/** GitHub's own check marks: green tick, red cross, amber dot. */
const checkMark = {
  passing: { icon: "check", cls: "text-success", title: "All checks passed" },
  failing: { icon: "x", cls: "text-danger", title: "Some checks failed" },
  pending: { icon: "circle-dot", cls: "text-warning", title: "Checks running" },
} as const;

function MyPullRow({ p, onOpen }: { p: MyPull; onOpen: () => void }) {
  const s = p.summary;
  const status = linkStatus(s);
  const mark = s.checks ? checkMark[s.checks] : null;
  return (
    <div className="row" role="option" aria-selected={false} onClick={onOpen}>
      <span className={s.state === "draft" ? "subtle" : "text-success"} style={{ paddingTop: 2 }}>
        <Icon name={s.state === "draft" ? "git-pull-request-draft" : "git-pull-request"} size={18} />
      </span>
      <div className="row-main">
        <div className="row-title">
          {s.title}
          {mark && <span className={mark.cls} title={mark.title} style={{ marginLeft: 6, verticalAlign: "-2px" }}><Icon name={mark.icon} size={14} /></span>}
        </div>
        <div className="row-meta">
          <span className="row-meta-main">
            {status && <Badge tone={status.tone} icon={status.icon}>{status.label}</Badge>}
            <span className="row-byline">{p.repo} #{p.number}</span>
          </span>
          {s.additions != null && s.deletions != null && <span className="row-time" style={{ fontVariantNumeric: "tabular-nums" }} title="Lines added and removed"><span className="text-success">+{s.additions}</span> <span className="text-danger">−{s.deletions}</span></span>}
          {s.updated_at && <span className="row-time">{timeAgo(s.updated_at)}</span>}
        </div>
      </div>
    </div>
  );
}

type Group = "requested" | "mine";

const ReviewView = lazy(() => import("./review/Workspace"));

/** The GitHub module's tab: pull requests waiting on you, opened in the built-in editor. */
export function ReviewsTab() {
  const { data: items = [] } = useItems("open", "");
  const pulls = useMemo(() => items.filter((i) => parsePullUrl(i.url)), [items]);
  const [open, setOpen] = useState<string | null>(null);
  // A PR opened from a link, which may not be in the queue.
  const [linked, setLinked] = useState<LinkedPull | null>(null);
  useEffect(() => {
    const take = () => {
      const p = takePendingPull();
      if (p) (setOpen(null), setLinked(p));
    };
    take();
    window.addEventListener("tab:open-pr", take);
    return () => window.removeEventListener("tab:open-pr", take);
  }, []);
  const [group, setGroup] = useState<Group>("requested");
  const requested = pulls.filter((p) => p.kind === "github.review_requested");
  // Notifications only cover PRs someone else touched, so ask GitHub for all of yours.
  const mine = useQuery({ queryKey: ["github", "my-pulls"], queryFn: api.githubMyPulls, staleTime: 60_000, refetchInterval: 120_000 });
  const current = pulls.find((p) => p.id === open);
  // A linked PR that's also in the queue keeps its sample flag.
  const queued = linked ? pulls.find((p) => { const q = parsePullUrl(p.url); return q?.repo === linked.repo && q.number === linked.number; }) : undefined;
  const pr = current ? parsePullUrl(current.url) : linked;
  const sample = (current ?? queued)?.source === "demo";
  // Opened from a chat or the queue: Back goes there, and the task to offer
  // closing after a merge is the item that was open, else the PR's own item.
  const from = linked?.from && linked.from.tab !== "reviews" ? linked.from : null;
  const task = (from?.tab === "work" && from.itemId) || (current ?? queued)?.id;
  const back = () => {
    setOpen(null);
    setLinked(null);
    if (from) goBack(from);
  };
  const merged = () => task && offerDone({ itemId: task, message: "Pull request merged", icon: "git-merge", tone: "merged", back: from ?? { tab: "reviews", itemId: null } });

  if (pr)
    return (
      <div className="review-page">
        <Suspense fallback={<div className="skeleton" />}>
          <ReviewView key={`${pr.repo}#${pr.number}`} repo={pr.repo} number={pr.number} sample={sample} item={current ?? queued} onBack={back} onMerged={merged} />
        </Suspense>
      </div>
    );

  return (
    <div className="page page-narrow">
      <PageHeader title="Reviews" subtitle="Pull requests in your queue. Open one to review it in the built-in editor." />
      <div className="page-tabs">
        <Tabs<Group>
          value={group}
          onChange={setGroup}
          items={[
            { value: "requested", label: "Review requested", count: requested.length },
            { value: "mine", label: "Your PRs", count: mine.data?.length ?? 0 },
          ]}
        />
      </div>
      {group === "requested" ? (
        <>
          {!requested.length && <EmptyState icon="git-pull-request" title="No reviews waiting">PRs where your review is requested show up here.</EmptyState>}
          <div className="related">
            {requested.map((p) => <QueueRow key={p.id} item={p} selected={false} onSelect={() => setOpen(p.id)} />)}
          </div>
        </>
      ) : mine.isError ? (
        <EmptyState icon="alert" title="Couldn't load your PRs">
          {String(mine.error)}
          <div style={{ marginTop: 12 }}><Button size="sm" icon="refresh" onClick={() => mine.refetch()}>Try again</Button></div>
        </EmptyState>
      ) : mine.isLoading ? (
        <div className="skeleton" />
      ) : !mine.data?.length ? (
        <EmptyState icon="git-pull-request" title="No open PRs">Open pull requests you wrote show up here.</EmptyState>
      ) : (
        <div className="related">
          {mine.data.map((p) => <MyPullRow key={p.url} p={p} onOpen={() => (setOpen(null), setLinked({ repo: p.repo, number: p.number }))} />)}
        </div>
      )}
    </div>
  );
}
