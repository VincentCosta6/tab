import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Badge } from "./components";
import { Brand, brandFor } from "./brands";
import { timeAgo } from "../lib/format";
import { openLink } from "../lib/links";
import { useLinkPreview } from "../lib/linkPreview";

/** True once the element has been on screen, so a long chat only asks GitHub about links the person reaches. */
function useSeen<T extends Element>() {
  const ref = useRef<T>(null);
  const [seen, setSeen] = useState(typeof IntersectionObserver === "undefined");
  useEffect(() => {
    const el = ref.current;
    if (seen || !el) return;
    const io = new IntersectionObserver((es) => { if (es.some((e) => e.isIntersecting)) { setSeen(true); io.disconnect(); } }, { rootMargin: "200px" });
    io.observe(el);
    return () => io.disconnect();
  }, [seen]);
  return [ref, seen] as const;
}

/**
 * A bare link in a message that a connector can preview (a GitHub PR, say):
 * `owner/repo #123`, a cut-off title and the connector's status to the right. Hover for the fuller card. It shares one cached
 * summary per link with the context bar, and only asks once it is on screen.
 */
export function LinkPill({ href }: { href: string }) {
  const [ref, seen] = useSeen<HTMLAnchorElement>();
  const { link, sum } = useLinkPreview(href, seen);
  const [card, setCard] = useState<DOMRect | null>(null);
  const timer = useRef<number>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  if (!link) return null;
  const status = sum?.status ?? null;
  const brand = brandFor(link.kind, link.source);

  const show = (delay: number) => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => ref.current && setCard(ref.current.getBoundingClientRect()), delay);
  };
  const hide = () => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCard(null), 150);
  };

  return (
    <>
      <a
        ref={ref}
        href={href}
        className="link-pill"
        target="_blank"
        rel="noreferrer"
        onClick={(e) => { e.preventDefault(); setCard(null); void openLink(href, e); }}
        onMouseEnter={() => show(300)}
        onMouseLeave={hide}
        onFocus={() => show(0)}
        onBlur={hide}
      >
        {brand && <Brand name={brand} size={12} />}
        <span className="link-pill-ref">{link.title}</span>
        {sum?.title && <span className="link-pill-title">{sum.title}</span>}
        {status && <Badge tone={status.tone}>{status.label}</Badge>}
      </a>
      {card && createPortal(
        <div
          className="link-card"
          role="tooltip"
          style={{ left: Math.max(8, Math.min(card.left, window.innerWidth - 348)), ...(card.bottom + 220 > window.innerHeight ? { bottom: window.innerHeight - card.top + 6 } : { top: card.bottom + 6 }) }}
          onMouseEnter={() => window.clearTimeout(timer.current)}
          onMouseLeave={hide}
        >
          <div className="link-card-head">
            {brand && <Brand name={brand} size={14} />}
            <span>{link.title}</span>
            {status && <Badge tone={status.tone}>{status.label}</Badge>}
          </div>
          {sum ? (
            <>
              <strong>{sum.title}</strong>
              <span className="link-card-meta">{[sum.author && `by ${sum.author}`, sum.updated_at && `updated ${timeAgo(sum.updated_at)}`].filter(Boolean).join(" · ")}</span>
              {!!sum.facts?.length && <dl className="link-card-facts">{sum.facts.map(([k, v]) => <div key={k}><dt>{k}</dt><dd>{v}</dd></div>)}</dl>}
            </>
          ) : (
            <span className="link-card-meta">Loading…</span>
          )}
          <span className="link-card-hint">{link.open ? "Click to review in tab" : "Click to open"} · Ctrl-click for GitHub</span>
        </div>,
        document.body,
      )}
    </>
  );
}
