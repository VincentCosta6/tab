// Renders connector-provided Blocks. One component per Block variant; a new
// connector gets rich previews by emitting these, with no UI work.

import { useEffect, useRef, useState } from "react";
import type { Block } from "../lib/types";
import { openUrl } from "../lib/api";
import { Badge, Button, cx } from "./components";
import { Icon } from "./icons";
import { Markdown } from "./markdown";

export function BlockView({ block }: { block: Block }) {
  switch (block.type) {
    case "text":
      return block.markdown ? <Markdown text={block.text} /> : <p className="block-text">{block.text}</p>;
    case "quote":
      return (
        <blockquote className="block-quote">
          {block.author && <cite>{block.author}</cite>}
          <Markdown text={block.text} />
        </blockquote>
      );
    case "fields":
      return (
        <dl className="block-fields">
          {block.items.map((f, i) => (
            <div key={i}>
              <dt>{f.label}</dt>
              <dd className={cx(f.tone && f.tone !== "neutral" && `text-${f.tone}`)}>{f.value}</dd>
            </div>
          ))}
        </dl>
      );
    case "stats":
      return (
        <div className="block-stats">
          {block.items.map((f, i) => (
            <div key={i} className="stat">
              <span className={cx("stat-value", f.tone && `text-${f.tone}`)}>{f.value}</span>
              <span className="stat-label">{f.label}</span>
            </div>
          ))}
        </div>
      );
    case "badges":
      return (
        <div className="block-badges">
          {block.items.map((f, i) => (
            <Badge key={i} tone={f.tone} title={f.label}>{f.value}</Badge>
          ))}
        </div>
      );
    case "links":
      return (
        <div className="block-links">
          {block.items.map((l, i) => (
            <button key={i} className="link-row" onClick={() => openUrl(l.url)}>
              <Icon name="external" size={14} />
              <span>{l.label}</span>
            </button>
          ))}
        </div>
      );
    case "code":
      return <pre className="block-code"><code>{block.text}</code></pre>;
    case "html":
      return <HtmlFrame html={block.html} />;
    case "choices":
      return (
        <div className="block-choices">
          <span className="block-choices-prompt">{block.prompt}</span>
          <div className="block-choices-options">
            {block.options.map((o, i) => (
              <Button key={i} size="sm" title="Answering agents from tab arrives with the agent sessions connector" disabled>
                {o}
              </Button>
            ))}
          </div>
        </div>
      );
  }
}

/**
 * A formatted email, isolated in a frame: no scripts, no forms, and remote
 * images (often trackers) blocked until the person loads them. Links open in
 * the browser.
 */
function HtmlFrame({ html }: { html: string }) {
  const ref = useRef<HTMLIFrameElement>(null);
  const [images, setImages] = useState(false);
  const [height, setHeight] = useState(200);
  const remote = /<img[^>]+src=["']?https?:|url\(\s*["']?https?:/i.test(html);
  const csp = `default-src 'none'; style-src 'unsafe-inline' ${images ? "https:" : ""}; img-src data: ${images ? "https: http:" : ""}; font-src data: ${images ? "https:" : ""}`;
  const doc = `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${csp}"><base target="_blank"><style>html,body{margin:0;padding:0;background:#fff;color:#1a1a1a;font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;overflow-wrap:anywhere}body{padding:16px}img{max-width:100%;height:auto}table{max-width:100%}pre{white-space:pre-wrap}</style></head><body>${html}</body></html>`;

  useEffect(() => {
    const frame = ref.current;
    if (!frame) return;
    const onLoad = () => {
      const d = frame.contentDocument;
      if (!d) return;
      const fit = () => setHeight(Math.min(4000, d.documentElement.scrollHeight + 2));
      fit();
      d.querySelectorAll("img").forEach((i) => i.addEventListener("load", fit));
      d.addEventListener("click", (e) => {
        const a = (e.target as Element | null)?.closest?.("a[href]") as HTMLAnchorElement | null;
        if (!a) return;
        e.preventDefault();
        if (/^(https?|mailto):/i.test(a.href)) void openUrl(a.href);
      });
    };
    frame.addEventListener("load", onLoad);
    return () => frame.removeEventListener("load", onLoad);
  }, [doc]);

  return (
    <div className="block-html">
      {remote && !images && (
        <div className="block-html-bar">
          <Icon name="shield" size={13} /> Remote images are blocked.
          <Button size="sm" variant="ghost" onClick={() => setImages(true)}>Load images</Button>
        </div>
      )}
      {/* allow-same-origin without allow-scripts: nothing in the email runs, but tab can size the frame and catch link clicks. */}
      <iframe ref={ref} title="Email" sandbox="allow-same-origin" srcDoc={doc} style={{ height }} />
    </div>
  );
}

export function Blocks({ blocks }: { blocks: Block[] }) {
  return (
    <div className="blocks">
      {blocks.map((b, i) => (
        <BlockView key={i} block={b} />
      ))}
    </div>
  );
}
