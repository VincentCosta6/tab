import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Badge, EmptyState, ScorePill, SourceIcon, Tabs, TextInput, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api, onEngineEvent } from "../../lib/api";
import { useApp } from "../../lib/app-context";
import { timeAgo, timeUntil } from "../../lib/format";
import type { ItemState, QueueItem, Tone, Urgency } from "../../lib/types";
import { ItemDetailPane } from "./ItemDetail";

const urgencyOrder: Urgency[] = ["now", "today", "this_week", "whenever"];
const urgencyHeading: Record<Urgency, string> = { now: "Now", today: "Today", this_week: "This week", whenever: "Whenever" };

export function WorkTab() {
  const { kind, apps } = useApp();
  const [view, setView] = useState<ItemState>("open");
  const [app, setApp] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [items, setItems] = useState<QueueItem[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  const load = useCallback(async () => {
    const states: ItemState[] = [view];
    setItems(query.trim() ? await api.searchItems(query, states) : await api.listItems(states));
  }, [view, query]);

  useEffect(() => {
    load();
  }, [load]);

  // Live updates: coalesce bursts of engine events into one reload.
  useEffect(() => {
    let t: number | undefined;
    let off: (() => void) | undefined;
    onEngineEvent((e) => {
      if (e.type !== "item_changed") return;
      clearTimeout(t);
      t = window.setTimeout(load, 150);
    }).then((u) => (off = u));
    return () => {
      clearTimeout(t);
      off?.();
    };
  }, [load]);

  const counts = useMemo(() => {
    const c: Record<string, number> = {};
    for (const i of items) c[i.kind.split(".")[0]] = (c[i.kind.split(".")[0]] ?? 0) + 1;
    return c;
  }, [items]);

  const visible = useMemo(() => (app ? items.filter((i) => i.kind.startsWith(`${app}.`)) : items), [items, app]);

  // Changing what's listed starts from the top; otherwise keep the selection,
  // so opening a related (or done) item from the detail pane sticks.
  useEffect(() => setSelected(null), [view, app, query]);
  useEffect(() => {
    if (!selected && visible.length) setSelected(visible[0].id);
  }, [visible, selected]);

  const move = useCallback(
    (delta: number) => {
      const idx = visible.findIndex((i) => i.id === selected);
      const next = visible[Math.max(0, Math.min(visible.length - 1, idx + delta))];
      if (next) {
        setSelected(next.id);
        document.getElementById(`row-${next.id}`)?.scrollIntoView({ block: "nearest" });
      }
    },
    [visible, selected],
  );

  /** After acting on an item, select the next one so triage keeps flowing. */
  const advance = useCallback(
    (id: string) => {
      const idx = visible.findIndex((i) => i.id === id);
      const next = visible[idx + 1] ?? visible[idx - 1];
      setSelected(next?.id ?? null);
    },
    [visible],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const typing = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement;
      if (e.key === "Escape" && typing) (e.target as HTMLElement).blur();
      if (typing || e.metaKey || e.ctrlKey) return;
      if (e.key === "j" || e.key === "ArrowDown") (e.preventDefault(), move(1));
      if (e.key === "k" || e.key === "ArrowUp") (e.preventDefault(), move(-1));
      if (e.key === "/") (e.preventDefault(), searchRef.current?.focus());
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [move]);

  const grouped = useMemo(() => {
    if (view !== "open" || query) return [{ urgency: null as Urgency | null, items: visible }];
    return urgencyOrder
      .map((u) => ({ urgency: u as Urgency | null, items: visible.filter((i) => (i.ranking?.urgency ?? "today") === u) }))
      .filter((g) => g.items.length);
  }, [visible, view, query]);

  return (
    <div className="work">
      <section className="queue">
        <div className="queue-top">
          <div className="queue-title">
            <h1>Work</h1>
            <Tabs<ItemState>
              value={view}
              onChange={setView}
              items={[
                { value: "open", label: "Queue" },
                { value: "snoozed", label: "Snoozed" },
                { value: "done", label: "Done" },
              ]}
            />
          </div>
          <TextInput ref={searchRef} icon="search" placeholder="Search everything  /" value={query} onChange={(e) => setQuery(e.target.value)} />
          <div className="chips">
            <button className={cx("chip", !app && "active")} onClick={() => setApp(null)}>
              All <span className="count">{items.length}</span>
            </button>
            {apps
              .filter((a) => counts[a.id])
              .map((a) => (
                <button key={a.id} className={cx("chip", app === a.id && "active")} onClick={() => setApp(app === a.id ? null : a.id)}>
                  <Icon name={a.icon} size={12} /> {a.label} <span className="count">{counts[a.id]}</span>
                </button>
              ))}
          </div>
        </div>
        <div className="queue-list" role="listbox" aria-label="Work queue">
          {!visible.length && (
            <EmptyState icon={query ? "search" : "check"} title={query ? "No matches" : view === "open" ? "Queue is clear" : `Nothing ${view}`}>
              {query ? "Search covers titles, bodies, people and places." : view === "open" ? "New work from your connections lands here, ranked." : undefined}
            </EmptyState>
          )}
          {grouped.map((g) => (
            <div key={g.urgency ?? "all"}>
              {g.urgency && <div className={cx("urgency-group", `urgency-${g.urgency}`)}>{urgencyHeading[g.urgency]}</div>}
              {g.items.map((i) => (
                <QueueRow key={i.id} item={i} selected={i.id === selected} onSelect={() => setSelected(i.id)} kindIcon={kind(i.kind).icon} kindTone={kind(i.kind).tone} />
              ))}
            </div>
          ))}
        </div>
      </section>
      <section className="detail">
        {selected ? <ItemDetailPane id={selected} onActed={advance} onSelect={setSelected} /> : <EmptyState icon="inbox" title="Nothing selected" />}
      </section>
    </div>
  );
}

export function QueueRow({ item, selected, onSelect, kindIcon, kindTone }: { item: QueueItem; selected: boolean; onSelect: () => void; kindIcon: string; kindTone: Tone }) {
  return (
    <div id={`row-${item.id}`} role="option" aria-selected={selected} className={cx("row", selected && "selected")} onClick={onSelect}>
      <SourceIcon icon={kindIcon} tone={kindTone} />
      <div className="row-main">
        <div className="row-title">{item.title}</div>
        <div className="row-meta">{[item.actor, item.context].filter(Boolean).join(" · ")}</div>
        {item.ranking && <div className="row-reason">{item.ranking.reason}</div>}
      </div>
      <div className="row-side">
        <ScorePill score={item.ranking?.score} urgency={item.ranking?.urgency} pending={!item.ranking} />
        <span className="row-time">{timeAgo(item.occurred_at)}</span>
        {item.state === "snoozed" && item.snoozed_until && <Badge icon="clock">{timeUntil(item.snoozed_until)}</Badge>}
      </div>
    </div>
  );
}
