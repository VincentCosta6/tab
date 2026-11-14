import { useLayoutEffect, useRef, type DependencyList, type RefObject } from "react";

/** How close to the bottom counts as "at the bottom" when the person scrolls back down. */
const NEAR = 48;
const UP_KEYS = new Set(["ArrowUp", "PageUp", "Home"]);

/**
 * Keeps a scrolling box at its bottom while new output arrives, until the
 * person scrolls up to read; scrolling back down to the bottom follows again.
 *
 * Scrolling up is read from the input (wheel, touch, keys) as well as from
 * scroll events: a reply streams a token every few milliseconds, and the
 * next token used to snap the box back down before the scroll event of a
 * small wheel step arrived, so the person couldn't scroll at all.
 *
 * `rejoin` changing to a new non-null value follows the bottom again, e.g.
 * the id of the person's newest message.
 */
export function useFollowBottom(box: RefObject<HTMLElement | null>, deps: DependencyList, rejoin: unknown) {
  const stuck = useRef(true);
  const lastTop = useRef(0);
  const touchY = useRef<number | null>(null);
  const lastRejoin = useRef(rejoin);

  // A new value of `rejoin` (the person's own message) follows the bottom
  // again, once: the reply streaming in after it can still be scrolled away from.
  if (rejoin !== lastRejoin.current) {
    lastRejoin.current = rejoin;
    if (rejoin != null) stuck.current = true;
  }

  useLayoutEffect(() => {
    const el = box.current;
    if (!el || !stuck.current) return;
    el.scrollTop = el.scrollHeight;
    lastTop.current = el.scrollTop;
  }, deps);

  return {
    /** Spread these on the scrolling box. */
    handlers: {
      onWheel: (e: React.WheelEvent) => {
        if (e.deltaY < 0) stuck.current = false;
      },
      onTouchStart: (e: React.TouchEvent) => {
        touchY.current = e.touches[0]?.clientY ?? null;
      },
      onTouchMove: (e: React.TouchEvent) => {
        const y = e.touches[0]?.clientY;
        // A finger moving down drags the content down: scrolling up.
        if (y !== undefined && touchY.current !== null && y > touchY.current) stuck.current = false;
        touchY.current = y ?? null;
      },
      onKeyDown: (e: React.KeyboardEvent) => {
        if (UP_KEYS.has(e.key) || (e.key === " " && e.shiftKey)) stuck.current = false;
      },
    },
    /** Call from the box's onScroll. */
    onScroll: (el: HTMLElement) => {
      const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
      const up = el.scrollTop < lastTop.current;
      lastTop.current = el.scrollTop;
      // Content shrinking can pull scrollTop up while staying at the bottom;
      // only a move up that leaves the bottom is the person reading back.
      if (dist <= 1) stuck.current = true;
      else if (up) stuck.current = false;
      else if (dist < NEAR) stuck.current = true;
    },
  };
}
