// Language servers for the review workspace. tab runs the server in the PR's
// checkout and this file is the client: it opens the files editors show
// with the server, turns hover, go to definition, references and semantic
// tokens into Monaco providers, and shows the server's diagnostics as
// markers. Models live at file:///<path>; the server sees <worktree>/<path>.
//
// TypeScript and JavaScript work without a server too (Monaco's built-in
// service). With vtsls running, the built-in features step aside so nothing
// shows twice. Either way the checkout's dependencies are installed first,
// shared between checkouts by lockfile (see crates/tab-core/src/deps.rs):
// vtsls reads them from disk, and without it their type declarations go to
// the built-in service.

import { useEffect } from "react";
import { api, onLsp } from "../../lib/api";
import type { DepsStatus, LspServer } from "../../lib/types";
import { ensureModel, headUri, languageFor, lspLanguageFor, monaco, pathOf, setBuiltinTs, setLibraryTypes } from "./monaco";

export type ServerRun = { state: "starting" | "ready" | "error" | "stopped"; error?: string };
export type DepsRun = { state: "installing" } | DepsStatus;
export interface LspState {
  /** Every server tab knows, with install state. Null until loaded. */
  servers: LspServer[] | null;
  /** Languages in the open checkout that a server covers. */
  wanted: string[];
  running: Record<string, ServerRun>;
  installing: Record<string, boolean>;
  installErrors: Record<string, string>;
  /** Shared dependencies for the open checkout, by ecosystem (`js`). */
  deps: Record<string, DepsRun>;
}

let state: LspState = { servers: null, wanted: [], running: {}, installing: {}, installErrors: {}, deps: {} };
const subs = new Set<() => void>();
const set = (f: (s: LspState) => LspState) => {
  state = f(state);
  subs.forEach((s) => s());
};
export const lspStore = {
  get: () => state,
  subscribe: (f: () => void) => {
    subs.add(f);
    return () => void subs.delete(f);
  },
};

interface Client {
  session: string;
  server: LspServer;
  root: string;
  caps: Record<string, any>;
  /** didOpen sent (its promise), by LSP uri. Requests wait for it. */
  opened: Map<string, Promise<void>>;
  versions: Map<string, number>;
  read: (path: string) => Promise<string>;
  /** Providers registered for this client only. */
  disposables: monaco.IDisposable[];
}

/** Running clients by language id. One checkout is open at a time. */
const byLang = new Map<string, Client>();
const bySession = new Map<string, Client>();
/** Diagnostics that arrived before their file was opened here. */
const diags = new Map<string, any[]>();

const LANGS = ["typescript", "javascript", "go", "sql", "rust", "python"];
const TS_LANGS = new Set(["typescript", "javascript"]);

const toLsp = (c: Client, uri: monaco.Uri) => `${c.root}/${pathOf(uri).split("/").map(encodeURIComponent).join("/")}`;
const fromLsp = (c: Client, uri: string): string | null => {
  const prefix = `${c.root}/`;
  if (!uri.startsWith(prefix)) return null;
  try {
    return uri.slice(prefix.length).split("/").map(decodeURIComponent).join("/");
  } catch {
    return null;
  }
};

const clientFor = (m: monaco.editor.ITextModel) => (m.uri.scheme === "file" ? byLang.get(m.getLanguageId()) : undefined);

/** Opens a model with its server once; resolves when didOpen is sent. */
function open(c: Client, m: monaco.editor.ITextModel): Promise<void> {
  const uri = toLsp(c, m.uri);
  const done = c.opened.get(uri);
  if (done) return done;
  c.versions.set(uri, 1);
  const languageId = lspLanguageFor(pathOf(m.uri), m.getLanguageId());
  const p = api.lspNotify(c.session, "textDocument/didOpen", { textDocument: { uri, languageId, version: 1, text: m.getValue() } }).catch(() => {});
  c.opened.set(uri, p);
  const pending = diags.get(`${c.session} ${uri}`);
  if (pending) setMarkers(c, m, pending);
  return p;
}

function setMarkers(c: Client, m: monaco.editor.ITextModel, list: any[]) {
  const sev = [0, monaco.MarkerSeverity.Error, monaco.MarkerSeverity.Warning, monaco.MarkerSeverity.Info, monaco.MarkerSeverity.Hint];
  monaco.editor.setModelMarkers(m, c.server.id, list.map((d) => ({
    severity: sev[d.severity ?? 1] ?? monaco.MarkerSeverity.Error,
    message: d.message,
    source: d.source ?? c.server.command,
    code: d.code != null ? String(d.code) : undefined,
    startLineNumber: d.range.start.line + 1,
    startColumn: d.range.start.character + 1,
    endLineNumber: d.range.end.line + 1,
    endColumn: d.range.end.character + 1,
  })));
}

/**
 * Opens what an editor shows. The project loads thousands of models for the
 * built-in service; only the ones on screen go to the server, which reads the
 * rest from disk.
 */
function openShown(m: monaco.editor.ITextModel | null) {
  if (!m) return;
  const c = clientFor(m);
  if (c) void open(c, m);
}

const shownModels = () => monaco.editor.getEditors().map((e) => e.getModel()).filter((m): m is monaco.editor.ITextModel => !!m);

// Keep the server's view of open files in step with the models.
let synced = false;
function syncModels() {
  if (synced) return;
  synced = true;
  monaco.editor.onDidCreateEditor((e) => {
    openShown(e.getModel());
    e.onDidChangeModel(() => openShown(e.getModel()));
  });
  monaco.editor.onDidCreateModel((m) => {
    m.onDidChangeContent(() => {
      const c = clientFor(m);
      if (!c) return;
      const uri = toLsp(c, m.uri);
      if (!c.opened.has(uri)) return;
      const v = (c.versions.get(uri) ?? 1) + 1;
      c.versions.set(uri, v);
      void api.lspNotify(c.session, "textDocument/didChange", { textDocument: { uri, version: v }, contentChanges: [{ text: m.getValue() }] }).catch(() => {});
    });
  });
  monaco.editor.onWillDisposeModel((m) => {
    const c = clientFor(m);
    if (!c) return;
    const uri = toLsp(c, m.uri);
    c.versions.delete(uri);
    if (c.opened.delete(uri)) void api.lspNotify(c.session, "textDocument/didClose", { textDocument: { uri } }).catch(() => {});
  });
  void onLsp((e) => {
    const c = bySession.get(e.session);
    if (!c) return;
    if (e.method === "tab/exited") {
      drop(c);
      set((s) => ({ ...s, running: { ...s.running, [c.server.id]: { state: "error", error: `${c.server.command} stopped` } } }));
      return;
    }
    if (e.method !== "textDocument/publishDiagnostics") return;
    const path = fromLsp(c, e.params.uri);
    if (path == null) return;
    const m = monaco.editor.getModel(headUri(path));
    if (m) setMarkers(c, m, e.params.diagnostics ?? []);
    else diags.set(`${c.session} ${e.params.uri}`, e.params.diagnostics ?? []);
  });
}

const pos = (p: monaco.Position) => ({ line: p.lineNumber - 1, character: p.column - 1 });
const range = (r: any): monaco.IRange => ({ startLineNumber: r.start.line + 1, startColumn: r.start.character + 1, endLineNumber: r.end.line + 1, endColumn: r.end.character + 1 });
const docPos = (c: Client, m: monaco.editor.ITextModel, p: monaco.Position) => ({ textDocument: { uri: toLsp(c, m.uri) }, position: pos(p) });

/** A request about a model, once the server has it open. */
async function ask<T = any>(c: Client, m: monaco.editor.ITextModel, method: string, params: unknown): Promise<T | null> {
  await open(c, m);
  return api.lspRequest<T>(c.session, method, params).catch(() => null);
}

function markdown(contents: any): monaco.IMarkdownString[] {
  const one = (x: any): monaco.IMarkdownString | null => {
    if (!x) return null;
    if (typeof x === "string") return { value: x };
    if ("kind" in x) return { value: x.kind === "markdown" ? x.value : "```\n" + x.value + "\n```" };
    if ("language" in x) return { value: "```" + x.language + "\n" + x.value + "\n```" };
    return null;
  };
  return (Array.isArray(contents) ? contents : [contents]).map(one).filter((x): x is monaco.IMarkdownString => !!x && !!x.value.trim());
}

/** Locations inside the checkout, with models made so peek can show them. */
async function locations(c: Client, result: any): Promise<monaco.languages.Location[]> {
  const list: any[] = !result ? [] : Array.isArray(result) ? result : [result];
  const out: monaco.languages.Location[] = [];
  for (const l of list.slice(0, 200)) {
    const uri = l.targetUri ?? l.uri;
    const r = l.targetSelectionRange ?? l.range;
    const path = uri && fromLsp(c, uri);
    if (path == null || !r) continue;
    out.push({ uri: headUri(path), range: range(r) });
  }
  const missing = [...new Set(out.map((l) => l.uri.toString()))].filter((u) => !monaco.editor.getModel(monaco.Uri.parse(u))).slice(0, 50);
  await Promise.all(missing.map(async (u) => {
    const path = pathOf(monaco.Uri.parse(u));
    try {
      ensureModel(headUri(path), await c.read(path), path);
    } catch {
      // Leave it out of peek; opening it still works.
    }
  }));
  return out;
}

let providers = false;
function registerProviders() {
  if (providers) return;
  providers = true;
  for (const lang of LANGS) {
    monaco.languages.registerHoverProvider(lang, {
      async provideHover(m, p) {
        const c = clientFor(m);
        if (!c?.caps.hoverProvider) return null;
        const r: any = await ask(c, m, "textDocument/hover", docPos(c, m, p));
        if (!r) return null;
        const contents = markdown(r.contents);
        return contents.length ? { contents, range: r.range ? range(r.range) : undefined } : null;
      },
    });
    monaco.languages.registerDefinitionProvider(lang, {
      async provideDefinition(m, p) {
        const c = clientFor(m);
        if (!c?.caps.definitionProvider) return null;
        return locations(c, await ask(c, m, "textDocument/definition", docPos(c, m, p)));
      },
    });
    monaco.languages.registerTypeDefinitionProvider(lang, {
      async provideTypeDefinition(m, p) {
        const c = clientFor(m);
        if (!c?.caps.typeDefinitionProvider) return null;
        return locations(c, await ask(c, m, "textDocument/typeDefinition", docPos(c, m, p)));
      },
    });
    monaco.languages.registerImplementationProvider(lang, {
      async provideImplementation(m, p) {
        const c = clientFor(m);
        if (!c?.caps.implementationProvider) return null;
        return locations(c, await ask(c, m, "textDocument/implementation", docPos(c, m, p)));
      },
    });
    monaco.languages.registerReferenceProvider(lang, {
      async provideReferences(m, p, ctx) {
        const c = clientFor(m);
        if (!c?.caps.referencesProvider) return null;
        return locations(c, await ask(c, m, "textDocument/references", { ...docPos(c, m, p), context: { includeDeclaration: ctx.includeDeclaration } }));
      },
    });
    monaco.languages.registerDocumentHighlightProvider(lang, {
      async provideDocumentHighlights(m, p) {
        const c = clientFor(m);
        if (!c?.caps.documentHighlightProvider) return null;
        const r = await ask<any[]>(c, m, "textDocument/documentHighlight", docPos(c, m, p));
        return (r ?? []).map((h) => ({ range: range(h.range), kind: h.kind ? h.kind - 1 : 0 }));
      },
    });
  }
}

/**
 * Semantic tokens (types, parameters, readonly variables), colored by the
 * theme. Registered per client: Monaco keeps a provider's legend for good,
 * and each server has its own.
 */
function registerSemanticTokens(c: Client) {
  const sp = c.caps.semanticTokensProvider;
  if (!sp?.legend || !sp.full) return;
  const legend = { tokenTypes: sp.legend.tokenTypes ?? [], tokenModifiers: sp.legend.tokenModifiers ?? [] };
  for (const lang of c.server.languages) {
    c.disposables.push(monaco.languages.registerDocumentSemanticTokensProvider(lang, {
      getLegend: () => legend,
      async provideDocumentSemanticTokens(m) {
        if (clientFor(m) !== c) return null;
        const r = await ask<{ data: number[]; resultId?: string }>(c, m, "textDocument/semanticTokens/full", { textDocument: { uri: toLsp(c, m.uri) } });
        return r?.data ? { data: new Uint32Array(r.data), resultId: r.resultId } : null;
      },
      releaseDocumentSemanticTokens() {},
    }));
  }
}

function drop(c: Client) {
  bySession.delete(c.session);
  for (const [l, x] of byLang) if (x === c) byLang.delete(l);
  c.disposables.forEach((d) => d.dispose());
  c.disposables = [];
  if (c.server.languages.some((l) => TS_LANGS.has(l))) setBuiltinTs(true);
  for (const m of monaco.editor.getModels()) monaco.editor.setModelMarkers(m, c.server.id, []);
  for (const k of [...diags.keys()]) if (k.startsWith(`${c.session} `)) diags.delete(k);
}

export async function refreshServers() {
  try {
    const servers = await api.lspServers();
    set((s) => ({ ...s, servers }));
    return servers;
  } catch {
    set((s) => ({ ...s, servers: s.servers ?? [] }));
    return [];
  }
}

export async function installServer(id: string) {
  set((s) => ({ ...s, installing: { ...s.installing, [id]: true }, installErrors: { ...s.installErrors, [id]: "" } }));
  try {
    const st = await api.lspInstall(id);
    set((s) => ({ ...s, servers: (s.servers ?? []).map((x) => (x.id === id ? st : x)) }));
    current?.();
  } catch (e) {
    set((s) => ({ ...s, installErrors: { ...s.installErrors, [id]: String(e) } }));
  } finally {
    set((s) => ({ ...s, installing: { ...s.installing, [id]: false } }));
  }
}

/** Restarts servers for the open checkout; set while one is open. */
let current: (() => void) | null = null;

/** Ecosystems whose dependencies a language needs. */
const ECOSYSTEM: Record<string, string> = { typescript: "js", javascript: "js" };

/**
 * Runs the installed servers this checkout needs while it's open, after
 * installing (or linking) its shared dependencies. `files` are the
 * checkout's paths; `read` reads a head-side file.
 */
export function useLanguageServers(target: { repo: string; number: number; key: string; files: string[]; local: boolean } | null, read: (path: string) => Promise<string>) {
  const key = target ? `${target.key}|${target.local}` : "";
  useEffect(() => {
    if (!target) return;
    const wanted = LANGS.filter((l) => target.files.some((f) => languageFor(f) === l));
    set((s) => ({ ...s, wanted, running: {}, deps: {} }));
    let live = true;
    const mine: Client[] = [];
    const deps = new Map<string, Promise<DepsRun>>();

    /** Installs an ecosystem's dependencies once per open checkout. */
    const prepare = (eco: string) => {
      let p = deps.get(eco);
      if (!p) {
        set((s) => ({ ...s, deps: { ...s.deps, [eco]: { state: "installing" } } }));
        p = api.depsPrepare(target.repo, target.number, eco).then(
          (r): DepsRun => r,
          (e): DepsRun => ({ state: "error", lockfile: null, manager: null, key: null, reused: false, message: String(e) }),
        );
        void p.then((r) => live && set((s) => ({ ...s, deps: { ...s.deps, [eco]: r } })));
        deps.set(eco, p);
      }
      return p;
    };

    const start = async () => {
      const servers = await refreshServers();
      if (!live || !target.local) return;
      const ecos = [...new Set(wanted.map((l) => ECOSYSTEM[l]).filter(Boolean))];
      for (const eco of ecos) {
        const server = servers.find((sv) => sv.deps === eco);
        // No server for it: give the built-in service the libraries' types.
        if (!server?.installed && eco === "js") {
          void prepare(eco).then(async (r) => {
            if (!live || r.state !== "ready" || byLang.has("typescript")) return;
            const files = await api.depsTypes(target.repo, target.number).catch(() => [] as [string, string][]);
            if (live && !byLang.has("typescript")) setLibraryTypes(files);
          });
        }
      }
      for (const sv of servers) {
        if (!sv.installed || !sv.languages.some((l) => wanted.includes(l))) continue;
        if ([...bySession.values()].some((c) => c.server.id === sv.id) || state.running[sv.id]?.state === "starting") continue;
        set((s) => ({ ...s, running: { ...s.running, [sv.id]: { state: "starting" } } }));
        void (async () => {
          // Types need the dependencies on disk before the server indexes.
          if (sv.deps) await prepare(sv.deps);
          if (!live) return;
          try {
            const r = await api.lspStart(sv.id, target.repo, target.number);
            if (!live) return void api.lspStop(r.session).catch(() => {});
            syncModels();
            registerProviders();
            const c: Client = { session: r.session, server: sv, root: r.root_uri.replace(/\/$/, ""), caps: r.capabilities ?? {}, opened: new Map(), versions: new Map(), read, disposables: [] };
            mine.push(c);
            bySession.set(c.session, c);
            for (const l of sv.languages) byLang.set(l, c);
            if (sv.languages.some((l) => TS_LANGS.has(l))) {
              setBuiltinTs(false);
              setLibraryTypes([]);
            }
            registerSemanticTokens(c);
            for (const m of shownModels()) if (clientFor(m) === c) void open(c, m);
            set((s) => ({ ...s, running: { ...s.running, [sv.id]: { state: "ready" } } }));
          } catch (e) {
            if (!live) return;
            set((s) => ({ ...s, running: { ...s.running, [sv.id]: { state: "error", error: String(e) } } }));
            // The built-in service still gets the libraries' types.
            if (sv.deps === "js" && !byLang.has("typescript")) {
              const files = await api.depsTypes(target.repo, target.number).catch(() => [] as [string, string][]);
              if (live) setLibraryTypes(files);
            }
          }
        })();
      }
    };
    current = () => void start();
    void start();
    return () => {
      live = false;
      current = null;
      for (const c of mine) {
        drop(c);
        void api.lspStop(c.session).catch(() => {});
      }
      setLibraryTypes([]);
      set((s) => ({ ...s, wanted: [], running: {}, deps: {} }));
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
}

export const LANGUAGE_NAMES: Record<string, string> = { typescript: "TypeScript", javascript: "JavaScript", go: "Go", sql: "SQL", rust: "Rust", python: "Python" };
