import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Badge, Button, EmptyState, ScorePill, Tabs, TextInput, cx } from "../../design/components";
import { Brand, SourceGlyph, hasBrand } from "../../design/brands";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { useApp } from "../../lib/app-context";
import { timeAgo, timeUntil } from "../../lib/format";
import { useItems } from "../../lib/hooks";
import { keys } from "../../lib/query";
import type { ItemState, QueueItem, Urgency } from "../../lib/types";
import { takePendingOpen } from "./Chat";
import { ItemPane } from "./ItemPane";
import { NewChat } from "./NewChat";
import { SnoozeMenu } from "./SnoozeMenu";

const urgencyOrder: Urgency[] = ["now", "today", "this_week", "whenever"];
const urgencyHeading: Record<Urgency, string> = { now: "Now", today: "Today", this_week: "This week", whenever: "Whenever" };
const NEW = "__new__";

export function WorkTab() {
  const { apps } = useApp();
  const qc = useQueryClient();
  const [view, setView] = useState<ItemState>("open");
  const [app, setApp] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const { data: items = [] } = useItems(view, query);
  const [selected, setSelected] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  const counts = useMemo(() => {
    const c: Record<string, number> = {};
    for (const i of items) c[i.kind.split(".")[0]] = (c[i.kind.split(".")[0]] ?? 0) + 1;
    return c;
  }, [items]);

  const visible = useMemo(() => (app ? items.filter((i) => i.kind.startsWith(`${app}.`)) : items), [items, app]);

  // Changing what's listed starts from the top; otherwise keep the selection,
  // so opening a related (or done) item from the pane sticks.
  useEffect(() => setSelected((s) => (s === NEW ? s : null)), [view, app, query]);
  // A chat started elsewhere (e.g. "Set up with assistant" in Connections).
  useEffect(() => {
    const open = () => {
      const id = takePendingOpen();
      if (!id) return;
      setView("open");
      setApp(null);
      setQuery("");
      // After the reset above clears the selection.
      setTimeout(() => setSelected(id));
      qc.invalidateQueries({ queryKey: keys.allItems });
    };
    open();
    window.addEventListener("tab:open-item", open);
    return () => window.removeEventListener("tab:open-item", open);
  }, [qc]);
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
      if (id !== selected) return;
      const idx = visible.findIndex((i) => i.id === id);
      const next = visible[idx + 1] ?? visible[idx - 1];
      setSelected(next?.id ?? null);
    },
    [visible, selected],
  );

  // Optimistic: the row leaves the list right away; the engine event confirms.
  const act = useCallback(
    async (item: QueueItem, action: "done" | "reopen" | { snooze: number }) => {
      advance(item.id);
      qc.setQueriesData<QueueItem[]>({ queryKey: keys.allItems }, (xs) => xs?.filter((x) => x.id !== item.id));
      if (action === "done") await api.markDone(item.id);
      else if (action === "reopen") await api.reopen(item.id);
      else await api.snooze(item.id, action.snooze);
    },
    [advance, qc],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const typing = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement || (e.target as HTMLElement)?.isContentEditable;
      if (e.key === "Escape" && typing) (e.target as HTMLElement).blur();
      if (typing || e.metaKey || e.ctrlKey) return;
      if (e.key === "j" || e.key === "ArrowDown") (e.preventDefault(), move(1));
      if (e.key === "k" || e.key === "ArrowUp") (e.preventDefault(), move(-1));
      if (e.key === "/") (e.preventDefault(), searchRef.current?.focus());
      if (e.key === "n") (e.preventDefault(), setSelected(NEW));
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
            <Button variant={selected === NEW ? "primary" : "secondary"} size="sm" icon="plus" shortcut="N" onClick={() => setSelected(NEW)}>
              New
            </Button>
          </div>
          <div className="queue-filters">
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
                  {hasBrand(a.icon) ? <Brand name={a.icon} size={12} /> : <Icon name={a.icon} size={12} />} {a.label} <span className="count">{counts[a.id]}</span>
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
                <QueueRow key={i.id} item={i} selected={i.id === selected} onSelect={() => setSelected(i.id)} onAct={(a) => act(i, a)} />
              ))}
            </div>
          ))}
        </div>
      </section>
      <section className="pane">
        {selected === NEW ? (
          <NewChat onStarted={(itemId) => {
            setView("open");
            setSelected(itemId);
          }} />
        ) : selected ? (
          <ItemPane key={selected} id={selected} onActed={advance} onSelect={setSelected} />
        ) : (
          <EmptyState icon="inbox" title="Nothing selected">Pick something from the queue, or start a new chat.</EmptyState>
        )}
      </section>
    </div>
  );
}

/**
 * One queue row. Hover actions sit in an overlay on the right edge, on top
 * of the score and time, so showing them never moves or wraps any content.
 */
export function QueueRow({ item, selected, onSelect, onAct, compact }: {
  item: QueueItem;
  selected: boolean;
  onSelect: () => void;
  onAct?: (a: "done" | "reopen" | { snooze: number }) => void;
  compact?: boolean;
}) {
  const { kind } = useApp();
  const k = kind(item.kind);
  const [menu, setMenu] = useState(false);
  return (
    <div id={compact ? undefined : `row-${item.id}`} role="option" aria-selected={selected} className={cx("row", selected && "selected", menu && "menu-open", compact && "compact")} onClick={onSelect}>
      <SourceGlyph kind={item.kind} source={item.source} icon={k.icon} tone={k.tone} size={compact ? 24 : 28} />
      <div className="row-main">
        <div className="row-title">{item.title}</div>
        <div className="row-meta">{[item.actor, item.context].filter(Boolean).join(" · ")}</div>
        {!compact && item.body && <div className="row-snippet">{item.body}</div>}
      </div>
      <div className="row-side">
        <ScorePill score={item.ranking?.score} urgency={item.ranking?.urgency} pending={!item.ranking} />
        <span className="row-time">{timeAgo(item.occurred_at)}</span>
        {item.state === "snoozed" && item.snoozed_until && <Badge icon="clock">{timeUntil(item.snoozed_until)}</Badge>}
      </div>
      {onAct && (
        <div className="row-actions" onClick={(e) => e.stopPropagation()}>
          {item.state === "done" ? (
            <button className="row-action" title="Reopen" onClick={() => onAct("reopen")}>
              <Icon name="undo" size={15} />
            </button>
          ) : (
            <>
              <button className={cx("row-action", menu && "active")} title="Snooze" onClick={() => setMenu((v) => !v)}>
                <Icon name="clock" size={15} />
              </button>
              <button className="row-action" title="Done" onClick={() => onAct("done")}>
                <Icon name="check" size={15} />
              </button>
            </>
          )}
          {menu && <SnoozeMenu onPick={(m) => (setMenu(false), onAct({ snooze: m }))} onClose={() => setMenu(false)} />}
        </div>
      )}
    </div>
  );
}
