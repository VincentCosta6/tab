import { useEffect, useRef, useState } from "react";
import { openUrl } from "../lib/api";
import { openInTabBrowser } from "../lib/browser";
import { Button } from "./components";
import { Icon } from "./icons";

/**
 * "Open" with its three destinations: tab's own view of the thing, tab's
 * browser (a side panel), or the computer's browser. `onOpenInTab` is absent
 * when the thing is already showing in its own view.
 */
export function OpenMenu({ url, onOpenInTab, tabLabel = "Open in tab" }: { url: string; onOpenInTab?: () => void; tabLabel?: string }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    const key = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    window.addEventListener("mousedown", away);
    window.addEventListener("keydown", key);
    return () => (window.removeEventListener("mousedown", away), window.removeEventListener("keydown", key));
  }, [open]);

  // O opens the menu, as it opened the link before.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (e.key !== "o" || e.metaKey || e.ctrlKey || t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t?.isContentEditable) return;
      setOpen((v) => !v);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const pick = (f: () => void) => () => (setOpen(false), f());
  return (
    <span className="menu-anchor" ref={ref}>
      <Button size="sm" icon="external" shortcut="O" onClick={() => setOpen((v) => !v)} aria-haspopup="menu" aria-expanded={open}>Open</Button>
      {open && (
        <div className="menu open-menu" role="menu">
          {onOpenInTab && <button role="menuitem" onClick={pick(onOpenInTab)}><span><Icon name="inbox" size={13} /> {tabLabel}</span></button>}
          <button role="menuitem" onClick={pick(() => openInTabBrowser(url))}><span><Icon name="panel-right" size={13} /> Open in tab browser</span><span className="sub">side panel</span></button>
          <button role="menuitem" onClick={pick(() => void openUrl(url))}><span><Icon name="external" size={13} /> Open in browser</span></button>
        </div>
      )}
    </span>
  );
}
