// The review workspace: an editor-style shell around one pull request.
//
//   header      PR info, status, Actions, Submit review
//   activity    Explorer · Search · Files changed · Threads
//   editor      tabs: Changes (every diff, stacked), Overview, opened files
//   status bar  checkout, language service, view options

import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import { api } from "../../../lib/api";
import { openItem } from "../../../lib/links";
import type { QueueItem } from "../../../lib/types";
import { ContextDrawer } from "../../work/ContextDrawer";
import { keys } from "../../../lib/query";
import { useLanguageServers } from "../lsp";
import { holdProject, loadProject, projectStore, setOpener } from "../monaco";
import { AiPanel, FixBanner, useAi } from "./ai";
import { Boot } from "./Boot";
import { Ctx, type ComposerTarget, type DiffMode, type ReviewCtx } from "./context";
import { isReviewImage, readFile, useActions, usePull, useSource, useWorkspace } from "./data";
import { DiffStack, FileHeader } from "./DiffStack";
import { FileDiff } from "./FileDiff";
import { FileView } from "./FileView";
import { Header } from "./Header";
import { ImageDiff, ImageFile } from "./ImageDiff";
import { LinkedWork } from "./LinkedWork";
import { LanguageStatus } from "./LanguageServers";
import { buildTree, compactTree, isViewed, openThreads, splitPath, treeOrder } from "./model";
import { Overview } from "./Overview";
import "./review.css";
import { ActivityBar, ChangesPanel, Explorer, SearchPanel, ThreadsPanel, type Panel } from "./Sidebar";

type Tab =
  | { id: "changes"; kind: "changes" }
  | { id: "overview"; kind: "overview" }
  | { id: string; kind: "file" | "diff"; path: string; preview: boolean; reveal?: { line: number; column?: number; nonce: number } };

function stored<T>(key: string, fallback: T): [T, (v: T) => void] {
  const [v, setV] = useState<T>(() => {
    try {
      const s = localStorage.getItem(`tab.review.${key}`);
      return s ? (JSON.parse(s) as T) : fallback;
    } catch {
      return fallback;
    }
  });
  const set = useCallback((x: T) => {
    setV(x);
    try {
      localStorage.setItem(`tab.review.${key}`, JSON.stringify(x));
    } catch {
      /* storage unavailable */
    }
  }, [key]);
  return [v, set];
}

function useDark() {
  const q = typeof window !== "undefined" ? window.matchMedia("(prefers-color-scheme: dark)") : null;
  const [dark, setDark] = useState(!!q?.matches);
  useEffect(() => {
    if (!q) return;
    const on = () => setDark(q.matches);
    q.addEventListener("change", on);
    return () => q.removeEventListener("change", on);
  }, [q]);
  return dark;
}

let nonce = 0;

/** `onMerged` runs once a merge from here goes through, so the host can offer closing the task. */
export default function ReviewWorkspace({ repo, number, sample, item, onBack, onMerged }: { repo: string; number: number; sample: boolean; item?: QueueItem; onBack: () => void; onMerged?: () => void }) {
  const qc = useQueryClient();
  const dark = useDark();
  const pull = usePull(repo, number, sample);
  // The query cache is persisted, so a copy from an earlier visit can be days
  // old: wait for this open's fetch before using it, unless it was just fetched.
  const [recent] = useState(() => Date.now() - pull.dataUpdatedAt < 30_000);
  const fresh = pull.isFetchedAfterMount || recent;
  const pr = fresh ? pull.data : undefined;
  const boot = useWorkspace(pr);
  const src = useSource(pr, boot.ws);
  const act = useActions(pr);

  const [panel, setPanel] = stored<Panel | null>("panel", "changes");
  const [mode, setMode] = stored<DiffMode>("mode", "split");
  const [autoRead, setAutoRead] = stored("autoRead", true);
  const [sideWidth, setSideWidth] = stored("sideWidth", 280);
  const [tabs, setTabs] = useState<Tab[]>([{ id: "changes", kind: "changes" }, { id: "overview", kind: "overview" }]);
  const [active, setActive] = useState("changes");
  const [composer, setComposer] = useState<ComposerTarget | null>(null);
  const [scrollTo, setScrollTo] = useState<{ path: string; nonce: number } | null>(null);
  const [seed, setSeed] = useState<{ text: string; nonce: number } | null>(null);
  const [toast, setToast] = useState<{ text: string; tone?: "danger" | "success"; id: number } | null>(null);
  const project = useSyncExternalStore(projectStore.subscribe, projectStore.get);

  const notify = useCallback((text: string, tone?: "danger" | "success") => setToast({ text, tone, id: ++nonce }), []);
  useEffect(() => {
    if (!toast) return;
    const t = window.setTimeout(() => setToast(null), toast.tone === "danger" ? 7000 : 2500);
    return () => clearTimeout(t);
  }, [toast]);

  const changed = useMemo(() => new Set(pr?.files.map((f) => f.filename) ?? []), [pr?.files]);
  // The combined view follows the Files changed tree: grouped by folder.
  const ordered = useMemo(() => {
    if (!pr) return [];
    const by = new Map(pr.files.map((f) => [f.filename, f]));
    return treeOrder(compactTree(buildTree(pr.files.map((f) => f.filename)))).map((p) => by.get(p)!).filter(Boolean);
  }, [pr]);

  const openFile = useCallback((path: string, line?: number, column?: number) => {
    const kind = changed.has(path) ? "diff" : "file";
    const id = `${kind}:${path}`;
    const reveal = line ? { line, column, nonce: ++nonce } : undefined;
    setTabs((ts) => {
      const existing = ts.find((t) => t.id === id);
      if (existing) return ts.map((t) => (t.id === id && (t.kind === "file" || t.kind === "diff") ? { ...t, reveal: reveal ?? t.reveal } : t));
      // Like VS Code, a single click replaces the previous preview tab.
      const next = ts.filter((t) => !(t.kind === "file" || t.kind === "diff") || !t.preview);
      return [...next, { id, kind, path, preview: true, reveal }];
    });
    setActive(id);
  }, [changed]);

  const showChange = useCallback((path: string) => {
    setActive("changes");
    setScrollTo({ path, nonce: ++nonce });
  }, []);

  const searchFor = useCallback((text: string) => {
    setPanel("search");
    setSeed({ text, nonce: ++nonce });
  }, [setPanel]);

  // Go to definition into another file opens it here.
  useEffect(() => {
    setOpener(openFile);
    return () => setOpener(null);
  }, [openFile]);

  // Load the repository's sources so the language service sees every file.
  // They're freed a while after the last review closes.
  useEffect(() => holdProject(), []);
  const ws = boot.ws;
  useEffect(() => {
    if (!pr || !ws) return;
    const read = pr.sample
      ? async (paths: string[]) => paths.map((p) => [p, pr.sample_tree.find((t) => t.path === p)?.content ?? pr.files.find((f) => f.filename === p)?.modified ?? ""] as [string, string])
      : (paths: string[]) => api.workspaceFiles(pr.repo, pr.number, paths);
    loadProject(`${pr.repo}#${pr.number}@${ws.head_sha}`, ws.files, read);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ws, pr?.repo, pr?.number, pr?.sample]);

  // Go, SQL, Rust and Python get a language server in the checkout.
  const srcRef = useRef(src);
  srcRef.current = src;
  const readHead = useCallback((path: string) => (srcRef.current ? readFile(srcRef.current, "head", path) : Promise.reject(new Error("not ready"))), []);
  useLanguageServers(
    pr && ws ? { repo: pr.repo, number: pr.number, key: `${pr.repo}#${pr.number}@${ws.head_sha}`, files: ws.files, local: !pr.sample && boot.phase === "ready" } : null,
    readHead,
  );

  // Ctrl+Shift+E/F/G switch panels, like VS Code.
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || !e.shiftKey) return;
      const p = ({ e: "explorer", f: "search", g: "changes" } as const)[e.key.toLowerCase() as "e" | "f" | "g"];
      if (p) {
        e.preventDefault();
        setPanel(p);
      }
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  }, [setPanel]);

  const dragging = useRef(false);
  useEffect(() => {
    const move = (e: MouseEvent) => dragging.current && setSideWidth(Math.max(200, Math.min(560, e.clientX - 48)));
    const up = () => (dragging.current = false);
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    return () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
  }, [setSideWidth]);

  const ai = useAi(pr, notify);
  const ctx = useMemo<ReviewCtx | null>(
    () => (pr && src && ai ? { pr, src, act, mode, dark, composer, setComposer, openFile, showChange, searchFor, notify, ai } : null),
    [pr, src, act, mode, dark, composer, openFile, showChange, searchFor, notify, ai],
  );

  if (pull.error && !pr)
    return (
      <div className="rv-boot">
        <div className="rv-boot-card">
          <h2>Couldn't load {repo}#{number}</h2>
          <p className="rv-error">{String(pull.error)}</p>
          <div className="rv-boot-actions"><button className="btn btn-secondary btn-md" onClick={onBack}>Back</button><button className="btn btn-primary btn-md" onClick={() => pull.refetch()}>Try again</button></div>
        </div>
      </div>
    );
  if (!pr || !ctx || boot.phase === "booting" || boot.phase === "error") return <Boot pr={pr ?? pull.data} loading={!pr} boot={boot} onBack={onBack} />;

  const local = boot.phase === "ready";
  const files = ws?.files ?? pr.files.map((f) => f.filename);
  const tab = tabs.find((t) => t.id === active) ?? tabs[0];
  const unread = pr.files.filter((f) => !isViewed(f)).length;
  const activePath = tab.kind === "file" || tab.kind === "diff" ? tab.path : null;
  const closeTab = (id: string) => {
    setTabs((ts) => ts.filter((t) => t.id !== id));
    if (active === id) setActive("changes");
  };

  return (
    <Ctx.Provider value={ctx}>
      <div className="rv">
        <Header onBack={onBack} onMerged={onMerged} local={local} onRefresh={() => qc.invalidateQueries({ queryKey: keys.pull(repo, number) })} refreshing={pull.isFetching} />
        <LinkedWork url={pr.url} />
        {item && <ContextDrawer item={item} onSelect={openItem} />}
        <FixBanner ai={ctx.ai} local={boot.phase !== "remote"} />
        <div className="rv-body" style={{ gridTemplateColumns: `48px ${panel ? `${sideWidth}px ` : ""}minmax(0, 1fr)${ctx.ai.open ? " minmax(320px, 400px)" : ""}` }}>
          <ActivityBar panel={panel} onPanel={(p) => setPanel(panel === p ? null : p)} unread={unread} threads={openThreads(pr)} />
          {panel && (
            <aside className="rv-side">
              {panel === "explorer" && <Explorer files={files} active={activePath} />}
              {panel === "search" && <SearchPanel seed={seed} local={local || pr.sample} />}
              {panel === "changes" && <ChangesPanel autoRead={autoRead} setAutoRead={setAutoRead} active={activePath} />}
              {panel === "threads" && <ThreadsPanel />}
              <div className="rv-side-resize" onMouseDown={(e) => { e.preventDefault(); dragging.current = true; }} />
            </aside>
          )}
          <main className="rv-main">
            <div className="rv-tabs">
              {tabs.map((t) => {
                const label = t.kind === "changes" ? "Changes" : t.kind === "overview" ? "Overview" : splitPath(t.path).name;
                const icon = t.kind === "changes" ? "file-diff" : t.kind === "overview" ? "git-pull-request" : t.kind === "diff" ? "file-diff" : "file";
                const closable = t.kind === "file" || t.kind === "diff";
                return (
                  <div
                    key={t.id}
                    className={cx("rv-tab", t.id === tab.id && "active", closable && t.preview && "preview")}
                    onClick={() => setActive(t.id)}
                    onDoubleClick={() => closable && setTabs((ts) => ts.map((x) => (x.id === t.id ? { ...x, preview: false } as Tab : x)))}
                    onAuxClick={(e) => e.button === 1 && closable && closeTab(t.id)}
                    title={closable ? t.path : undefined}
                  >
                    <Icon name={icon} size={13} />
                    <span>{label}</span>
                    {t.kind === "changes" && <span className="rv-tab-count">{pr.files.length - unread}/{pr.files.length}</span>}
                    {closable && (
                      <button className="rv-tab-close" onClick={(e) => { e.stopPropagation(); closeTab(t.id); }} title="Close">
                        <Icon name="x" size={12} />
                      </button>
                    )}
                  </div>
                );
              })}
              <span className="rv-flex" />
              {(tab.kind === "changes" || tab.kind === "diff") && <div className="rv-seg rv-tabs-tools">
                <button className={cx(mode === "split" && "on")} onClick={() => setMode("split")} title="Side by side"><Icon name="columns" size={13} /> Split</button>
                <button className={cx(mode === "unified" && "on")} onClick={() => setMode("unified")} title="One column"><Icon name="rows" size={13} /> Unified</button>
              </div>}
            </div>
            <div className="rv-editor">
              {tab.kind === "changes" && <DiffStack files={ordered} autoRead={autoRead} scrollTo={scrollTo} />}
              {tab.kind === "overview" && <Overview />}
              {tab.kind === "diff" && (() => {
                const f = pr.files.find((x) => x.filename === tab.path);
                return f ? (
                  <div className="rv-diff-tab">
                    <FileHeader file={f} />
                    {isReviewImage(f.filename) ? <ImageDiff key={f.filename} file={f} fill /> : <FileDiff key={`${f.filename}:${mode}`} file={f} fill reveal={tab.reveal} />}
                  </div>
                ) : null;
              })()}
              {tab.kind === "file" && (isReviewImage(tab.path) ? <ImageFile key={tab.path} path={tab.path} reveal={tab.reveal} /> : <FileView key={tab.path} path={tab.path} reveal={tab.reveal} />)}
            </div>
          </main>
          {ctx.ai.open && <AiPanel ai={ctx.ai} pr={pr} openFile={openFile} />}
        </div>
        <footer className="rv-statusbar">
          <span><Icon name="git-branch" size={12} /> {pr.head_ref}</span>
          <span className="rv-mono">{pr.head_sha.slice(0, 7)}</span>
          <span title={ws?.path}>{local ? <><Icon name="folder" size={12} /> {pr.sample ? "Sample checkout" : ws?.reused ? "Local copy" : "Cloned"}</> : <><Icon name="cloud" size={12} /> GitHub API (no local copy)</>}</span>
          {project && ws && project.key === `${pr.repo}#${pr.number}@${ws.head_sha}` && (
            <span title={project.error}>{project.state === "loading" ? `Loading sources ${project.done}/${project.total}` : project.state === "error" ? "Some sources didn't load" : `TypeScript: ${project.total} files`}</span>
          )}
          <LanguageStatus local={local} sample={!!pr.sample} />
          <span className="rv-flex" />
          {pr.pending_review && <span className="text-warning">Review in progress</span>}
          <button className="rv-status-btn" onClick={() => setAutoRead(!autoRead)} title="Mark files viewed as you scroll past them">
            <Icon name={autoRead ? "eye" : "eye-off"} size={12} /> Auto-read {autoRead ? "on" : "off"}
          </button>
        </footer>
        {toast && <div key={toast.id} className={cx("rv-toast", toast.tone && `tone-${toast.tone}`)}>{toast.text}</div>}
      </div>
    </Ctx.Provider>
  );
}
