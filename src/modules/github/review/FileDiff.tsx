// One file's diff in Monaco, with review threads and the comment composer
// laid out inline under their lines, like github.com.
//
// Threads don't live inside Monaco's DOM (it swallows mouse and key events).
// Each one reserves space with an empty view zone, and the real card is a
// normal React element in an overlay positioned over that zone.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { PrFile, ReviewThread } from "../../../lib/types";
import { baseUri, ensureModel, headUri, monaco } from "../monaco";
import { useReview, type Side } from "./context";
import { useFileText } from "./data";
import { NewThread, ThreadCard } from "./Thread";

const ZONE_PAD = 10;

interface Zone { key: string; side: Side; after: number; thread?: ReviewThread }

export function themeName(dark: boolean) {
  return dark ? "tab-dark" : "tab-light";
}

let themed = false;
export function defineThemes() {
  if (themed) return;
  themed = true;
  const css = getComputedStyle(document.documentElement);
  const v = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback;
  monaco.editor.defineTheme("tab-light", {
    base: "vs", inherit: true, rules: [],
    colors: { "editor.background": "#ffffff", "editorGutter.background": "#ffffff", "diffEditor.insertedLineBackground": "#ecf8ef", "diffEditor.removedLineBackground": "#fdefed", "diffEditor.insertedTextBackground": "#b7e8c488", "diffEditor.removedTextBackground": "#f7c2bd88", "editorLineNumber.foreground": "#a1a1aa", "diffEditor.unchangedRegionBackground": "#f4f4f6" },
  });
  monaco.editor.defineTheme("tab-dark", {
    base: "vs-dark", inherit: true, rules: [],
    colors: { "editor.background": v("--bg-raised", "#18181b"), "editorGutter.background": v("--bg-raised", "#18181b"), "diffEditor.insertedLineBackground": "#0f2417", "diffEditor.removedLineBackground": "#2a1212", "diffEditor.insertedTextBackground": "#2a6a3d55", "diffEditor.removedTextBackground": "#8a2d2655", "editorLineNumber.foreground": "#5c5c66", "diffEditor.unchangedRegionBackground": "#141417" },
  });
}

const GUTTER = new Set([
  monaco.editor.MouseTargetType.GUTTER_GLYPH_MARGIN,
  monaco.editor.MouseTargetType.GUTTER_LINE_NUMBERS,
  monaco.editor.MouseTargetType.GUTTER_LINE_DECORATIONS,
]);
const LINE_TARGETS = new Set([...GUTTER, monaco.editor.MouseTargetType.CONTENT_TEXT, monaco.editor.MouseTargetType.CONTENT_EMPTY]);

export function FileDiff({ file, fill, reveal, initialHeight, onHeight }: { file: PrFile; fill?: boolean; reveal?: { line: number; nonce: number }; initialHeight?: number; onHeight?: (h: number) => void }) {
  const { pr, src, mode, dark, composer, setComposer, searchFor } = useReview();
  const path = file.filename;
  const basePath = file.previous_filename ?? file.filename;
  const base = useFileText(src, "base", basePath, file.status !== "added");
  const head = useFileText(src, "head", path, file.status !== "removed");
  const loaded = (file.status === "added" || base.isSuccess) && (file.status === "removed" || head.isSuccess);
  const loadError = base.error ?? head.error;

  const host = useRef<HTMLDivElement>(null);
  const wrap = useRef<HTMLDivElement>(null);
  const editor = useRef<monaco.editor.IStandaloneDiffEditor | null>(null);
  // Stacked mode: the page scrolls, never the editor. `height` is the whole
  // diff's height (so the page scrollbar is honest); the editor itself is only
  // as tall as the scroll area (`viewH`), pinned in view, and fed the page's
  // scroll offset so Monaco renders just the visible lines however big the file.
  const [height, setHeight] = useState(fill ? 0 : (initialHeight ?? 120));
  const [viewH, setViewH] = useState(fill ? 0 : Math.min(initialHeight ?? 120, 900));
  const expectedTop = useRef(0);
  const [ready, setReady] = useState(0);

  // Threads and the composer for this file become zones.
  const zones = useMemo<Zone[]>(() => {
    const out: Zone[] = pr.threads
      .filter((t) => t.path === path && t.line && !t.outdated && (mode === "split" || t.side === "RIGHT"))
      .map((t) => ({ key: t.id, side: t.side, after: t.line!, thread: t }));
    if (composer && composer.path === path && (mode === "split" || composer.side === "RIGHT"))
      out.push({ key: "composer", side: composer.side, after: composer.end });
    return out;
  }, [pr.threads, path, mode, composer]);

  // Create the editor once.
  useEffect(() => {
    if (!host.current) return;
    defineThemes();
    const ed = monaco.editor.createDiffEditor(host.current, {
      readOnly: true,
      originalEditable: false,
      automaticLayout: true,
      renderSideBySide: mode === "split",
      useInlineViewWhenSpaceIsLimited: false,
      hideUnchangedRegions: { enabled: true, contextLineCount: 3, minimumLineCount: 4, revealLineCount: 20 },
      minimap: { enabled: false },
      scrollBeyondLastLine: false,
      renderOverviewRuler: false,
      glyphMargin: true,
      fontSize: 12.5,
      lineHeight: 19,
      stickyScroll: { enabled: false },
      renderMarginRevertIcon: false,
      enableSplitViewResizing: true,
      diffAlgorithm: "advanced",
      ignoreTrimWhitespace: false,
      contextmenu: true,
      theme: themeName(dark),
      scrollbar: fill
        ? { alwaysConsumeMouseWheel: false }
        : { vertical: "hidden", handleMouseWheel: true, alwaysConsumeMouseWheel: false, verticalScrollbarSize: 0, useShadows: false },
    });
    editor.current = ed;
    setReady((n) => n + 1);
    return () => {
      // The models are shared with the project and other editors; keep them.
      ed.setModel(null);
      ed.dispose();
      editor.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    editor.current?.updateOptions({ renderSideBySide: mode === "split" });
  }, [mode]);
  useEffect(() => {
    monaco.editor.setTheme(themeName(dark));
  }, [dark]);

  // Models.
  useEffect(() => {
    const ed = editor.current;
    if (!ed || !loaded) return;
    const original = ensureModel(baseUri(basePath), base.data ?? "", basePath);
    const modified = ensureModel(headUri(path), file.status === "removed" ? "" : (head.data ?? ""), path);
    ed.setModel({ original, modified });
    setReady((n) => n + 1);
  }, [loaded, base.data, head.data, basePath, path, file.status]);

  // Total height follows the content.
  useEffect(() => {
    const ed = editor.current;
    if (!ed || fill) return;
    const update = () => {
      const h = Math.max(ed.getOriginalEditor().getContentHeight(), ed.getModifiedEditor().getContentHeight());
      setHeight(h);
      onHeight?.(h);
    };
    const subs = [ed.getOriginalEditor().onDidContentSizeChange(update), ed.getModifiedEditor().onDidContentSizeChange(update), ed.onDidUpdateDiff(update)];
    update();
    return () => subs.forEach((s) => s.dispose());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready, fill]);

  // The editor is as tall as the scroll area, no taller, and follows the page.
  useEffect(() => {
    const ed = editor.current;
    const root = wrap.current?.closest<HTMLElement>(".rv-stack");
    if (fill || !ed || !root) return;
    const modified = ed.getModifiedEditor();
    const size = () => setViewH(Math.max(120, Math.min(height, root.clientHeight)));
    const follow = () => {
      const w = wrap.current;
      if (!w) return;
      const hostH = Math.min(height, root.clientHeight);
      const off = Math.max(0, Math.min(height - hostH, root.getBoundingClientRect().top - w.getBoundingClientRect().top));
      if (Math.abs(modified.getScrollTop() - off) > 0.5) {
        expectedTop.current = off;
        modified.setScrollTop(off);
      }
    };
    size();
    follow();
    root.addEventListener("scroll", follow, { passive: true });
    const ro = new ResizeObserver(() => { size(); follow(); });
    ro.observe(root);
    // Scrolls the editor does on its own (keyboard, find, go to definition) move the page instead.
    const sub = modified.onDidScrollChange((e) => {
      if (!e.scrollTopChanged || Math.abs(e.scrollTop - expectedTop.current) <= 1) return;
      root.scrollTop += e.scrollTop - expectedTop.current;
      expectedTop.current = e.scrollTop;
    });
    // Vertical wheel over the editor scrolls the page; sideways still scrolls long lines.
    const host_ = host.current;
    const wheel = (e: WheelEvent) => {
      if (e.ctrlKey || e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) return;
      root.scrollTop += e.deltaY * (e.deltaMode === 1 ? 19 : e.deltaMode === 2 ? root.clientHeight : 1);
      e.preventDefault();
      e.stopPropagation();
    };
    host_?.addEventListener("wheel", wheel, { capture: true, passive: false });
    return () => {
      root.removeEventListener("scroll", follow);
      ro.disconnect();
      sub.dispose();
      host_?.removeEventListener("wheel", wheel, { capture: true });
    };
  }, [ready, fill, height]);

  // Reveal a line (from search, the explorer, or go to definition).
  useEffect(() => {
    const ed = editor.current?.getModifiedEditor();
    if (!ed || !reveal || !loaded) return;
    const t = window.setTimeout(() => {
      ed.revealLineInCenter(reveal.line);
      ed.setSelection(new monaco.Range(reveal.line, 1, reveal.line, 1));
      ed.focus();
    }, 60);
    return () => clearTimeout(t);
  }, [reveal, loaded, ready]);

  // Hover "+" in the gutter, click or drag to comment, and editor actions.
  useEffect(() => {
    const ed = editor.current;
    if (!ed || !loaded) return;
    const subs: monaco.IDisposable[] = [];
    let drag: { side: Side; start: number; end: number } | null = null;
    const sides = [["LEFT", ed.getOriginalEditor()], ["RIGHT", ed.getModifiedEditor()]] as [Side, monaco.editor.IStandaloneCodeEditor][];
    const paints: (() => void)[] = [];
    const paintAll = () => paints.forEach((p) => p());
    const finish = () => {
      if (!drag) return;
      const d = drag;
      drag = null;
      paintAll();
      // Dragging down the line numbers selects those lines; use the selection.
      const sel = sides.find(([s]) => s === d.side)![1].getSelection();
      let [a, b] = [Math.min(d.start, d.end), Math.max(d.start, d.end)];
      if (sel && sel.endLineNumber > sel.startLineNumber && sel.startLineNumber <= d.start) {
        a = sel.startLineNumber;
        b = sel.endColumn === 1 ? sel.endLineNumber - 1 : sel.endLineNumber;
      }
      setComposer({ path, side: d.side, start: a, end: Math.max(a, b) });
    };
    for (const [side, e] of sides) {
      e.updateOptions({ glyphMargin: true, lineDecorationsWidth: 6 });
      const hover = e.createDecorationsCollection();
      const dragDeco = e.createDecorationsCollection();
      paints.push(() => {
        if (!drag || drag.side !== side) return dragDeco.clear();
        const [a, b] = [Math.min(drag.start, drag.end), Math.max(drag.start, drag.end)];
        dragDeco.set([{ range: new monaco.Range(a, 1, b, 1), options: { isWholeLine: true, className: "rv-line-selected" } }]);
      });
      subs.push(
        e.onMouseMove((ev) => {
          const line = ev.target.position?.lineNumber;
          if (!line || !LINE_TARGETS.has(ev.target.type)) return hover.clear();
          hover.set([{ range: new monaco.Range(line, 1, line, 1), options: { glyphMarginClassName: "rv-add-glyph", glyphMarginHoverMessage: { value: "Comment on this line (drag to select several)" } } }]);
          if (drag && drag.side === side && drag.end !== line) {
            drag.end = line;
            paintAll();
          }
        }),
        e.onMouseLeave(() => hover.clear()),
        e.onMouseDown((ev) => {
          const line = ev.target.position?.lineNumber;
          if (!line || !GUTTER.has(ev.target.type)) return;
          drag = { side, start: line, end: line };
          paintAll();
        }),
        e.onMouseUp(finish),
        e.addAction({
          id: "tab.review.comment",
          label: "Comment on selected lines",
          keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.KeyM],
          contextMenuGroupId: "navigation",
          contextMenuOrder: 0,
          run: (x) => {
            const s = x.getSelection();
            if (s) setComposer({ path, side, start: s.startLineNumber, end: s.endColumn === 1 && s.endLineNumber > s.startLineNumber ? s.endLineNumber - 1 : s.endLineNumber });
          },
        }),
        e.addAction({
          id: "tab.review.search",
          label: "Search the repository for this",
          keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.KeyF],
          contextMenuGroupId: "navigation",
          contextMenuOrder: 1,
          run: (x) => {
            const s = x.getSelection();
            const m = x.getModel();
            if (!s || !m) return;
            const text = s.isEmpty() ? (m.getWordAtPosition(s.getStartPosition())?.word ?? "") : m.getValueInRange(s);
            if (text.trim()) searchFor(text.split("\n")[0]);
          },
        }),
      );
    }
    window.addEventListener("mouseup", finish);
    return () => {
      subs.forEach((s) => s.dispose());
      window.removeEventListener("mouseup", finish);
    };
  }, [loaded, ready, path, setComposer, searchFor]);

  // Markers in the gutter for threads, and the composer's range.
  useEffect(() => {
    const ed = editor.current;
    if (!ed || !loaded) return;
    const cols = ([["LEFT", ed.getOriginalEditor()], ["RIGHT", ed.getModifiedEditor()]] as const).map(([side, e]) => {
      const decos: monaco.editor.IModelDeltaDecoration[] = [];
      for (const t of pr.threads) {
        if (t.path !== path || !t.line || t.outdated || t.side !== side) continue;
        decos.push({ range: new monaco.Range(t.line, 1, t.line, 1), options: { glyphMarginClassName: t.resolved ? "rv-thread-glyph resolved" : "rv-thread-glyph" } });
        if (t.start_line && t.start_line < t.line && !t.resolved)
          decos.push({ range: new monaco.Range(t.start_line, 1, t.line, 1), options: { isWholeLine: true, className: "rv-line-thread" } });
      }
      if (composer && composer.path === path && composer.side === side)
        decos.push({ range: new monaco.Range(composer.start, 1, composer.end, 1), options: { isWholeLine: true, className: "rv-line-selected" } });
      return e.createDecorationsCollection(decos);
    });
    return () => cols.forEach((c) => c.clear());
  }, [pr.threads, composer, path, loaded, ready]);

  // Zones reserve space; the overlay holds the real cards.
  const items = useRef(new Map<string, HTMLDivElement>());
  const zoneState = useRef(new Map<string, { id: string; zone: monaco.editor.IViewZone; side: Side; dom: HTMLDivElement }>());
  const heights = useRef(new Map<string, number>());
  const zoneEpoch = useRef("");
  const refs = useRef(new Map<string, (el: HTMLDivElement | null) => (() => void) | void>());

  const sync = useRef(() => {});
  sync.current = () => {
    const w = wrap.current;
    const ed = editor.current;
    if (!w || !ed) return;
    const box = w.getBoundingClientRect();
    for (const [key, z] of zoneState.current) {
      const el = items.current.get(key);
      if (!el) continue;
      const e = z.side === "LEFT" ? ed.getOriginalEditor() : ed.getModifiedEditor();
      const edDom = e.getDomNode();
      const visible = z.dom.isConnected && z.dom.style.display !== "none";
      if (!visible || !edDom) {
        el.style.visibility = "hidden";
        continue;
      }
      const r = z.dom.getBoundingClientRect();
      const er = edDom.getBoundingClientRect();
      const layout = e.getLayoutInfo();
      const left = er.left - box.left + layout.contentLeft - 4;
      el.style.visibility = "visible";
      el.style.transform = `translate(${left}px, ${r.top - box.top + ZONE_PAD / 2}px)`;
      el.style.width = `${Math.max(240, Math.min(layout.contentWidth - 16, 900))}px`;
    }
  };

  useLayoutEffect(() => {
    const ed = editor.current;
    if (!ed || !loaded) return;
    const accessors: [Side, monaco.editor.ICodeEditor][] = [["LEFT", ed.getOriginalEditor()], ["RIGHT", ed.getModifiedEditor()]];
    const wanted = new Map(zones.map((z) => [`${z.key}:${z.side}:${z.after}`, z]));
    // New models or a new layout drop every zone; start over.
    const epoch = `${mode}:${ready}`;
    if (zoneEpoch.current !== epoch) {
      zoneEpoch.current = epoch;
      for (const [, z] of zoneState.current) {
        const e = z.side === "LEFT" ? ed.getOriginalEditor() : ed.getModifiedEditor();
        e.changeViewZones((acc) => acc.removeZone(z.id));
      }
      zoneState.current.clear();
    }
    // Remove zones that went away or moved.
    for (const [key, z] of [...zoneState.current]) {
      const zone = zones.find((x) => x.key === key);
      if (zone && zone.side === z.side && zone.after === z.zone.afterLineNumber) continue;
      const e = z.side === "LEFT" ? ed.getOriginalEditor() : ed.getModifiedEditor();
      e.changeViewZones((acc) => acc.removeZone(z.id));
      zoneState.current.delete(key);
    }
    for (const [, z] of wanted) {
      if (zoneState.current.has(z.key)) continue;
      const e = accessors.find(([s]) => s === z.side)![1];
      const dom = document.createElement("div");
      dom.className = "rv-zone";
      const zone: monaco.editor.IViewZone = {
        afterLineNumber: z.after,
        heightInPx: heights.current.get(z.key) ?? 90,
        domNode: dom,
        showInHiddenAreas: true,
        suppressMouseDown: true,
        onDomNodeTop: () => requestAnimationFrame(() => sync.current()),
      };
      let id = "";
      e.changeViewZones((acc) => (id = acc.addZone(zone)));
      zoneState.current.set(z.key, { id, zone, side: z.side, dom });
    }
    requestAnimationFrame(() => sync.current());
  }, [zones, loaded, ready, mode]);

  // Keep the overlay in place as the editor scrolls, lays out, or resizes.
  useEffect(() => {
    const ed = editor.current;
    if (!ed) return;
    const s = () => requestAnimationFrame(() => sync.current());
    const subs = [ed.getModifiedEditor().onDidScrollChange(s), ed.getOriginalEditor().onDidScrollChange(s), ed.getModifiedEditor().onDidLayoutChange(s), ed.onDidUpdateDiff(s)];
    const ro = new ResizeObserver(s);
    if (wrap.current) ro.observe(wrap.current);
    return () => {
      subs.forEach((x) => x.dispose());
      ro.disconnect();
    };
  }, [ready]);

  // Cards report their height so their zones can match it.
  const measure = (key: string) => {
    let f = refs.current.get(key);
    if (!f) {
      f = measureEl(key);
      refs.current.set(key, f);
    }
    return f;
  };
  const measureEl = (key: string) => (el: HTMLDivElement | null) => {
    if (!el) {
      items.current.delete(key);
      return;
    }
    items.current.set(key, el);
    const ro = new ResizeObserver(() => {
      const h = Math.ceil(el.offsetHeight) + ZONE_PAD;
      if (heights.current.get(key) === h) return;
      heights.current.set(key, h);
      const z = zoneState.current.get(key);
      const ed = editor.current;
      if (!z || !ed) return;
      z.zone.heightInPx = h;
      const e = z.side === "LEFT" ? ed.getOriginalEditor() : ed.getModifiedEditor();
      e.changeViewZones((acc) => acc.layoutZone(z.id));
      requestAnimationFrame(() => sync.current());
    });
    ro.observe(el);
    sync.current();
    return () => ro.disconnect();
  };

  const hiddenThreads = pr.threads.filter((t) => t.path === path && (t.outdated || !t.line || (mode === "unified" && t.side === "LEFT")));

  return (
    <div className={fill ? "rv-diff rv-diff-fill" : "rv-diff"}>
      {loadError && <p className="rv-error rv-pad">Couldn't load this file: {String(loadError)}</p>}
      {!loaded && !loadError && <div className="rv-diff-loading skeleton" style={{ height: fill ? "100%" : 120 }} />}
      <div ref={wrap} className={fill ? "rv-diff-wrap" : "rv-diff-wrap rv-diff-stacked"} style={fill ? undefined : { height }}>
        <div ref={host} className="rv-diff-host" style={fill ? undefined : { height: Math.min(height, viewH) }} />
        <div className="rv-overlay">
          {zones.map((z) => (
            <div key={z.key} ref={measure(z.key)} className="rv-overlay-item" style={{ visibility: "hidden" }}>
              {z.thread ? <ThreadCard thread={z.thread} /> : composer && <NewThread {...composer} onDone={() => setComposer(null)} />}
            </div>
          ))}
        </div>
      </div>
      {hiddenThreads.length > 0 && (
        <div className="rv-outdated">
          <div className="label-sm">{hiddenThreads.some((t) => t.outdated) ? "Outdated and other comments" : "Comments on removed lines"}</div>
          {hiddenThreads.map((t) => (
            <div key={t.id} className="rv-outdated-item">
              {t.diff_hunk && <pre className="rv-hunk">{t.diff_hunk.split("\n").slice(-6).join("\n")}</pre>}
              <ThreadCard thread={t} />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
