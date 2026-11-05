import { useEffect, useRef } from "react";

function minutesUntilHour(hour: number, days: number) {
  const t = new Date();
  t.setDate(t.getDate() + days);
  t.setHours(hour, 0, 0, 0);
  return Math.max(1, Math.round((t.getTime() - Date.now()) / 60_000));
}

export const snoozeOptions = [
  { label: "1 hour", minutes: () => 60, key: "1" },
  { label: "4 hours", minutes: () => 240, key: "2" },
  { label: "Tomorrow morning", minutes: () => minutesUntilHour(9, 1), key: "3" },
  { label: "Next week", minutes: () => minutesUntilHour(9, 7), key: "4" },
];

export function SnoozeMenu({ onPick, onClose, style }: { onPick: (minutes: number) => void; onClose: () => void; style?: React.CSSProperties }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const opt = snoozeOptions.find((o) => o.key === e.key);
      if (opt) (e.preventDefault(), e.stopPropagation(), onPick(opt.minutes()));
      if (e.key === "Escape") onClose();
    };
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("mousedown", onDown);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("mousedown", onDown);
    };
  }, [onPick, onClose]);
  return (
    <div className="menu" ref={ref} style={style} role="menu">
      {snoozeOptions.map((o) => (
        <button key={o.key} role="menuitem" onClick={() => onPick(o.minutes())}>
          {o.label} <span className="kbd">{o.key}</span>
        </button>
      ))}
    </div>
  );
}
