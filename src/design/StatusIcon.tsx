import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Badge, cx } from "./components";
import { Icon } from "./icons";
import type { Status, Tone } from "../lib/types";

const toneIcon: Record<Tone, string> = {
  success: "check-circle",
  danger: "x-circle",
  warning: "alert",
  info: "circle-dot",
  accent: "circle-dot",
  neutral: "dot",
};

/**
 * A status as one small tone-coloured icon, for tight rows. Hover or focus
 * shows the full label, plus any `details` (checks, review, who and when).
 */
export function StatusIcon({ status, details }: { status: Status; details?: ReactNode }) {
  const ref = useRef<HTMLSpanElement>(null);
  const [at, setAt] = useState<DOMRect | null>(null);
  const timer = useRef<number>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const show = (delay: number) => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => ref.current && setAt(ref.current.getBoundingClientRect()), delay);
  };
  const hide = () => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setAt(null), 120);
  };
  const width = 260;
  return (
    <>
      <span
        ref={ref}
        className={cx("status-icon", `tone-${status.tone}`)}
        aria-label={status.label}
        tabIndex={0}
        onMouseEnter={() => show(200)}
        onMouseLeave={hide}
        onFocus={() => show(0)}
        onBlur={hide}
      >
        <Icon name={status.icon ?? toneIcon[status.tone]} size={12} />
      </span>
      {at && createPortal(
        <div
          className="link-card status-card"
          role="tooltip"
          style={{
            width,
            left: Math.max(8, Math.min(at.right - width, window.innerWidth - width - 8)),
            ...(at.bottom + 180 > window.innerHeight ? { bottom: window.innerHeight - at.top + 6 } : { top: at.bottom + 6 }),
          }}
          onMouseEnter={() => window.clearTimeout(timer.current)}
          onMouseLeave={hide}
        >
          <div className="link-card-head">
            <Badge tone={status.tone} icon={status.icon ?? toneIcon[status.tone]}>{status.label}</Badge>
          </div>
          {details}
        </div>,
        document.body,
      )}
    </>
  );
}
