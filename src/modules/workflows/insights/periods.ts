// The slots a daily chart draws for a window: one bar a day up to 30 days
// (the person's days), one bar a week for 90 days and a year (UTC days, as
// the long windows and their daily rollups count them), so a year reads as
// 53 bars instead of 365 slivers.

import { dayLabel, windowDays } from "./format";

const DAY = 86_400_000;

export interface Slots {
  /** Each slot's key: its first day, `YYYY-MM-DD`. */
  keys: string[];
  /** The slot a day falls in, if it's in the window. */
  of: (day: string) => string | undefined;
  /** "day" or "week". */
  unit: "day" | "week";
  /** Axis and tooltip text for a slot. */
  label: (key: string, long?: boolean) => string;
}

/** A window's name: "7 days", "90 days", "year". */
export const windowName = (days: number) => (days >= 365 ? "year" : `${days} days`);

export function slotsFor(from: string, days: number): Slots {
  if (days <= 30) {
    const keys = windowDays(from, days);
    const set = new Set(keys);
    return { keys, of: (d) => (set.has(d) ? d : undefined), unit: "day", label: dayLabel };
  }
  // UTC days, newest last; weeks counted back from today so the last bar is a whole week.
  const start = Date.parse(from);
  const utc = Array.from({ length: days }, (_, i) => new Date(start + i * DAY).toISOString().slice(0, 10));
  const first = days % 7;
  const keyOf = new Map<string, string>();
  const keys: string[] = [];
  const size = new Map<string, number>();
  utc.forEach((d, i) => {
    const k = i < first ? utc[0] : utc[first + Math.floor((i - first) / 7) * 7];
    if (keys[keys.length - 1] !== k) keys.push(k);
    keyOf.set(d, k);
    size.set(k, (size.get(k) ?? 0) + 1);
  });
  const last = (k: string) => utc[utc.indexOf(k) + (size.get(k) ?? 1) - 1];
  return {
    keys,
    of: (d) => keyOf.get(d),
    unit: "week",
    label: (k, long) => {
      if (!long) return dayLabel(k);
      const n = size.get(k) ?? 7;
      return n === 1 ? `${dayLabel(k)} (1 day, UTC)` : `${dayLabel(k)} – ${dayLabel(last(k))} (UTC)`;
    },
  };
}

/** Adds each day's values into its slot. */
export function bucket(slots: Slots, values: Record<string, number[]>): Record<string, number[]> {
  if (slots.unit === "day") return values;
  const out: Record<string, number[]> = {};
  for (const [day, vs] of Object.entries(values)) {
    const k = slots.of(day);
    if (!k) continue;
    const acc = (out[k] ??= vs.map(() => 0));
    vs.forEach((v, i) => (acc[i] = (acc[i] ?? 0) + v));
  }
  return out;
}
