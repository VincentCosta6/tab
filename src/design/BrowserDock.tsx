import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { api, inTauri, onBrowserEvent, openUrl } from "../lib/api";
import {
  closeBrowser, closeBrowserTab, handBack, newBrowserTab, patchBrowserTab, selectBrowserTab, setBrowserExpanded, stopAgent, useBrowser, withHost,
} from "../lib/browser";
import { useConfig, useSaveConfig } from "../lib/hooks";
import { ImportLogins, ImportOffer } from "./BrowserLogins";
import type { AgentTabState as AgentTab } from "../lib/browser";
import { Button, cx } from "./components";
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
  const [importing, setImporting] = useState(false);
  const config = useConfig().data;
  const saveConfig = useSaveConfig();
  const canImport = config?.browser?.import_logins === true;
  // Asked once: while it's unanswered and there's a browser to import from.
  const unasked = !!config && config.browser?.import_logins == null;
  const [browsers, setBrowsers] = useState<string[]>([]);
  useEffect(() => {
    if (unasked) void api.browserLoginBrowsers().then(setBrowsers, () => {});
  }, [unasked]);
  const answerOffer = (yes: boolean) => {
    if (!config) return;
    saveConfig.mutate({ ...config, browser: { ...config.browser, import_logins: yes } });
    setImporting(yes);
  };
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

  // Watch mode: tell tab which agent tab the person can see right now.
  useEffect(() => {
    if (!inTauri) return;
    const report = () => {
      const t = tabs.find((x) => x.id === active);
      const seen = !!t?.agent && !dragging && document.visibilityState === "visible" && document.hasFocus();
      void api.browserAgentWatch(seen ? t!.id : null).catch(() => {});
    };
    report();
    window.addEventListener("focus", report);
    window.addEventListener("blur", report);
    document.addEventListener("visibilitychange", report);
    return () => {
      window.removeEventListener("focus", report);
      window.removeEventListener("blur", report);
      document.removeEventListener("visibilitychange", report);
      void api.browserAgentWatch(null).catch(() => {});
    };
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
          <button key={t.id} role="tab" aria-selected={t.id === active} className={cx("btab", t.id === active && "active", t.agent && "agent")} onClick={() => selectBrowserTab(t.id)} onAuxClick={(e) => e.button === 1 && closeBrowserTab(t.id)} title={t.url}>
            <Icon name={t.loading ? "refresh" : t.agent ? "bot" : "globe"} size={12} />
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
        {canImport && !current?.agent && <button type="button" className={cx("icon-btn", importing && "active")} title="Import logins from another browser" onClick={() => setImporting((v) => !v)}><Icon name="download" size={14} /></button>}
        <button type="button" className="icon-btn" title="Open in your browser" onClick={() => current && void openUrl(current.url)}><Icon name="external" size={14} /></button>
      </form>
      {unasked && browsers.length > 0 && <ImportOffer browsers={browsers} onAnswer={answerOffer} />}
      {canImport && importing && !current?.agent && <ImportLogins url={current?.url} onClose={() => setImporting(false)} onImported={() => nav("reload")} />}
      {current?.agent && <AgentBanner tabId={current.id} a={current.agent} level={config?.browser?.agents ?? "off"} />}
      <div className={cx("bview", current?.agent && "agent")} ref={view}>
        {!inTauri && current && <iframe key={current.id} src={current.url} title={current.title} />}
      </div>
    </aside>
  );
}

/** What an agent's tab is doing, and the person's controls over it. */
function AgentBanner({ tabId, a, level }: { tabId: string; a: AgentTab; level: string }) {
  const progress = a.goal ? `${a.goal} · ${a.done ?? 0} of ${a.steps ?? 0} steps` : level === "read_only" ? "Read-only: it can't sign in, click or type." : "No plan approved yet: it can only read.";
  return (
    <div className={cx("bagent", (a.paused || a.waiting) && "attention")} role="status">
      <Icon name="bot" size={13} />
      <span>
        {a.paused ? <>You have this tab: {a.paused}.</> : a.waiting ? <>Waiting for you to watch, to {a.waiting}.</> : <>An agent is using this tab. {progress}</>}
        {a.lent && <> · Your {a.lent} login is lent.</>}
        {a.actions != null && <span className="sub"> {a.actions}/{a.maxActions} actions</span>}
      </span>
      {a.paused && <Button size="sm" variant="primary" type="button" onClick={() => handBack(tabId)}>Hand back</Button>}
      <Button size="sm" type="button" title="Ends the agent's browsing in this chat: its plan, lent logins and tabs" onClick={() => stopAgent(a.sessionId)}>Stop</Button>
      <Button size="sm" variant="ghost" type="button" onClick={() => closeBrowserTab(tabId)}>Close</Button>
    </div>
  );
}
