import { lazy, Suspense, useMemo, useState } from "react";
import { EmptyState, PageHeader } from "../../design/components";
import { useItems } from "../../lib/hooks";
import { QueueRow } from "../work/WorkTab";
import { parsePullUrl } from "./pulls";

const ReviewView = lazy(() => import("./ReviewView"));

/** The GitHub module's tab: pull requests waiting on you, opened in the built-in editor. */
export function ReviewsTab() {
  const { data: items = [] } = useItems("open", "");
  const pulls = useMemo(() => items.filter((i) => parsePullUrl(i.url)), [items]);
  const [open, setOpen] = useState<string | null>(null);
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
      {!pulls.length && <EmptyState icon="git-pull-request" title="No pull requests waiting">Review requests from GitHub show up here.</EmptyState>}
      <div className="related">
        {pulls.map((p) => <QueueRow key={p.id} item={p} selected={false} onSelect={() => setOpen(p.id)} />)}
      </div>
    </div>
  );
}
