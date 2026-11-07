import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ElementType, type HTMLAttributes } from "react";

/**
 * A title that plays a split-flap animation when its text changes: the old
 * letters flip up and away, then each new letter rolls in through a few
 * random glyphs, like a departures board. Use it anywhere a title can be
 * renamed under the person (the titler, a rename elsewhere), so every place
 * that shows the title changes together.
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
  const shown = useRef({ text, id });
  const el = useRef<HTMLElement>(null);
  /** The element's height before the new text went in, to ease between the two. */
  const fromHeight = useRef<number | null>(null);
  const settle = useRef<ReturnType<typeof setTimeout>>(undefined);
  // Read before React swaps the text in; by any effect it's already the new height.
  if (el.current && text !== shown.current.text) fromHeight.current = el.current.getBoundingClientRect().height;

  useLayoutEffect(() => {
    const prev = shown.current;
    shown.current = { text, id };
    if (prev.text === text) return;
    if (prev.id !== id || still || reducedMotion()) return (fromHeight.current = null), setAnim(null);
    setAnim(plan(anim?.next ?? prev.text, text));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [text, id, still]);

  useEffect(() => {
    if (!anim) return;
    // Two frames so the starting positions paint before transitions run.
    let raf = requestAnimationFrame(() => (raf = requestAnimationFrame(() => setAnim((a) => (a?.key === anim.key && a.phase === "start" ? { ...a, phase: "out" } : a)))));
    const t1 = setTimeout(() => setAnim((a) => (a?.key === anim.key ? { ...a, phase: "roll" } : a)), anim.outMs);
    const t2 = setTimeout(() => setAnim((a) => (a?.key === anim.key ? { ...a, phase: "rolling" } : a)), anim.outMs + 40);
    const t3 = setTimeout(() => setAnim((a) => (a?.key === anim.key ? null : a)), anim.outMs + 40 + anim.rollMs);
    return () => (cancelAnimationFrame(raf), clearTimeout(t1), clearTimeout(t2), clearTimeout(t3));
    // Only when a new animation starts, not on its own phase changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [anim?.key]);

  // When the new title takes more or fewer lines (a wrapping header), ease
  // the height instead of jumping what's below it.
  useLayoutEffect(() => {
    const node = el.current;
    const from = fromHeight.current;
    if (anim?.phase !== "roll" || !node || from == null) return;
    fromHeight.current = null;
    clearTimeout(settle.current);
    node.style.transition = "";
    node.style.height = "";
    const to = node.getBoundingClientRect().height;
    if (Math.abs(to - from) < 1) return;
    node.style.height = `${from}px`;
    void node.offsetHeight;
    node.style.transition = "height 360ms cubic-bezier(0.2, 0.9, 0.25, 1)";
    node.style.height = `${to}px`;
    settle.current = setTimeout(() => ((node.style.transition = ""), (node.style.height = "")), 400);
  }, [anim?.phase]);

  useEffect(() => () => clearTimeout(settle.current), []);

  return (
    <Tag {...rest} ref={el} aria-label={anim ? text : rest["aria-label"]}>
      {!anim ? text : anim.phase === "start" || anim.phase === "out" ? (
        <span className={anim.phase === "out" ? "flap flap-go" : "flap"} aria-hidden>
          {words(anim.prev).map((w, wi) =>
            w === " " ? " " : (
              <span key={wi} className="flap-word">
                {w.chars.map((c, i) => (
                  <span key={i} className="flap-out" style={{ transitionDelay: `${(w.start + i) * anim.outStagger}ms` }}>{c}</span>
                ))}
              </span>
            ),
          )}
        </span>
      ) : (
        <span className={anim.phase === "rolling" ? "flap flap-go" : "flap"} aria-hidden>
          {words(anim.next).map((w, wi) =>
            w === " " ? " " : (
              <span key={wi} className="flap-word">
                {w.chars.map((c, i) => {
                  const reel = anim.reels[w.start + i];
                  return (
                    <span key={i} className="flap-slot">
                      <span className="flap-size">{c}</span>
                      <span
                        className="flap-reel"
                        style={{ "--k": reel.length - 1, transitionDuration: `${380 + reel.length * 70}ms`, transitionDelay: `${(w.start + i) * anim.rollStagger}ms` } as CSSProperties}
                      >
                        {reel.map((g, gi) => <span key={gi}>{g}</span>)}
                      </span>
                    </span>
                  );
                })}
              </span>
            ),
          )}
        </span>
      )}
    </Tag>
  );
}

type Anim = {
  key: number;
  prev: string;
  next: string;
  phase: "start" | "out" | "roll" | "rolling";
  /** Per letter of `next`: random glyphs, then the letter itself. */
  reels: string[][];
  outStagger: number;
  rollStagger: number;
  outMs: number;
  rollMs: number;
};

const GLYPHS = "ABCDEFGHJKLMNPRSTUVWXYZabcdefghkmnpqrstuvwxyz0123456789#%&*+=/";
const MAX_STAGGER_MS = 900;
let seq = 0;

function plan(prev: string, next: string): Anim {
  const p = Array.from(prev);
  const n = Array.from(next);
  // Long titles still finish in about the same time.
  const outStagger = Math.min(6, 240 / Math.max(p.length, 1));
  const rollStagger = Math.min(28, MAX_STAGGER_MS / Math.max(n.length, 1));
  const reels = n.map((c, i) => (c === " " ? [" "] : [...Array.from({ length: 3 + (i % 3) }, () => GLYPHS[(Math.random() * GLYPHS.length) | 0]), c]));
  const longest = Math.max(1, ...reels.map((r) => r.length));
  return {
    key: ++seq,
    prev,
    next,
    phase: "start",
    reels,
    outStagger,
    rollStagger,
    outMs: 260 + Math.min(p.length, 60) * outStagger,
    rollMs: n.length * rollStagger + 380 + longest * 70 + 60,
  };
}

/** Words keep their letters together so a wrapping title only breaks at spaces. */
function words(s: string): ({ chars: string[]; start: number } | " ")[] {
  const out: ({ chars: string[]; start: number } | " ")[] = [];
  let i = 0;
  for (const part of s.split(/( +)/)) {
    if (!part) continue;
    const chars = Array.from(part);
    if (part.trim() === "") for (const _ of chars) out.push(" ");
    else out.push({ chars, start: i });
    i += chars.length;
  }
  return out;
}

function reducedMotion() {
  return typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
}
