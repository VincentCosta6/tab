// Language servers for languages Monaco has no service for (Go, SQL, Rust,
// Python). tab runs the server in the PR's checkout and this file is the
// client: it opens every head-side model with the server, turns hover,
// go to definition and references into Monaco providers, and shows the
// server's diagnostics as markers. Models live at file:///<path>; the server
// sees <worktree>/<path>.

import { useEffect } from "react";
import { api, onLsp } from "../../lib/api";
import type { LspServer } from "../../lib/types";
import { ensureModel, headUri, languageFor, monaco, pathOf } from "./monaco";

export type ServerRun = { state: "starting" | "ready" | "error" | "stopped"; error?: string };
export interface LspState {
  /** Every server tab knows, with install state. Null until loaded. */
  servers: LspServer[] | null;
  /** Languages in the open checkout that a server covers. */
  wanted: string[];
  running: Record<string, ServerRun>;
  installing: Record<string, boolean>;
  installErrors: Record<string, string>;
}

let state: LspState = { servers: null, wanted: [], running: {}, installing: {}, installErrors: {} };
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
  versions: Map<string, number>;
  read: (path: string) => Promise<string>;
}

/** Running clients by language id. One checkout is open at a time. */
const byLang = new Map<string, Client>();
const bySession = new Map<string, Client>();
/** Diagnostics that arrived before their file was opened here. */
const diags = new Map<string, any[]>();

const LANGS = ["go", "sql", "rust", "python"];

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

function open(c: Client, m: monaco.editor.ITextModel) {
  const uri = toLsp(c, m.uri);
  if (c.versions.has(uri)) return;
  c.versions.set(uri, 1);
  void api.lspNotify(c.session, "textDocument/didOpen", { textDocument: { uri, languageId: m.getLanguageId(), version: 1, text: m.getValue() } }).catch(() => {});
  const pending = diags.get(`${c.session} ${uri}`);
  if (pending) setMarkers(c, m, pending);
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

// Keep the server's view of open files in step with the models.
let synced = false;
function syncModels() {
  if (synced) return;
  synced = true;
  monaco.editor.onDidCreateModel((m) => {
    const c = clientFor(m);
    if (c) open(c, m);
    m.onDidChangeContent(() => {
      const c = clientFor(m);
      if (!c) return;
      const uri = toLsp(c, m.uri);
      const v = (c.versions.get(uri) ?? 1) + 1;
      c.versions.set(uri, v);
      void api.lspNotify(c.session, "textDocument/didChange", { textDocument: { uri, version: v }, contentChanges: [{ text: m.getValue() }] }).catch(() => {});
    });
  });
  monaco.editor.onWillDisposeModel((m) => {
    const c = clientFor(m);
    if (!c) return;
    const uri = toLsp(c, m.uri);
    if (c.versions.delete(uri)) void api.lspNotify(c.session, "textDocument/didClose", { textDocument: { uri } }).catch(() => {});
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
        const r: any = await api.lspRequest(c.session, "textDocument/hover", docPos(c, m, p)).catch(() => null);
        if (!r) return null;
        const contents = markdown(r.contents);
        return contents.length ? { contents, range: r.range ? range(r.range) : undefined } : null;
      },
    });
    monaco.languages.registerDefinitionProvider(lang, {
      async provideDefinition(m, p) {
        const c = clientFor(m);
        if (!c?.caps.definitionProvider) return null;
        return locations(c, await api.lspRequest(c.session, "textDocument/definition", docPos(c, m, p)).catch(() => null));
      },
    });
    monaco.languages.registerTypeDefinitionProvider(lang, {
      async provideTypeDefinition(m, p) {
        const c = clientFor(m);
        if (!c?.caps.typeDefinitionProvider) return null;
        return locations(c, await api.lspRequest(c.session, "textDocument/typeDefinition", docPos(c, m, p)).catch(() => null));
      },
    });
    monaco.languages.registerImplementationProvider(lang, {
      async provideImplementation(m, p) {
        const c = clientFor(m);
        if (!c?.caps.implementationProvider) return null;
        return locations(c, await api.lspRequest(c.session, "textDocument/implementation", docPos(c, m, p)).catch(() => null));
      },
    });
    monaco.languages.registerReferenceProvider(lang, {
      async provideReferences(m, p, ctx) {
        const c = clientFor(m);
        if (!c?.caps.referencesProvider) return null;
        return locations(c, await api.lspRequest(c.session, "textDocument/references", { ...docPos(c, m, p), context: { includeDeclaration: ctx.includeDeclaration } }).catch(() => null));
      },
    });
    monaco.languages.registerDocumentHighlightProvider(lang, {
      async provideDocumentHighlights(m, p) {
        const c = clientFor(m);
        if (!c?.caps.documentHighlightProvider) return null;
        const r: any[] | null = await api.lspRequest<any[]>(c.session, "textDocument/documentHighlight", docPos(c, m, p)).catch(() => null);
        return (r ?? []).map((h) => ({ range: range(h.range), kind: h.kind ? h.kind - 1 : 0 }));
      },
    });
  }
}

function drop(c: Client) {
  bySession.delete(c.session);
  for (const [l, x] of byLang) if (x === c) byLang.delete(l);
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

/**
 * Runs the installed servers this checkout needs while it's open. `files`
 * are the checkout's paths; `read` reads a head-side file.
 */
export function useLanguageServers(target: { repo: string; number: number; key: string; files: string[]; local: boolean } | null, read: (path: string) => Promise<string>) {
  const key = target ? `${target.key}|${target.local}` : "";
  useEffect(() => {
    if (!target) return;
    const wanted = LANGS.filter((l) => target.files.some((f) => languageFor(f) === l));
    set((s) => ({ ...s, wanted, running: {} }));
    let live = true;
    const mine: Client[] = [];

    const start = async () => {
      const servers = await refreshServers();
      if (!live || !target.local) return;
      for (const sv of servers) {
        if (!sv.installed || !sv.languages.some((l) => wanted.includes(l))) continue;
        if ([...bySession.values()].some((c) => c.server.id === sv.id) || state.running[sv.id]?.state === "starting") continue;
        set((s) => ({ ...s, running: { ...s.running, [sv.id]: { state: "starting" } } }));
        api.lspStart(sv.id, target.repo, target.number).then(
          (r) => {
            if (!live) return void api.lspStop(r.session).catch(() => {});
            syncModels();
            registerProviders();
            const c: Client = { session: r.session, server: sv, root: r.root_uri.replace(/\/$/, ""), caps: r.capabilities ?? {}, versions: new Map(), read };
            mine.push(c);
            bySession.set(c.session, c);
            for (const l of sv.languages) byLang.set(l, c);
            for (const m of monaco.editor.getModels()) if (clientFor(m) === c) open(c, m);
            set((s) => ({ ...s, running: { ...s.running, [sv.id]: { state: "ready" } } }));
          },
          (e) => live && set((s) => ({ ...s, running: { ...s.running, [sv.id]: { state: "error", error: String(e) } } })),
        );
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
      set((s) => ({ ...s, wanted: [], running: {} }));
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
}

export const LANGUAGE_NAMES: Record<string, string> = { go: "Go", sql: "SQL", rust: "Rust", python: "Python" };
