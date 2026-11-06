import { flushSync } from "react-dom";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Badge, Button, EmptyState, Tabs, TextInput, cx } from "../../design/components";
import { Brand, SourceGlyph, hasBrand } from "../../design/brands";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { useApp } from "../../lib/app-context";
import { timeAgo, timeUntil } from "../../lib/format";
import { useAgentActivity, useItems } from "../../lib/hooks";
import { keys } from "../../lib/query";
import type { AgentActivity, ItemState, QueueItem, Urgency } from "../../lib/types";
import { takePendingOpen } from "./Chat";
import { FileViewer } from "./FileViewer";
import { ItemPane } from "./ItemPane";
import { registerViewerHost, useViewer } from "../../lib/viewer";
import { NewChat } from "./NewChat";
import { ProjectChip, ProjectPicker, useRememberedProject } from "./Projects";
import { SnoozeMenu, snoozeOptions } from "./SnoozeMenu";
import { ScoreBreakdown } from "./ScoreBreakdown";

const urgencyOrder: Urgency[] = ["now", "today", "this_week", "whenever"];
const urgencyHeading: Record<Urgency, string> = { now: "Now", today: "Today", this_week: "This week", whenever: "Whenever" };
const NEW = "__new__";
const WIDTH_KEY = "tab.queueWidth";
const DEFAULT_LIST_WIDTH = 420;
const clampWidth = (w: number) => Math.round(Math.min(Math.max(w, 300), Math.max(320, window.innerWidth - 560)));

export function WorkTab() {
  const { apps } = useApp();
  const qc = useQueryClient();
  const [view, setView] = useState<ItemState>("open");
  const [app, setApp] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const { data: all = [] } = useItems(view, query);
  // "All projects" (null) or one project; remembered across launches.
  const [project, setProject] = useRememberedProject("tab.queueProject");
  const items = useMemo(() => (project ? all.filter((i) => i.project_id === project) : all), [all, project]);
  const [selected, setSelected] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const [listWidth, setListWidth] = useState(() => {
    try {
      return clampWidth(Number(localStorage.getItem(WIDTH_KEY)) || DEFAULT_LIST_WIDTH);
    } catch {
      return DEFAULT_LIST_WIDTH;
    }
  });
  const saveWidth = (w: number) => {
    setListWidth(clampWidth(w));
    try {
      localStorage.setItem(WIDTH_KEY, String(clampWidth(w)));
    } catch {
      /* private mode: the width just isn't remembered */
    }
  };
  const startResize = (e: React.MouseEvent) => {
    e.preventDefault();
    const x0 = e.clientX;
    const w0 = listWidth;
    let w = w0;
    const move = (ev: MouseEvent) => {
      w = clampWidth(w0 + ev.clientX - x0);
      setListWidth(w);
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.classList.remove("resizing");
      saveWidth(w);
    };
    document.body.classList.add("resizing");
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  const counts = useMemo(() => {
    const c: Record<string, number> = {};
    for (const i of items) c[i.kind.split(".")[0]] = (c[i.kind.split(".")[0]] ?? 0) + 1;
    return c;
  }, [items]);

  const visible = useMemo(() => (app ? items.filter((i) => i.kind.startsWith(`${app}.`)) : items), [items, app]);

  // A new filter or search keeps the open item while it's still in the list,
  // so typing doesn't rebuild the right pane; once the results leave it out,
  // the top result opens. Otherwise the selection sticks, so opening a
  // related (or done) item from the pane stays put.
  const filterKey = `${view}|${app ?? ""}|${query}|${project ?? ""}`;
  const recheck = useRef<string | null>(null);
  useEffect(() => {
    recheck.current = filterKey;
  }, [filterKey]);
  useEffect(() => {
    if (recheck.current === null) return;
    setSelected((s) => {
      if (s === NEW || (s && visible.some((i) => i.id === s))) return s;
      recheck.current = null;
      return visible[0]?.id ?? null;
    });
  }, [visible]);
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

  const activity = useAgentActivity();
  const grouped = useMemo(() => {
    if (view !== "open" || query) return [{ urgency: null as Urgency | null, items: visible }];
    return urgencyOrder
      .map((u) => ({ urgency: u as Urgency | null, items: visible.filter((i) => (i.ranking?.urgency ?? "today") === u) }))
      .filter((g) => g.items.length);
  }, [visible, view, query]);

  // With a file open it takes the middle; the queue steps aside and the chat stays on the right.
  const viewing = useViewer().current !== null;
  useEffect(() => registerViewerHost(), []);

  return (
    <div className={cx("work", viewing && "viewing")} style={{ gridTemplateColumns: viewing ? undefined : `${listWidth}px 1fr` }}>
      <section className="queue">
        <div className="queue-resize" onMouseDown={startResize} onDoubleClick={() => saveWidth(DEFAULT_LIST_WIDTH)} title="Drag to resize. Double-click to reset." />
        <div className="queue-top">
          <div className="queue-title">
            <ProjectPicker value={project} onChange={setProject} none="All projects" />
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
                <QueueRow key={i.id} item={i} agent={activity.get(i.id)} selected={i.id === selected} onSelect={() => setSelected(i.id)} onAct={(a) => act(i, a)} />
              ))}
            </div>
          ))}
        </div>
      </section>
      <FileViewer />
      <section className="pane work-pane">
        {selected === NEW ? (
          <NewChat project={project} onStarted={(itemId) => {
            // The composer glides from the middle of the page down to the
            // chat's foot instead of jumping there.
            const go = () => flushSync(() => {
              setView("open");
              setSelected(itemId);
            });
            const doc = document as Document & { startViewTransition?: (f: () => void) => unknown };
            if (doc.startViewTransition && !matchMedia("(prefers-reduced-motion: reduce)").matches) doc.startViewTransition(go);
            else go();
          }} />
        ) : selected ? (
          <ItemPane key={selected} id={selected} onActed={advance} onSelect={(id) => ((recheck.current = null), setSelected(id))} />
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
const agentText: Record<string, string> = { working: "Agent working", needs_input: "Agent needs you", failed: "Agent failed" };

export function QueueRow({ item, agent, selected, onSelect, onAct, compact }: {
  item: QueueItem;
  agent?: AgentActivity;
  selected: boolean;
  onSelect: () => void;
  onAct?: (a: "done" | "reopen" | { snooze: number }) => void;
  compact?: boolean;
}) {
  const { kind } = useApp();
  const k = kind(item.kind);
  const [menu, setMenu] = useState(false);
  const [ctx, setCtx] = useState<{ x: number; y: number } | null>(null);
  const [showScore, setShowScore] = useState(false);
  return (
    <div id={compact ? undefined : `row-${item.id}`} role="option" aria-selected={selected} className={cx("row", selected && "selected", menu && "menu-open", compact && "compact")} onClick={onSelect}
      onContextMenu={(e) => (e.preventDefault(), setMenu(false), setCtx({ x: e.clientX, y: e.clientY }))}>
      <span className="row-glyph">
        <SourceGlyph kind={item.kind} source={item.source} icon={k.icon} tone={k.tone} size={compact ? 24 : 28} />
        {agent && <span className={cx("agent-dot", `agent-${agent.status}`)} title={agentText[agent.status]} />}
      </span>
      <div className="row-main">
        <div className="row-title">{item.title}</div>
        <div className="row-meta">
          <ProjectChip id={item.project_id} />
          {agent && <span className={cx("row-agent", `agent-${agent.status}`)}>{agentText[agent.status]}</span>}
          {[item.actor, item.context].filter(Boolean).join(" · ")}
        </div>
        {!compact && item.body && <div className="row-snippet">{item.body}</div>}
      </div>
      <div className="row-side">
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
      {ctx && (
        <RowContextMenu
          at={ctx}
          done={item.state === "done"}
          canAct={!!onAct}
          onClose={() => setCtx(null)}
          onAct={(a) => (setCtx(null), onAct?.(a))}
          onScore={() => (setCtx(null), setShowScore(true))}
        />
      )}
      {showScore && <ScoreBreakdown item={item} onClose={() => setShowScore(false)} />}
    </div>
  );
}

/** Right-click menu for a queue row: snooze, done, and why it ranks where it does. */
function RowContextMenu({ at, done, canAct, onClose, onAct, onScore }: {
  at: { x: number; y: number };
  done: boolean;
  canAct: boolean;
  onClose: () => void;
  onAct: (a: "done" | "reopen" | { snooze: number }) => void;
  onScore: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    const onDown = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && onClose();
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("mousedown", onDown);
    window.addEventListener("blur", onClose);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("blur", onClose);
    };
  }, [onClose]);
  // Keep the menu on screen near the window's right and bottom edges.
  const left = Math.min(at.x, window.innerWidth - 200);
  const top = Math.min(at.y, window.innerHeight - 260);
  return (
    <div className="menu context-menu" ref={ref} role="menu" style={{ left, top }} onClick={(e) => e.stopPropagation()}>
      {canAct && !done && (
        <>
          <div className="menu-label">Snooze</div>
          {snoozeOptions.map((o) => (
            <button key={o.key} role="menuitem" onClick={() => onAct({ snooze: o.minutes() })}>{o.label}</button>
          ))}
          <div className="menu-sep" />
          <button role="menuitem" onClick={() => onAct("done")}>Mark done</button>
        </>
      )}
      {canAct && done && <button role="menuitem" onClick={() => onAct("reopen")}>Reopen</button>}
      {canAct && <div className="menu-sep" />}
      <button role="menuitem" onClick={onScore}>View score</button>
    </div>
  );
}
