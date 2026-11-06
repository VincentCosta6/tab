import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { api, inTauri, onBrowserEvent, openUrl } from "../lib/api";
import {
  closeBrowser, closeBrowserTab, newBrowserTab, patchBrowserTab, selectBrowserTab, setBrowserExpanded, useBrowser, withHost,
} from "../lib/browser";
import { cx } from "./components";
import { Icon } from "./icons";

const WIDTH_KEY = "tab.browser.width";
const clamp = (w: number) => Math.max(320, Math.min(w, Math.round(window.innerWidth * 0.75)));

/** Turns what was typed in the address bar into a URL: an address, or a search. */
function toUrl(input: string) {
  const t = input.trim();
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(t) || /^[\w-]+(\.[\w-]+)+(:\d+)?(\/\S*)?$/.test(t) || /^localhost(:\d+)?(\/\S*)?$/.test(t)) return withHost(t);
  return `https://duckduckgo.com/?q=${encodeURIComponent(t)}`;
}

/** The in-app browser: tabs, an address bar, and a view that is a native webview on desktop. */
export function BrowserDock() {
  const { tabs, active, expanded } = useBrowser();
  const [width, setWidth] = useState(() => {
    try { return clamp(Number(localStorage.getItem(WIDTH_KEY)) || 520); } catch { return 520; }
  });
  const [dragging, setDragging] = useState(false);
  const [address, setAddress] = useState("");
  const view = useRef<HTMLDivElement>(null);
  const created = useRef(new Set<string>());
  const current = tabs.find((t) => t.id === active);

  useEffect(() => {
    let off = () => {};
    void onBrowserEvent((e) => {
      if (e.kind === "url") patchBrowserTab(e.id, { url: e.value });
      else if (e.kind === "title") patchBrowserTab(e.id, { title: e.value });
      else patchBrowserTab(e.id, { loading: e.kind === "loading" });
    }).then((f) => (off = f));
    return () => off();
  }, []);

  useEffect(() => setAddress(current?.url ?? ""), [current?.id, current?.url]);

  const bounds = () => {
    const r = view.current!.getBoundingClientRect();
    return { x: r.left, y: r.top, w: r.width, h: r.height };
  };

  // Native webviews: create one per tab, show the active one, keep it on the placeholder.
  useLayoutEffect(() => {
    if (!inTauri || !view.current) return;
    const live = new Set(tabs.map((t) => t.id));
    created.current.forEach((id) => !live.has(id) && created.current.delete(id));
    for (const t of tabs) {
      if (created.current.has(t.id)) continue;
      created.current.add(t.id);
      api
        .browserOpen(t.id, t.url, bounds())
        .then(() => (t.id === active && !dragging ? undefined : api.browserVisible(t.id, false)))
        .catch(() => created.current.delete(t.id));
    }
    for (const t of tabs) {
      if (!created.current.has(t.id)) continue;
      const show = t.id === active && !dragging;
      void api.browserVisible(t.id, show).catch(() => {});
      if (show) void api.browserBounds(t.id, bounds()).catch(() => {});
    }
  }, [tabs, active, dragging]);

  const open = tabs.length > 0;
  useEffect(() => {
    if (!inTauri || !view.current || !active) return;
    const fit = () => view.current && void api.browserBounds(active, bounds()).catch(() => {});
    const ro = new ResizeObserver(fit);
    ro.observe(view.current);
    window.addEventListener("resize", fit);
    return () => (ro.disconnect(), window.removeEventListener("resize", fit));
  }, [active, expanded, width, open]);

  useEffect(() => {
    if (!expanded) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setBrowserExpanded(false);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [expanded]);

  if (!open) return null;

  const go = (e: React.FormEvent) => {
    e.preventDefault();
    if (!current || !address.trim()) return;
    const url = toUrl(address);
    patchBrowserTab(current.id, { url, loading: true });
    if (inTauri) void api.browserNav(current.id, "go", url).catch(() => {});
  };
  const nav = (a: "back" | "forward" | "reload") => current && inTauri && void api.browserNav(current.id, a).catch(() => {});

  // The native view swallows pointer events, so it is hidden while the edge is dragged.
  const drag = (e: React.PointerEvent) => {
    e.preventDefault();
    const startX = e.clientX, start = width;
    let w = start;
    setDragging(true);
    const move = (m: PointerEvent) => setWidth((w = clamp(start + startX - m.clientX)));
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      setDragging(false);
      try { localStorage.setItem(WIDTH_KEY, String(w)); } catch { /* not remembered */ }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  return (
    <aside className={cx("bdock", expanded && "expanded")} style={{ width }} aria-label="Browser">
      <div className={cx("bdock-grip", dragging && "drag")} onPointerDown={drag} />
      <div className="btabs" role="tablist">
        {tabs.map((t) => (
          <button key={t.id} role="tab" aria-selected={t.id === active} className={cx("btab", t.id === active && "active")} onClick={() => selectBrowserTab(t.id)} onAuxClick={(e) => e.button === 1 && closeBrowserTab(t.id)} title={t.url}>
            <Icon name={t.loading ? "refresh" : "globe"} size={12} />
            <span className="btab-title">{t.title || t.url}</span>
            <span className="btab-x" role="button" aria-label="Close tab" onClick={(e) => (e.stopPropagation(), closeBrowserTab(t.id))}><Icon name="x" size={11} /></span>
          </button>
        ))}
        <div className="btabs-end">
          <button className="icon-btn" title="New tab" onClick={newBrowserTab}><Icon name="plus" size={14} /></button>
          <button className="icon-btn" title={expanded ? "Shrink (Esc)" : "Expand to fill the window"} onClick={() => setBrowserExpanded(!expanded)}><Icon name={expanded ? "minimize" : "maximize"} size={14} /></button>
          <button className="icon-btn" title="Close browser" onClick={closeBrowser}><Icon name="x" size={14} /></button>
        </div>
      </div>
      <form className="bbar" onSubmit={go}>
        <button type="button" className="icon-btn" title="Back" onClick={() => nav("back")}><Icon name="arrow-left" size={14} /></button>
        <button type="button" className="icon-btn" title="Forward" onClick={() => nav("forward")}><Icon name="arrow-right" size={14} /></button>
        <button type="button" className="icon-btn" title="Reload" onClick={() => nav("reload")}><Icon name="refresh" size={14} /></button>
        <input value={address} onChange={(e) => setAddress(e.target.value)} onFocus={(e) => e.target.select()} spellCheck={false} aria-label="Address" placeholder="Search or enter address" />
        <button type="button" className="icon-btn" title="Open in your browser" onClick={() => current && void openUrl(current.url)}><Icon name="external" size={14} /></button>
      </form>
      <div className="bview" ref={view}>
        {!inTauri && current && <iframe key={current.id} src={current.url} title={current.title} />}
      </div>
    </aside>
  );
}
