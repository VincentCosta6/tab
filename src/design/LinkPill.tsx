// A link tab understands, shown as a pill: the product's mark, the thing's
// title and one status badge. Hovering opens a small preview card. Used for
// links in chat, comments and the Context bar, so a PR or ticket reads the
// same everywhere.

import { useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState, type MouseEvent } from "react";
import { createPortal } from "react-dom";
import { openLink, parseLink, type LinkKind } from "../lib/links";
import { timeAgo } from "../lib/format";
import type { LinkSummary, Tone } from "../lib/types";
import { Brand, brandFor } from "./brands";
import { Badge } from "./components";
import { Icon } from "./icons";

const checkStatus: Record<string, { label: string; tone: Tone }> = {
  failing: { label: "CI failing", tone: "danger" },
  pending: { label: "CI running", tone: "warning" },
  passing: { label: "CI passing", tone: "success" },
};

/**
 * One badge for a linked PR or issue: the thing that most decides what
 * happens next, like GitHub's merge box. Merged or closed, then conflicts,
 * failing CI, changes requested, unresolved threads, CI running, review
 * needed, and finally ready to merge.
 */
export function linkStatus(s: LinkSummary): { label: string; tone: Tone } | null {
  if (s.state === "merged") return { label: "Merged", tone: "accent" };
  if (s.state === "closed") return { label: "Closed", tone: "neutral" };
  if (s.merge_state === "dirty") return { label: "Conflicts", tone: "danger" };
  if (s.checks === "failing") return checkStatus.failing;
  if (s.review_decision === "changes_requested") return { label: "Changes requested", tone: "danger" };
  if (s.open_threads) return { label: `${s.open_threads} open thread${s.open_threads === 1 ? "" : "s"}`, tone: "warning" };
  if (s.state === "draft") return { label: "Draft", tone: "neutral" };
  if (s.checks === "pending") return checkStatus.pending;
  if (s.review_decision === "review_required") return { label: "Needs review", tone: "info" };
  if (s.merge_state === "behind") return { label: "Behind base", tone: "warning" };
  if (s.merge_state === "blocked") return { label: "Blocked", tone: "warning" };
  if (s.merge_state === "clean" || s.merge_state === "has_hooks") return { label: "Ready to merge", tone: "success" };
  if (s.merge_state === "unstable" || s.merge_state === "mergeable") return { label: "Mergeable", tone: "success" };
  if (s.state === "open" && (!s.merge_state || s.merge_state === "unknown") && s.review_decision !== undefined) return { label: "Checking…", tone: "neutral" };
  if (s.checks === "passing") return checkStatus.passing;
  return s.state === "open" ? { label: "Open", tone: "success" } : null;
}

export function useLinkSummary(url: string | null | undefined) {
  const link = parseLink(url);
  const { data } = useQuery({
    queryKey: ["link-summary", link?.canonical ?? ""],
    queryFn: () => link!.summary!(),
    enabled: !!link?.summary,
    staleTime: 120_000,
    retry: false,
    // GitHub works out mergeability lazily: the first look often says
    // "unknown". Ask again shortly so the badge settles on the real answer.
    refetchInterval: (q) => (q.state.data?.state === "open" && (!q.state.data.merge_state || q.state.data.merge_state === "unknown") ? 5_000 : false),
  });
  return { link, sum: data };
}

/** True when a module has claimed this URL, so it should be a pill. */
export function isDeepLink(url: string) {
  return /^https?:\/\//i.test(url) && !!parseLink(url);
}

function Preview({ at, link, sum, url }: { at: DOMRect; link: LinkKind; sum: LinkSummary | undefined; url: string }) {
  const status = sum ? linkStatus(sum) : null;
  const width = 320;
  const left = Math.max(8, Math.min(window.innerWidth - width - 8, at.left));
  const below = at.bottom + 6 + 150 < window.innerHeight;
  const brand = brandFor(link.kind, link.source);
  return createPortal(
    <div className="lp-pop" role="tooltip" style={{ left, width, ...(below ? { top: at.bottom + 6 } : { bottom: window.innerHeight - at.top + 6 }) }}>
      <div className="lp-pop-type">
        {brand ? <Brand name={brand} size={14} /> : <Icon name={link.icon} size={13} />}
        <span>{link.label}</span>
        <span className="lp-pop-ref">{link.title}</span>
      </div>
      <div className="lp-pop-title">{sum?.title || link.title}</div>
      <div className="lp-pop-meta">
        {status && <Badge tone={status.tone}>{status.label}</Badge>}
        {sum?.author && <span className="subtle">{sum.author}</span>}
        {sum?.updated_at && <span className="subtle">updated {timeAgo(sum.updated_at)}</span>}
      </div>
      <div className="lp-pop-url subtle">{url}</div>
    </div>,
    document.body,
  );
}

/** `label` is the link text the author wrote; a bare URL has none. */
export function LinkPill({ url, label }: { url: string; label?: string }) {
  const { link, sum } = useLinkSummary(url);
  const ref = useRef<HTMLAnchorElement>(null);
  const [rect, setRect] = useState<DOMRect | null>(null);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  if (!link) return <a href={url} target="_blank" rel="noreferrer" onClick={(e) => { e.preventDefault(); void openLink(url, e); }}>{label ?? url}</a>;

  const number = link.title.split(" ").pop() ?? "";
  const name = sum?.title ? `${number.startsWith("#") ? `${number} ` : ""}${sum.title}` : label && label !== url ? label : link.title;
  const status = sum ? linkStatus(sum) : null;
  const brand = brandFor(link.kind, link.source);
  const show = () => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => ref.current && setRect(ref.current.getBoundingClientRect()), 250);
  };
  const hide = () => {
    window.clearTimeout(timer.current);
    setRect(null);
  };
  return (
    <>
      <a
        ref={ref}
        href={url}
        className="lp"
        onMouseEnter={show}
        onMouseLeave={hide}
        onFocus={show}
        onBlur={hide}
        onClick={(e: MouseEvent) => {
          e.preventDefault();
          hide();
          void openLink(url, e);
        }}
      >
        <span className="lp-mark">{brand ? <Brand name={brand} size={13} /> : <Icon name={link.icon} size={12} />}</span>
        <span className="lp-name">{name}</span>
        {status && <Badge tone={status.tone}>{status.label}</Badge>}
      </a>
      {rect && <Preview at={rect} link={link} sum={sum} url={url} />}
    </>
  );
}
