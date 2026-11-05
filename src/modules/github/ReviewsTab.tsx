import { lazy, Suspense, useMemo, useState } from "react";
import { EmptyState, PageHeader, Tabs } from "../../design/components";
import { useItems } from "../../lib/hooks";
import { QueueRow } from "../work/WorkTab";
import { parsePullUrl } from "./pulls";

type Group = "requested" | "mine";

const ReviewView = lazy(() => import("./review/Workspace"));

/** The GitHub module's tab: pull requests waiting on you, opened in the built-in editor. */
export function ReviewsTab() {
  const { data: items = [] } = useItems("open", "");
  const pulls = useMemo(() => items.filter((i) => parsePullUrl(i.url)), [items]);
  const [open, setOpen] = useState<string | null>(null);
  const [group, setGroup] = useState<Group>("requested");
  // GitHub only sends PRs you wrote or were asked to review.
  const requested = pulls.filter((p) => p.kind === "github.review_requested");
  const mine = pulls.filter((p) => p.kind !== "github.review_requested");
  const shown = group === "requested" ? requested : mine;
  const current = pulls.find((p) => p.id === open);
  const pr = current ? parsePullUrl(current.url) : null;

  if (current && pr)
    return (
      <div className="review-page">
        <Suspense fallback={<div className="skeleton" />}>
          <ReviewView repo={pr.repo} number={pr.number} sample={current.source === "demo"} onBack={() => setOpen(null)} />
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
            { value: "mine", label: "Your PRs", count: mine.length },
          ]}
        />
      </div>
      {!shown.length &&
        (group === "requested" ? (
          <EmptyState icon="git-pull-request" title="No reviews waiting">PRs where your review is requested show up here.</EmptyState>
        ) : (
          <EmptyState icon="git-pull-request" title="Nothing on your PRs">Your open PRs show up here when there's activity on them.</EmptyState>
        ))}
      <div className="related">
        {shown.map((p) => <QueueRow key={p.id} item={p} selected={false} onSelect={() => setOpen(p.id)} />)}
      </div>
    </div>
  );
}
