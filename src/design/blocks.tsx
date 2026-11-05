// Renders connector-provided Blocks. One component per Block variant; a new
// connector gets rich previews by emitting these, with no UI work.

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

export function Blocks({ blocks }: { blocks: Block[] }) {
  return (
    <div className="blocks">
      {blocks.map((b, i) => (
        <BlockView key={i} block={b} />
      ))}
    </div>
  );
}
