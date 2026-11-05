import { useState } from "react";
import { Blocks } from "../../design/blocks";
import { cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { usePreview } from "../../lib/hooks";
import type { Block, QueueItem } from "../../lib/types";

type Mode = "rich" | "plain";
const MODE_KEY = "tab.readMode";

function htmlToText(html: string): string {
  const doc = new DOMParser().parseFromString(html, "text/html");
  doc.querySelectorAll("script, style, head").forEach((n) => n.remove());
  doc.querySelectorAll("br").forEach((n) => n.replaceWith("\n"));
  doc.querySelectorAll("p, div, tr, li, h1, h2, h3, h4, blockquote").forEach((n) => n.append("\n"));
  return (doc.body.textContent ?? "").replace(/[ \t ]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
}

/**
 * An item's content, read in the right pane: the full email (or rich
 * preview) formatted by default, with a switch to plain text.
 */
export function Reader({ item }: { item: QueueItem }) {
  const { data: preview, isLoading } = usePreview(item.id);
  const [mode, setModeState] = useState<Mode>(() => {
    try {
      return localStorage.getItem(MODE_KEY) === "plain" ? "plain" : "rich";
    } catch {
      return "rich";
    }
  });
  const setMode = (m: Mode) => {
    setModeState(m);
    try {
      localStorage.setItem(MODE_KEY, m);
    } catch {
      /* not remembered in private mode */
    }
  };

  const blocks: Block[] = preview?.blocks.length ? preview.blocks : item.blocks;
  const hasHtml = blocks.some((b) => b.type === "html");
  const shown: Block[] =
    mode === "plain" ? blocks.map((b) => (b.type === "html" ? { type: "text", text: htmlToText(b.html) } : b)) : blocks;

  return (
    <div className="reader">
      <div className="reader-bar">
        {hasHtml && (
          <div className="seg" role="radiogroup" aria-label="Show as">
            {(["rich", "plain"] as Mode[]).map((m) => (
              <button key={m} role="radio" aria-checked={mode === m} className={cx("seg-btn", mode === m && "active")} onClick={() => setMode(m)}>
                {m === "rich" ? "Rich" : "Plain text"}
              </button>
            ))}
          </div>
        )}
        {isLoading && <span className="subtle"><Icon name="clock" size={12} /> Loading the full message…</span>}
      </div>
      <div className="reader-body">
        {shown.length ? <Blocks blocks={shown} /> : <p className="subtle">{item.body ?? "Nothing to show."}</p>}
      </div>
    </div>
  );
}
