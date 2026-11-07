import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ElementType, type HTMLAttributes } from "react";

/**
 * A title that changes like a split-flap board: each letter flips in place
 * from the old character through a few random glyphs to the new one, left
 * to right. Letters that are the same in both titles stay still. Use it
 * anywhere a title can be renamed under the person (the titler, a rename
 * elsewhere), so every place showing the title changes together.
 *
 * The first frame is exactly what was on screen: the visible part of the old
 * title (with its "…" if it was cut off), each letter at its real width, and
 * the old title's line breaks. Each position's width then eases to the new
 * letter's, a line break moves to the new title's once its letter flips,
 * and the height eases to the new title's, so nothing around it jumps.
 *
 * It only animates a change to the same thing: pass `id` and a new id shows
 * its text at once. The first render, reduced motion and `still` show the
 * text plainly. When it finishes the element holds plain text again, so
 * ellipsis, line clamps and selection work as before.
 */
export function FlapTitle({
  text,
  id,
  as: Tag = "span",
  still,
  ...rest
}: { text: string; id?: string; as?: ElementType; still?: boolean } & HTMLAttributes<HTMLElement>) {
  const [anim, setAnim] = useState<Anim | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const shown = useRef({ text, id });
  const el = useRef<HTMLElement>(null);
  /** Read during render, before React swaps the text in. */
  const before = useRef<Layout | null>(null);
  if (el.current && text !== shown.current.text && !anim) before.current = layoutOf(el.current);

  useLayoutEffect(() => {
    const prev = shown.current;
    shown.current = { text, id };
    if (prev.text === text) return;
    // A change mid-flip lands at once; the next one animates from there.
    const from = anim ? null : before.current;
    before.current = null;
    const node = el.current;
    if (prev.id !== id || still || reducedMotion() || !from || !node) return clearHeight(node), setAnim(null);
    const to = layoutOf(probe(node, text));
    const a = plan(from, to, text);
    // Hold the old height; grow with the new letters, or shrink once the old line is gone.
    clearHeight(node);
    node.style.height = `${from.height}px`;
    if (Math.abs(to.height - from.height) >= 1) {
      void node.offsetHeight;
      const ms = 320;
      // Growing makes room as letters arrive; shrinking waits until the old line is empty.
      const delay = to.height > from.height ? 0 : Math.max(0, a.totalMs - 120);
      a.endMs = a.totalMs + (to.height > from.height ? 0 : ms - 120);
      node.style.transition = `height ${ms}ms cubic-bezier(0.2, 0.9, 0.25, 1) ${delay}ms`;
      node.style.height = `${to.height}px`;
    }
    setElapsed(0);
    setAnim(a);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [text, id, still]);

  useEffect(() => {
    if (!anim) return;
    let raf = 0;
    let start = 0;
    // Two frames so the old letters paint in place before they start to flip.
    raf = requestAnimationFrame(() => (raf = requestAnimationFrame((t) => {
      start = t;
      setAnim((a) => (a?.key === anim.key ? { ...a, go: true } : a));
      const tick = (now: number) => {
        const e = now - start;
        // Line breaks follow each letter as it flips; one render per frame.
        setElapsed(e);
        if (e < anim.endMs) raf = requestAnimationFrame(tick);
        else {
          clearHeight(el.current);
          setAnim((a) => (a?.key === anim.key ? null : a));
        }
      };
      raf = requestAnimationFrame(tick);
    })));
    return () => cancelAnimationFrame(raf);
    // Only when a new animation starts.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [anim?.key]);

  useEffect(() => () => clearHeight(el.current), []);

  return (
    <Tag {...rest} ref={el} aria-label={anim ? text : rest["aria-label"]}>
      {!anim ? (
        text
      ) : (
        // Every width eases on one shared clock, so each letter slides once,
        // straight from its old place to its new one. Per-letter timing made
        // the rest of the line shuffle back and forth as each letter resized.
        <span className={anim.go ? "flap flap-go" : "flap"} style={{ "--flap-width-ms": `${anim.totalMs}ms` } as CSSProperties} aria-hidden>
          {(() => {
            const breaks = breaksAt(anim, anim.go ? elapsed : -1);
            return anim.slots.map((s) => (
              <span key={s.i}>
                <span className="flap-slot" style={{ width: anim.go ? s.toW : s.fromW }}>
                  <span className="flap-reel" style={{ "--k": s.reel.length - 1, transitionDuration: `${s.ms}ms`, transitionDelay: `${s.delay}ms` } as CSSProperties}>
                    {s.reel.map((g, i) => <span key={i}>{g}</span>)}
                  </span>
                </span>
                {breaks.has(s.i) && <br />}
              </span>
            ));
          })()}
        </span>
      )}
    </Tag>
  );
}

type Slot = { i: number; from: string; to: string; reel: string[]; fromW: number; toW: number; ms: number; delay: number };
/**
 * Where line `n` ends: after `old` (the old title's break) until `at` ms in,
 * then after `neu` (the new title's). Either may be missing when the titles
 * have different line counts.
 */
type LineBreak = { old?: number; neu?: number; at: number };
type Anim = { key: number; go: boolean; slots: Slot[]; breaks: LineBreak[]; totalMs: number; endMs: number };
/** What a title looks like on screen: its visible letters (with "…" when cut off) and their widths. */
type Layout = { chars: string[]; widths: number[]; height: number; lineEnds: Set<number> };

const GLYPHS = "ABCDEFGHJKLMNPRSTUVWXYZabcdefghkmnpqrstuvwxyz0123456789#%&*+=/";
const MAX_STAGGER_MS = 800;
let seq = 0;

function plan(from: Layout, to: Layout, next: string): Anim {
  const n = Array.from(next);
  const len = Math.max(from.chars.length, n.length);
  // Long titles still finish in about the same time.
  const stagger = Math.min(26, MAX_STAGGER_MS / Math.max(len, 1));
  const slots: Slot[] = [];
  let totalMs = 0;
  for (let i = 0; i < len; i++) {
    const a = from.chars[i] ?? "";
    const b = n[i] ?? "";
    const flips = a === b ? 0 : 2 + (i % 3);
    const reel = flips === 0 ? [b] : [a, ...Array.from({ length: flips }, () => GLYPHS[(Math.random() * GLYPHS.length) | 0]), b];
    const ms = flips === 0 ? 0 : 260 + flips * 90;
    const delay = Math.round(i * stagger);
    totalMs = Math.max(totalMs, delay + ms);
    // Letters past the new title's visible end (it may be cut off too) shrink away.
    const toW = b ? (to.widths[i] ?? 0) : 0;
    slots.push({ i, from: a, to: b, reel, fromW: from.widths[i] ?? 0, toW, ms, delay });
  }
  // The browser may wrap between any two letter boxes, so the flaps don't wrap
  // at all: each line break is placed by hand. A line keeps the old title's
  // break until the letters around both breaks have flipped, then takes the
  // new title's; a line only the old title had keeps its break while its
  // letters shrink away, and one only the new title has appears as it flips.
  const flipAt = (i: number) => (slots[i] ? slots[i].delay + slots[i].ms / 2 : 0);
  const olds = [...from.lineEnds].sort((x, y) => x - y);
  const neus = [...to.lineEnds].filter((i) => i < n.length).sort((x, y) => x - y);
  const breaks: LineBreak[] = [];
  for (let l = 0; l < Math.max(olds.length, neus.length); l++) {
    const old = olds[l];
    const neu = neus[l];
    breaks.push({ old, neu, at: neu === undefined ? Infinity : flipAt(Math.max(neu, old ?? neu)) });
  }
  return { key: ++seq, go: false, slots, breaks, totalMs, endMs: totalMs };
}

function breaksAt(anim: Anim, elapsed: number): Set<number> {
  const out = new Set<number>();
  for (const b of anim.breaks) {
    const at = elapsed >= b.at ? b.neu : b.old;
    if (at !== undefined) out.add(at);
  }
  return out;
}

/**
 * The letters of the element's plain text that are actually visible, with
 * their rendered widths (kerning included). When the text is cut off by
 * `text-overflow` or a line clamp, the hidden letters are dropped and a "…"
 * stands where the browser drew one.
 */
function layoutOf(node: HTMLElement): Layout {
  const box = node.getBoundingClientRect();
  const t = node.firstChild;
  const all = Array.from(node.textContent ?? "");
  if (!t || t.nodeType !== Node.TEXT_NODE || t !== node.lastChild) return { chars: all, widths: [], height: box.height, lineEnds: new Set() };
  const range = document.createRange();
  const rects: DOMRect[] = [];
  let at = 0;
  for (const c of all) {
    range.setStart(t, at);
    range.setEnd(t, at + c.length);
    rects.push(range.getBoundingClientRect());
    at += c.length;
  }
  // A space hanging off the end of a line doesn't make the text cut off.
  const fits = (i: number) => rects[i].bottom <= box.bottom + 0.5 && (all[i] === " " || rects[i].right <= box.right + 0.5);
  const lineEnds = new Set<number>();
  for (let i = 0; i + 1 < rects.length; i++) if (rects[i + 1].top > rects[i].top + 1) lineEnds.add(i);
  let visible = 0;
  while (visible < rects.length && fits(visible)) visible++;
  if (visible === rects.length) return { chars: all, widths: rects.map((r) => r.width), height: box.height, lineEnds };
  // Cut off: the browser keeps as many letters as leave room for its "…".
  const ellipsis = ellipsisWidth(node);
  while (visible > 0 && rects[visible - 1].right + ellipsis > box.right + 0.5) visible--;
  while (visible > 0 && all[visible - 1] === " ") visible--;
  // The "…" sits at index `visible`; breaks past it are in hidden text.
  for (const i of lineEnds) if (i >= visible) lineEnds.delete(i);
  return {
    chars: [...all.slice(0, visible), "…"],
    widths: [...rects.slice(0, visible).map((r) => r.width), ellipsis],
    height: box.height,
    lineEnds,
  };
}

/** An invisible copy of the element holding `text`, laid out at the same width. */
function probe(node: HTMLElement, text: string) {
  const clone = node.cloneNode(false) as HTMLElement;
  clone.removeAttribute("id");
  clone.style.cssText = `position:absolute;visibility:hidden;pointer-events:none;height:auto;transition:none;width:${node.getBoundingClientRect().width}px`;
  clone.textContent = text;
  node.parentElement!.appendChild(clone);
  // Read now, then remove: layoutOf only needs it while measuring.
  queueMicrotask(() => clone.remove());
  return clone;
}

function ellipsisWidth(node: HTMLElement) {
  const span = document.createElement("span");
  span.textContent = "…";
  span.style.cssText = "position:absolute;visibility:hidden;white-space:pre";
  node.appendChild(span);
  const w = span.getBoundingClientRect().width;
  span.remove();
  return w;
}

function clearHeight(node: HTMLElement | null) {
  if (node) (node.style.transition = ""), (node.style.height = "");
}

function reducedMotion() {
  return typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
}
