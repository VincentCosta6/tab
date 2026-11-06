// Files an agent leaves in a chat (screenshots, recordings, HTML pages, PDFs,
// notes). Clicking one shows it in the middle of the Work tab with the chat
// kept on the right; the thread's other files are one click away.

import { useSyncExternalStore } from "react";
import { fileName, isLocalPath, openImages } from "./images";

export type FileKind = "image" | "video" | "html" | "pdf" | "markdown" | "text";

export interface ViewerFile {
  /** A local path or an http(s) address. */
  src: string;
  name: string;
  kind: FileKind;
}

const byExt: Record<string, FileKind> = {
  png: "image", jpg: "image", jpeg: "image", gif: "image", webp: "image", svg: "image",
  mp4: "video", webm: "video", mov: "video", m4v: "video", ogv: "video",
  html: "html", htm: "html",
  pdf: "pdf",
  md: "markdown", markdown: "markdown",
  txt: "text", log: "text", json: "text", csv: "text",
};

export const mimeFor = (src: string): string => {
  const e = extOf(src);
  return (
    { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp", svg: "image/svg+xml",
      mp4: "video/mp4", m4v: "video/mp4", webm: "video/webm", mov: "video/quicktime", ogv: "video/ogg", pdf: "application/pdf" } as Record<string, string>
  )[e] ?? "application/octet-stream";
};

const extOf = (src: string) => /\.([a-z0-9]+)(?:[?#].*)?$/i.exec(src)?.[1].toLowerCase() ?? "";

export function fileKindOf(src: string): FileKind | null {
  return byExt[extOf(src)] ?? null;
}

/** A path or address tab can show: a viewable extension, local or on the web. */
export function toViewerFile(src: string, name?: string): ViewerFile | null {
  const kind = fileKindOf(src);
  if (!kind) return null;
  if (!isLocalPath(src) && !/^https?:\/\//i.test(src)) return null;
  return { src, name: name || fileName(src), kind };
}

/** Every viewable file a message names: `paths`, [links](paths), ![images](paths) and bare web addresses. */
export function fileRefs(text: string): ViewerFile[] {
  const out: ViewerFile[] = [];
  const seen = new Set<string>();
  const add = (src: string, name?: string) => {
    const f = toViewerFile(src, name);
    if (f && !seen.has(f.src)) (seen.add(f.src), out.push(f));
  };
  const re = /!?\[([^\]]*)\]\(([^)\s]+)\)|`([^`\n]+)`|(https?:\/\/[^\s<>()\[\]`]+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    if (m[2]) add(m[2], m[1] || undefined);
    else if (m[3]) add(m[3]);
    else if (m[4]) add(m[4].replace(/[.,;:!?'"]+$/, ""));
  }
  return out;
}

interface State {
  /** What the thread holds, in order of appearance. */
  files: ViewerFile[];
  /** The one showing; null when the viewer is closed. */
  current: string | null;
  /** How many Work panes can show it; with none, images fall back to the overlay. */
  hosts: number;
}

let state: State = { files: [], current: null, hosts: 0 };
const subs = new Set<() => void>();
const set = (p: Partial<State>) => {
  state = { ...state, ...p };
  subs.forEach((f) => f());
};

export const viewerStore = { subscribe: (f: () => void) => (subs.add(f), () => void subs.delete(f)), get: () => state };
export const useViewer = () => useSyncExternalStore(viewerStore.subscribe, viewerStore.get);

/** The Work tab says it can show files while it is mounted. */
export function registerViewerHost() {
  set({ hosts: state.hosts + 1 });
  return () => set({ hosts: state.hosts - 1, ...(state.hosts <= 1 ? { current: null } : {}) });
}

/** The chat tells the viewer which files its thread holds. */
export function setThreadFiles(files: ViewerFile[]) {
  const same = files.length === state.files.length && files.every((f, i) => f.src === state.files[i].src);
  if (same) return;
  const keep = state.current && files.some((f) => f.src === state.current);
  set({ files, current: keep ? state.current : null });
}

export function openFile(file: ViewerFile) {
  if (!state.hosts) {
    if (file.kind === "image") return openImages([{ src: file.src, name: file.name }]);
    return void import("./api").then((m) => m.openUrl(file.src));
  }
  const files = state.files.some((f) => f.src === file.src) ? state.files : [...state.files, file];
  set({ files, current: file.src });
}

export const closeViewer = () => set({ current: null });

export function stepFile(d: number) {
  const i = state.files.findIndex((f) => f.src === state.current);
  if (i < 0 || state.files.length < 2) return;
  set({ current: state.files[(i + d + state.files.length) % state.files.length].src });
}

export const selectFile = (src: string) => set({ current: src });
