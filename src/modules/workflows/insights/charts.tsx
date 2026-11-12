// Small hand-rolled SVG charts for workflow insights: a progress ring, daily
// stacked bars with a hover readout, a split bar, and a change marker.
// Colors come from the chart tokens in tokens.css; every chart has a label
// for screen readers, and nothing is told apart by color alone.

import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { cx } from "../../../design/components";
import type { Tone } from "../../../lib/types";
import { dayLabel } from "./format";

export const toneColor: Record<Tone, string> = {
  success: "var(--chart-ok)", warning: "var(--chart-unplanned)", danger: "var(--chart-failed)", accent: "var(--chart-attended)",
  info: "var(--chart-planned)", neutral: "var(--fg-subtle)", merged: "var(--tone-merged-fg)",
};

// ---------------------------------------------------------------------------
// Ring
// ---------------------------------------------------------------------------

export interface RingSegment { value: number; color: string; label: string }

/**
 * A circular progress ring. `segments` are shares of the whole circle, drawn
 * in order from 12 o'clock. With `empty`, only a dashed track shows: there
 * isn't enough to say, and the center says so instead of 0%.
 */
export function Ring({ segments, size = 64, stroke, center, sub, label, empty }: {
  segments: RingSegment[]; size?: number; stroke?: number; center: ReactNode; sub?: ReactNode; label: string; empty?: boolean;
}) {
  const sw = stroke ?? Math.max(4, Math.round(size / 10));
  const r = (size - sw) / 2;
  const c = 2 * Math.PI * r;
  const gap = segments.filter((s) => s.value > 0).length > 1 ? 2.5 : 0;
  let at = 0;
  return (
    <div className={cx("ins-ring", empty && "is-empty")} style={{ width: size, height: size }} role="img" aria-label={label} title={label}>
      <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} aria-hidden>
        <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke="var(--chart-track)" strokeWidth={sw}
          strokeDasharray={empty ? `${Math.max(2, c / 40)} ${Math.max(2, c / 40)}` : undefined} />
        {!empty && segments.map((s, i) => {
          const len = Math.max(0, Math.min(1, s.value)) * c;
          const shown = Math.max(0, len - gap);
          const el = shown > 0.5 && (
            <circle key={i} cx={size / 2} cy={size / 2} r={r} fill="none" stroke={s.color} strokeWidth={sw}
              strokeDasharray={`${shown} ${c - shown}`} strokeDashoffset={-at}
              strokeLinecap={segments.length === 1 && s.value < 0.995 && s.value > 0.02 ? "round" : "butt"}
              transform={`rotate(-90 ${size / 2} ${size / 2})`} />
          );
          at += len;
          return el;
        })}
      </svg>
      <div className="ins-ring-center">
        <span className="ins-ring-value" style={{ fontSize: Math.max(11, Math.round(size * 0.22)) }}>{center}</span>
        {sub && <span className="ins-ring-sub">{sub}</span>}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Daily stacked bars
// ---------------------------------------------------------------------------

export interface Series { key: string; label: string; color: string }
export interface Marker { day: string; label: string; title?: string }

function useWidth<T extends HTMLElement>(): [React.RefObject<T | null>, number] {
  const ref = useRef<T>(null);
  const [w, setW] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setW(el.clientWidth);
    const ro = new ResizeObserver(() => setW(el.clientWidth));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, w];
}

/**
 * One bar per day, its parts stacked from the baseline in `series` order.
 * Hovering a day shows its numbers. `compact` drops the axis (for cards).
 */
export function StackedBars({ days, values, series, height = 120, compact, markers = [], label, format = (n) => String(n), extra }: {
  days: string[]; values: Record<string, number[]>; series: Series[]; height?: number; compact?: boolean; markers?: Marker[]; label: string;
  format?: (n: number) => string; extra?: (day: string) => ReactNode;
}) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const [hover, setHover] = useState<number | null>(null);
  const totals = days.map((d) => (values[d] ?? []).reduce((a, b) => a + b, 0));
  const max = Math.max(...totals, 0);
  const top = compact ? 2 : markers.length ? 16 : 6;
  const bottom = compact ? 0 : 18;
  const plotH = height - top - bottom;
  const band = days.length ? width / days.length : 0;
  const barW = Math.max(2, Math.min(compact ? 10 : 22, band * 0.68));
  const y = (v: number) => (max ? (v / max) * plotH : 0);
  const gap = 1.5;
  const ticks = compact || !days.length ? [] : days.length <= 8 && band >= 46 ? days.map((_, i) => i) : [0, Math.floor((days.length - 1) / 2), days.length - 1];
  const h = hover != null ? days[hover] : null;
  return (
    <div className={cx("ins-bars", compact && "compact")} ref={ref} style={{ height }} role="img" aria-label={label}
      onMouseLeave={() => setHover(null)}
      onMouseMove={(e) => {
        if (!band) return;
        const x = e.clientX - e.currentTarget.getBoundingClientRect().left;
        setHover(Math.max(0, Math.min(days.length - 1, Math.floor(x / band))));
      }}>
      {width > 0 && (
        <svg width={width} height={height} aria-hidden>
          {!compact && <line x1={0} x2={width} y1={top + plotH + 0.5} y2={top + plotH + 0.5} stroke="var(--border)" />}
          {!compact && max > 0 && (
            <>
              <line x1={0} x2={width} y1={top + 0.5} y2={top + 0.5} stroke="var(--border)" strokeDasharray="2 4" />
              <text x={width} y={top - 4} textAnchor="end" className="ins-axis">{format(max)}</text>
            </>
          )}
          {hover != null && <rect x={hover * band} y={top - 2} width={band} height={plotH + 2} rx={3} fill="var(--bg-hover)" />}
          {markers.map((m) => {
            const i = days.indexOf(m.day);
            if (i < 0) return null;
            const x = Math.round(i * band + band / 2) + 0.5;
            return (
              <g key={m.day + m.label}>
                <line x1={x} x2={x} y1={top - (compact ? 0 : 2)} y2={top + plotH} stroke="var(--fg-subtle)" strokeDasharray="2 3" />
                {!compact && <text x={x + 3} y={top - 5} className="ins-axis ins-marker">{m.label}</text>}
              </g>
            );
          })}
          {days.map((d, i) => {
            const vals = values[d] ?? [];
            const x = i * band + (band - barW) / 2;
            let base = top + plotH;
            const parts = vals.map((v, k) => ({ v, k })).filter((p) => p.v > 0);
            return (
              <g key={d}>
                {parts.map(({ v, k }, j) => {
                  const hh = Math.max(1.5, y(v) - (j < parts.length - 1 ? gap : 0));
                  base -= y(v);
                  const yy = j < parts.length - 1 ? base + gap : base;
                  return <rect key={k} x={x} y={yy} width={barW} height={hh} rx={Math.min(2, barW / 3)} fill={series[k]?.color} opacity={hover != null && hover !== i ? 0.55 : 1} />;
                })}
                {!vals.some((v) => v > 0) && !compact && <rect x={x} y={top + plotH - 1.5} width={barW} height={1.5} rx={0.75} fill="var(--border-strong)" />}
              </g>
            );
          })}
          {ticks.map((i) => (
            <text key={i} x={Math.min(width - 2, Math.max(2, i * band + band / 2))} y={height - 4} className="ins-axis"
              textAnchor={ticks.length === 3 && i === 0 ? "start" : ticks.length === 3 && i === days.length - 1 ? "end" : "middle"}>{dayLabel(days[i])}</text>
          ))}
        </svg>
      )}
      {h != null && hover != null && (
        <div className={cx("ins-tip", hover > days.length / 2 && "left")} style={hover > days.length / 2 ? { right: width - hover * band + 6 } : { left: (hover + 1) * band + 6 }}>
          <strong>{dayLabel(h, true)}</strong>
          {series.map((s, k) => (
            <span key={s.key} className="ins-tip-row"><i style={{ background: s.color }} />{s.label}<b>{format((values[h] ?? [])[k] ?? 0)}</b></span>
          ))}
          {extra?.(h)}
          {markers.filter((m) => m.day === h).map((m) => <span key={m.label} className="subtle">{m.title ?? m.label}</span>)}
        </div>
      )}
    </div>
  );
}

/** Names the series under a chart: swatch and words, so color isn't the only cue. */
export function Legend({ series }: { series: Series[] }) {
  return (
    <div className="ins-legend">
      {series.map((s) => <span key={s.key}><i style={{ background: s.color }} />{s.label}</span>)}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Split bar
// ---------------------------------------------------------------------------

/** A horizontal bar split into parts; `max` sets the full width, so bars line up against each other. */
export function SplitBar({ parts, max, label, height = 8 }: { parts: { value: number; color: string; label: string }[]; max: number; label: string; height?: number }) {
  const shown = parts.filter((p) => p.value > 0);
  return (
    <div className="ins-split" style={{ height }} role="img" aria-label={label} title={label}>
      {shown.map((p, i) => (
        <span key={i} style={{ width: `${max ? (p.value / max) * 100 : 0}%`, background: p.color }} />
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Change against the previous window
// ---------------------------------------------------------------------------

/**
 * How a number moved since the previous window. `better` says which way is
 * good, which sets the tone; the arrow and words carry it without color.
 */
export function Delta({ now, before, kind, better, enough = true, vs = "vs before" }: {
  now: number | null; before: number | null; kind: "rate" | "ratio"; better: "up" | "down" | "none"; enough?: boolean; vs?: string;
}) {
  if (now == null || before == null || !enough) return <span className="ins-delta none">{before == null || before === 0 ? "nothing earlier to compare" : "too few runs to compare"}</span>;
  let diff: number, text: string;
  if (kind === "rate") {
    diff = Math.round((now - before) * 100);
    text = `${Math.abs(diff)} pt${Math.abs(diff) === 1 ? "" : "s"}`;
  } else {
    if (before === 0) return <span className="ins-delta none">new this window</span>;
    diff = Math.round(((now - before) / before) * 100);
    text = `${Math.abs(diff)}%`;
  }
  if (diff === 0) return <span className="ins-delta flat" aria-label="No change from the previous window">= same as before</span>;
  const up = diff > 0;
  const good = better === "none" ? null : (better === "up") === up;
  return (
    <span className={cx("ins-delta", good === true && "good", good === false && "bad")} aria-label={`${up ? "Up" : "Down"} ${text} from the previous window`}>
      {up ? "▲" : "▼"} {text} <span className="ins-delta-vs">{vs}</span>
    </span>
  );
}
