import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { cx } from "../../design/components";
import { Brand } from "../../design/brands";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { useHarnesses, useModels } from "../../lib/hooks";
import type { AgentKind, Harness } from "../../lib/types";

/** A harness and one of its models; `model: null` is the harness's default. */
export interface ModelPick { agent: AgentKind; model: string | null }

/** A harness's mark, from its metadata. */
export function HarnessIcon({ harness, size = 12 }: { harness: Harness | undefined; size?: number }) {
  if (harness?.icon.brand) return <Brand name={harness.icon.brand} size={size} />;
  if (harness?.icon.svg) return <img src={`data:image/svg+xml;utf8,${encodeURIComponent(harness.icon.svg)}`} width={size} height={size} alt="" />;
  return <Icon name="bot" size={size} />;
}

/** Harnesses installed on this computer, by what tab found. */
export function useInstalled() {
  const { data } = useQuery({ queryKey: ["probes"], queryFn: api.detectAgents, staleTime: 60_000 });
  return new Set(data?.filter((p) => p.found).map((p) => p.kind) ?? []);
}

interface Row { agent: AgentKind; model: string | null; label: string; common: boolean }

/**
 * One searchable list of every installed harness's models, each row showing
 * its harness. Each harness's default comes first, then the common models,
 * then the rest. A chat that has started is tied to its harness: `lock`
 * keeps the others visible but not pickable.
 */
export function ModelMenu({
  value,
  onChange,
  lock,
  only,
  variant = "bar",
  defaultLabel,
  title = "Model",
}: {
  value: ModelPick | null;
  onChange: (pick: ModelPick) => void;
  /** Only this harness can be picked. */
  lock?: AgentKind;
  /** Show only these harnesses (e.g. the one chosen in onboarding). */
  only?: AgentKind[];
  variant?: "bar" | "field";
  /** What "Default model" means for a harness, shown under it. */
  defaultLabel?: (h: Harness) => string | null;
  title?: string;
}) {
  const { data: harnesses = [] } = useHarnesses();
  const { data: models = [] } = useModels();
  const installed = useInstalled();
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const ref = useRef<HTMLSpanElement>(null);
  const byId = useMemo(() => new Map(harnesses.map((h) => [h.id, h])), [harnesses]);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);
  useEffect(() => setQ(""), [open]);

  const shown = harnesses.filter((h) => (installed.has(h.id) || models.some((m) => m.agent === h.id) || h.id === value?.agent) && (!only || only.includes(h.id)));
  const rows: Row[] = [
    ...shown.map((h) => ({ agent: h.id, model: null, label: "Default model", common: true })),
    ...models.filter((m) => shown.some((h) => h.id === m.agent)).map((m) => ({ agent: m.agent, model: m.id, label: m.label, common: m.common })),
  ];
  // A model typed in Settings that no list has.
  if (value?.model && !rows.some((r) => r.agent === value.agent && r.model === value.model)) rows.push({ agent: value.agent, model: value.model, label: value.model, common: true });

  const needle = q.trim().toLowerCase();
  const match = (r: Row) => !needle || [r.label, r.model ?? "default", byId.get(r.agent)?.name ?? ""].some((s) => s.toLowerCase().includes(needle));
  const filtered = rows.filter(match);
  const locked = (r: Row) => !!lock && r.agent !== lock;
  // Common models first, grouped by harness; then each harness's other
  // models under its own heading. A search keeps the harness groups.
  const byHarness = (list: Row[]) => shown.map((h) => ({ harness: h, rows: list.filter((r) => r.agent === h.id) })).filter((g) => g.rows.length);
  const sections: { title: string | null; groups: { harness: Harness; rows: Row[] }[]; headed: boolean }[] = needle
    ? [{ title: null, groups: byHarness(filtered), headed: true }]
    : [
        { title: "Common", groups: byHarness(filtered.filter((r) => r.common)), headed: shown.length > 1 },
        ...byHarness(filtered.filter((r) => !r.common)).map((g) => ({ title: shown.length > 1 ? g.harness.name : "More models", groups: [g], headed: false })),
      ];

  const current = value ? rows.find((r) => r.agent === value.agent && r.model === value.model) : undefined;
  const h = value ? byId.get(value.agent) : undefined;
  const pick = (r: Row) => {
    if (locked(r)) return;
    setOpen(false);
    if (r.agent !== value?.agent || r.model !== value?.model) onChange({ agent: r.agent, model: r.model });
  };

  return (
    <span className={cx("model-menu", variant)} ref={ref}>
      <button
        type="button"
        className={cx(variant === "bar" ? "bar-picker-btn" : "select model-menu-field")}
        onClick={() => setOpen((v) => !v)}
        title={h ? `${title}: ${h.name} · ${current?.label ?? "Default model"}` : title}
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        {h && <HarnessIcon harness={h} size={11} />}
        <span className="model-menu-label">{current?.label ?? (value ? "Default model" : "Pick a model")}</span>
        {variant === "field" && h && <small>{h.name}</small>}
        <Icon name="chevron-down" size={11} />
      </button>
      {open && (
        <div className={cx("model-menu-pop", variant)} role="listbox">
          <input
            className="model-menu-search"
            autoFocus
            placeholder="Search models"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setOpen(false);
              if (e.key === "Enter") {
                e.preventDefault();
                const first = sections.flatMap((x) => x.groups.flatMap((g) => g.rows)).find((r) => !locked(r));
                if (first) pick(first);
              }
            }}
          />
          <div className="model-menu-list">
            {sections.map((sec, i) =>
              sec.groups.length ? (
                <div key={sec.title ?? i}>
                  {sec.title && (
                    <div className="model-menu-section">
                      {sec.title}
                      {!sec.headed && sec.groups[0] && locked(sec.groups[0].rows[0]) && <span> · not for this chat</span>}
                    </div>
                  )}
                  {sec.groups.map((g) => (
                    <div key={g.harness.id} className="model-menu-group">
                      {sec.headed && (
                        <div className="model-menu-harness-head">
                          <HarnessIcon harness={g.harness} size={11} /> {g.harness.name}
                          {locked(g.rows[0]) && <span> · not for this chat</span>}
                        </div>
                      )}
                      {g.rows.map((r) => {
                        const harness = g.harness;
                        const on = value?.agent === r.agent && value?.model === r.model;
                        const help = r.model === null ? defaultLabel?.(harness) : null;
                        return (
                          <button
                            key={`${r.agent}:${r.model ?? ""}`}
                            type="button"
                            role="option"
                            aria-selected={on}
                            aria-disabled={locked(r)}
                            className={cx("model-menu-row", on && "on", locked(r) && "locked")}
                            title={locked(r) ? `This chat runs on ${byId.get(lock!)?.name}. Start a new chat to use ${harness.name}.` : `${harness.name} · ${r.model ?? "its default model"}`}
                            onClick={() => pick(r)}
                          >
                            <HarnessIcon harness={harness} size={13} />
                            <span className="model-menu-name">
                              {r.label}
                              {help && <small> · {help}</small>}
                              {!help && r.model?.includes("/") && r.label !== r.model && <small> · {r.model.slice(0, r.model.indexOf("/"))}</small>}
                            </span>
                            {on ? <Icon name="check" size={12} /> : <span />}
                          </button>
                        );
                      })}
                    </div>
                  ))}
                </div>
              ) : null,
            )}
            {!filtered.length && <div className="model-menu-empty">No models match "{q}".</div>}
          </div>
        </div>
      )}
    </span>
  );
}

/** The model `agent`'s chats use, as the backend's `model_for` reads it. */
export function modelFor(agent: { kind: AgentKind | null; model: string | null; models?: Record<string, string> }, kind: AgentKind): string | null {
  return agent.models?.[kind] ?? (agent.kind === kind ? agent.model : null) ?? null;
}
