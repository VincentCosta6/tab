import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { Icon } from "../../design/icons";

/**
 * Files dragged anywhere over the window go to a chat composer: the one under
 * the cursor, else the visible one nearest to it. Composers register here; one
 * set of window listeners does the routing and drives the app-wide overlay.
 */
type Target = { el: HTMLElement; add: (files: File[]) => void };

const targets = new Set<Target>();
type State = { active: boolean; target: Target | null; rect: DOMRect | null };
let state: State = { active: false, target: null, rect: null };
const subs = new Set<() => void>();
const set = (next: State) => {
  if (next.active === state.active && next.target === state.target && sameRect(next.rect, state.rect)) return;
  state = next;
  subs.forEach((f) => f());
};
const sameRect = (a: DOMRect | null, b: DOMRect | null) => a === b || (!!a && !!b && a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height);

const visible = (el: HTMLElement) => {
  const r = el.getBoundingClientRect();
  if (!r.width || !r.height || r.bottom <= 0 || r.right <= 0 || r.top >= innerHeight || r.left >= innerWidth) return false;
  return el.checkVisibility ? el.checkVisibility({ visibilityProperty: true, opacityProperty: true } as CheckVisibilityOptions) : true;
};

const distance = (r: DOMRect, x: number, y: number) => Math.hypot(Math.max(r.left - x, 0, x - r.right), Math.max(r.top - y, 0, y - r.bottom));

function nearest(x: number, y: number): Target | null {
  let best: Target | null = null;
  let d = Infinity;
  for (const t of targets) {
    if (!visible(t.el)) continue;
    const dt = distance(t.el.getBoundingClientRect(), x, y);
    if (dt < d) (best = t), (d = dt);
  }
  return best;
}

const hasFile = (e: DragEvent) => !!e.dataTransfer && [...e.dataTransfer.items].some((i) => i.kind === "file");

let listening = false;
function listen() {
  if (listening) return;
  listening = true;
  let depth = 0;
  const reset = () => ((depth = 0), set({ active: false, target: null, rect: null }));
  const aim = (e: DragEvent) => {
    const target = nearest(e.clientX, e.clientY);
    set({ active: true, target, rect: target?.el.getBoundingClientRect() ?? null });
    return target;
  };
  window.addEventListener("dragenter", (e) => hasFile(e) && (depth++, aim(e)));
  window.addEventListener("dragleave", (e) => hasFile(e) && --depth <= 0 && reset());
  window.addEventListener("dragover", (e) => {
    if (!hasFile(e)) return;
    // Always claim the drag, so a drop never navigates the webview to the file.
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = aim(e) ? "copy" : "none";
  });
  window.addEventListener("drop", (e) => {
    const files = [...(e.dataTransfer?.files ?? [])];
    if (files.length) {
      e.preventDefault();
      nearest(e.clientX, e.clientY)?.add(files);
    }
    reset();
  });
  window.addEventListener("dragend", reset);
}

/** Make a composer a drop target: attach the returned ref; `add` receives dropped files. */
export function useDropTarget(add: (files: File[]) => void) {
  const latest = useRef(add);
  latest.current = add;
  const [el, setEl] = useState<HTMLElement | null>(null);
  useEffect(() => {
    if (!el) return;
    listen();
    const t: Target = { el, add: (f) => latest.current(f) };
    targets.add(t);
    return () => void targets.delete(t);
  }, [el]);
  const s = useSyncExternalStore(subscribe, snapshot);
  return { ref: setEl, armed: s.active, targeted: s.active && !!el && s.target?.el === el };
}

const subscribe = (f: () => void) => (subs.add(f), () => void subs.delete(f));
const snapshot = () => state;

/** Dims the whole app while a file is dragged and spotlights the chat it will land in. */
export function DropOverlay() {
  const s = useSyncExternalStore(subscribe, snapshot);
  if (!s.active) return null;
  const r = s.rect;
  const pad = 6;
  return (
    <div className="drop-overlay" aria-hidden>
      {r ? (
        <>
          <div className="drop-spot" style={{ left: r.left - pad, top: r.top - pad, width: r.width + pad * 2, height: r.height + pad * 2 }} />
          <div className="drop-pill" style={{ left: r.left + r.width / 2, top: Math.max(12, r.top - pad - 40) }}>
            <Icon name="plus" size={14} /> Drop to attach to this chat
          </div>
        </>
      ) : (
        <div className="drop-pill drop-pill-center"><Icon name="x" size={14} /> Open a chat to drop files into it</div>
      )}
    </div>
  );
}
