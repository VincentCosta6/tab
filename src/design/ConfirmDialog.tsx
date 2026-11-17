import { useEffect, useState, useSyncExternalStore } from "react";
import { Button } from "./components";

/**
 * In-app stand-ins for `window.confirm` and `window.alert`, which the macOS
 * webview doesn't show: there `confirm` returns false at once, unseen.
 * `ConfirmDialog` is mounted once at the root; `ask` and `tell` open it.
 */
export interface Ask {
  title: string;
  lines?: string[];
  /** The go-ahead button. Without one, only OK is offered (an alert). */
  confirm?: string;
}

let open: (Ask & { done: (ok: boolean) => void }) | null = null;
const subs = new Set<() => void>();
const set = (next: typeof open) => {
  open?.done(false);
  open = next;
  subs.forEach((f) => f());
};

/** Resolves true when the person confirms, false on Cancel, Esc or a click outside. */
export const ask = (a: Ask) => new Promise<boolean>((resolve) => set({ ...a, done: resolve }));

/** Shows a message with only OK. */
export const tell = (title: string, lines?: string[]) => ask({ title, lines }).then(() => undefined);

const answer = (ok: boolean) => {
  const current = open;
  open = null;
  current?.done(ok);
  subs.forEach((f) => f());
};

export function ConfirmDialog() {
  const a = useSyncExternalStore((f) => (subs.add(f), () => void subs.delete(f)), () => open);
  const [primed, setPrimed] = useState(false);

  // The key that opened it (Enter on a row, say) mustn't also answer it.
  useEffect(() => {
    setPrimed(false);
    if (!a) return;
    const t = setTimeout(() => setPrimed(true), 0);
    return () => clearTimeout(t);
  }, [a]);

  useEffect(() => {
    if (!a || !primed) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") answer(false);
      else if (e.key === "Enter") answer(true);
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [a, primed]);

  if (!a) return null;
  return (
    <div className="pj-backdrop confirm-backdrop" onMouseDown={(e) => e.target === e.currentTarget && answer(false)}>
      <div className="pj-dialog confirm-dialog" role="alertdialog" aria-label={a.title}>
        <p className="confirm-title">{a.title}</p>
        {!!a.lines?.length && (
          <ul className="confirm-lines">
            {a.lines.map((l, i) => <li key={i}>{l}</li>)}
          </ul>
        )}
        <footer className="pj-dialog-actions">
          {a.confirm ? (
            <>
              <Button size="sm" onClick={() => answer(false)}>Cancel</Button>
              <Button size="sm" variant="primary" autoFocus onClick={() => answer(true)}>{a.confirm}</Button>
            </>
          ) : (
            <Button size="sm" variant="primary" autoFocus onClick={() => answer(true)}>OK</Button>
          )}
        </footer>
      </div>
    </div>
  );
}
