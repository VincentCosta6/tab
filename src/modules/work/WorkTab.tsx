import { flushSync } from "react-dom";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Badge, Button, EmptyState, Tabs, TextInput, cx } from "../../design/components";
import { Brand, SourceGlyph, hasBrand } from "../../design/brands";
import { FlapTitle } from "../../design/FlapTitle";
import { Icon } from "../../design/icons";
import { useApp } from "../../lib/app-context";
import { timeAgo, timeUntil } from "../../lib/format";
import { useAgentActivity, useItems } from "../../lib/hooks";
import { SessionPill, StatusPill, sessionLook } from "./SessionPill";
import { taskStatus, unread, useContextStatus, useMarkSeen, useSeen, type TaskStatus } from "./taskStatus";
import { setHere, takePendingDone } from "../../lib/nav";
import { keys } from "../../lib/query";
import type { AgentActivity, ItemState, QueueItem, Urgency } from "../../lib/types";
import { takePendingOpen } from "./Chat";
import { ItemPane } from "./ItemPane";
import { NewChat } from "./NewChat";
import { ProjectChip, ProjectPicker, useRememberedProject } from "./Projects";
import { SnoozeMenu, snoozeOptions } from "./SnoozeMenu";
import { ScoreBreakdown } from "./ScoreBreakdown";
import { api } from "../../lib/api";
import { actOnItem, type ItemAction } from "./optimistic";

const urgencyOrder: Urgency[] = ["now", "today", "this_week", "whenever"];
const urgencyHeading: Record<Urgency, string> = { now: "Now", today: "Today", this_week: "This week", whenever: "Whenever" };
const NEW = "__new__";
type Leaving = "done" | "away";
/** A row animating out, kept drawn after the optimistic update drops it from the list. */
type Ghost = { kind: Leaving; item: QueueItem; after: string | null };
const WIDTH_KEY = "tab.queueWidth";
const DEFAULT_LIST_WIDTH = 420;
const clampWidth = (w: number) => Math.round(Math.min(Math.max(w, 300), Math.max(320, window.innerWidth - 560)));

export function WorkTab() {
  const { apps } = useApp();
  const qc = useQueryClient();
  const [view, setView] = useState<ItemState>("open");
  const [app, setApp] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const { data: all = [], isSuccess: loaded } = useItems(view, query);
  // "All projects" (null) or one project; remembered across launches.
  const [project, setProject] = useRememberedProject("tab.queueProject");
  const items = useMemo(() => (project ? all.filter((i) => i.project_id === project) : all), [all, project]);
  const [selected, setSelected] = useState<string | null>(null);
  // Rows animating out after Done, Snooze or Reopen: still drawn, no longer selectable.
  const [leaving, setLeaving] = useState<ReadonlyMap<string, Ghost>>(() => new Map());
  // The item that opened because the one before it was closed; it rises in.
  const [arrived, setArrived] = useState<string | null>(null);
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
  const live = useMemo(() => (leaving.size ? visible.filter((i) => !leaving.has(i.id)) : visible), [visible, leaving]);
  // What's drawn: the list, with leaving rows put back where they were.
  const drawn = useMemo(() => {
    if (!leaving.size) return visible;
    const out = [...visible];
    for (const [id, g] of leaving) {
      if (out.some((i) => i.id === id)) continue;
      out.splice(g.after ? out.findIndex((i) => i.id === g.after) + 1 : 0, 0, g.item);
    }
    return out;
  }, [visible, leaving]);
  useEffect(() => setHere({ itemId: selected === NEW ? null : selected }), [selected]);

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
      if (s === NEW || (s && live.some((i) => i.id === s))) return s;
      recheck.current = null;
      return live[0]?.id ?? null;
    });
  }, [live]);
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
    if (!selected && live.length) setSelected(live[0].id);
  }, [live, selected]);

  const move = useCallback(
    (delta: number) => {
      const idx = live.findIndex((i) => i.id === selected);
      const next = live[Math.max(0, Math.min(live.length - 1, idx + delta))];
      if (next) {
        setSelected(next.id);
        document.getElementById(`row-${next.id}`)?.scrollIntoView({ block: "nearest" });
      }
    },
    [live, selected],
  );

  /**
   * After acting on an item, select another so triage keeps flowing. Done
   * jumps to the top of the queue, or a new chat once the queue is empty;
   * other actions (snooze, reopen) step to the neighbouring row.
   */
  const advance = useCallback(
    (id: string, done = false) => {
      if (id !== selected) return;
      let next: string | null;
      if (done) next = live.find((i) => i.id !== id)?.id ?? NEW;
      else {
        const idx = live.findIndex((i) => i.id === id);
        next = (live[idx + 1] ?? live[idx - 1])?.id ?? null;
      }
      setSelected(next);
      setArrived(next);
      if (next && next !== NEW) document.getElementById(`row-${next}`)?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    },
    [live, selected],
  );

  const dropGhost = useCallback((id: string) => setLeaving((m) => {
    if (!m.has(id)) return m;
    const n = new Map(m);
    n.delete(id);
    return n;
  }), []);

  // Optimistic: the next item opens and the row animates out right away; the
  // engine event confirms. If Done is called off (a guard the person
  // declines) the row comes back.
  const act = useCallback(
    async (item: QueueItem, action: ItemAction) => {
      if (leaving.has(item.id)) return;
      const done = action === "done";
      const idx = visible.findIndex((i) => i.id === item.id);
      setLeaving((m) => new Map(m).set(item.id, { kind: done ? "done" : "away", item, after: idx > 0 ? visible[idx - 1].id : null }));
      advance(item.id, done);
      const anim = animateOut(item.id, done);
      const ok = await actOnItem(qc, item, action).catch((e) => (console.warn("item action failed", e), false));
      if (!ok) {
        document.getElementById(`row-${item.id}`)?.getAnimations().forEach((a) => a.cancel());
        dropGhost(item.id);
        setSelected(item.id);
        return;
      }
      await anim;
      dropGhost(item.id);
    },
    [advance, dropGhost, leaving, visible, qc],
  );

  // Closed from somewhere else (a merged PR's "Close task"): select it, so
  // closing it opens the next one, then close it like the Done button.
  const [closing, setClosing] = useState<string | null>(() => takePendingDone());
  useEffect(() => {
    const on = () => setClosing(takePendingDone());
    window.addEventListener("tab:done-item", on);
    return () => window.removeEventListener("tab:done-item", on);
  }, []);
  useEffect(() => {
    if (!closing || !loaded) return;
    const item = live.find((i) => i.id === closing);
    if (item && selected !== closing) return setSelected(closing);
    setClosing(null);
    if (item) void act(item, "done");
    else void api.itemDetail(closing).then((d) => d?.item && actOnItem(qc, d.item, "done")).catch((e) => console.warn("item action failed", e));
  }, [closing, loaded, live, selected, act, qc]);

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
  const seen = useSeen();
  // Opening a chat reads its latest reply.
  const openId = selected && selected !== NEW ? selected : null;
  useMarkSeen(openId, openId ? activity.get(openId) : undefined);
  // Only items with a chat get a task status; that keeps the lookups few.
  const contextStatus = useContextStatus(drawn.filter((i) => activity.has(i.id)).map((i) => i.id));
  const grouped = useMemo(() => {
    if (view !== "open" || query) return [{ urgency: null as Urgency | null, items: drawn }];
    return urgencyOrder
      .map((u) => ({ urgency: u as Urgency | null, items: drawn.filter((i) => (i.ranking?.urgency ?? "today") === u) }))
      .filter((g) => g.items.length);
  }, [drawn, view, query]);

  return (
    <div className="work" style={{ gridTemplateColumns: `${listWidth}px 1fr` }}>
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
          {!drawn.length && (
            <EmptyState icon={query ? "search" : "check"} title={query ? "No matches" : view === "open" ? "Queue is clear" : `Nothing ${view}`}>
              {query ? "Search covers titles, bodies, people and places." : view === "open" ? "New work from your connections lands here, ranked." : undefined}
            </EmptyState>
          )}
          {grouped.map((g) => (
            <div key={g.urgency ?? "all"}>
              {g.urgency && <div className={cx("urgency-group", `urgency-${g.urgency}`)}>{urgencyHeading[g.urgency]}</div>}
              {g.items.map((i) => (
                <QueueRow key={i.id} item={i} agent={activity.get(i.id)} status={taskStatus(activity.get(i.id), contextStatus.get(i.id), unread(activity.get(i.id), seen))} selected={i.id === selected} leaving={leaving.get(i.id)?.kind} onSelect={() => setSelected(i.id)} onAct={(a) => act(i, a)} />
              ))}
            </div>
          ))}
        </div>
      </section>
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
          <ItemPane key={selected} id={selected} arrived={arrived === selected} onAct={act} onSelect={(id) => ((recheck.current = null), setSelected(id))} />
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
/** Byline parts worth showing: the source icon already says where it's
 *  from, so "tab" (and "you" on your own tasks) would only repeat it. */
function byline(item: QueueItem): string {
  const same = (v: string | null) => !!v && v.toLowerCase() === item.source.toLowerCase();
  const actor = item.source === "tab" && item.actor === "you" ? null : item.actor;
  return [actor, same(item.context) ? null : item.context].filter(Boolean).join(" · ");
}

export function QueueRow({ item, agent, status, selected, leaving, onSelect, onAct, compact }: {
  item: QueueItem;
  agent?: AgentActivity;
  /** The task's one status; without it the row shows the agent's. */
  status?: TaskStatus | null;
  selected: boolean;
  /** Animating out: "done" strikes it through first. */
  leaving?: Leaving;
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
    <div id={compact ? undefined : `row-${item.id}`} role="option" aria-selected={selected} className={cx("row", selected && "selected", menu && "menu-open", compact && "compact", leaving && "leaving", leaving === "done" && "leaving-done")} onClick={onSelect}
      onContextMenu={(e) => (e.preventDefault(), setMenu(false), setCtx({ x: e.clientX, y: e.clientY }))}>
      <span className="row-glyph">
        <SourceGlyph kind={item.kind} source={item.source} icon={k.icon} tone={k.tone} size={compact ? 24 : 28} />
        {leaving === "done" && <span className="row-check"><Icon name="check" size={compact ? 13 : 15} /></span>}
      </span>
      <div className="row-main">
        <FlapTitle as="div" className="row-title" id={item.id} text={item.title} />
        <div className="row-meta">
          <span className="row-meta-main">
            {status ? <StatusPill status={agent ? { ...status, title: `${status.title} · chat updated ${timeAgo(agent.updated_at)}` } : status} />
              : agent && <SessionPill status={agent.status} title={`Chat ${sessionLook[agent.status].label.toLowerCase()} · updated ${timeAgo(agent.updated_at)}`} />}
            <ProjectChip id={item.project_id} />
            <span className="row-byline">{byline(item)}</span>
          </span>
          {item.state === "snoozed" && item.snoozed_until && <Badge icon="clock">{timeUntil(item.snoozed_until)}</Badge>}
          <span className="row-time">{timeAgo(item.occurred_at)}</span>
        </div>
        {!compact && item.body && <div className="row-snippet">{item.body}</div>}
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

/**
 * A closed row's exit. Done strikes the title through and pops a check on
 * the glyph (CSS, from the leaving-done class), then the row slides right and
 * folds away so the rows below glide up. Snooze and Reopen skip the strike.
 */
async function animateOut(id: string, done: boolean) {
  const el = document.getElementById(`row-${id}`);
  if (!el || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const h = el.offsetHeight;
  try {
    await el.animate([{ transform: "none", opacity: 1 }, { transform: "translateX(56px)", opacity: 0 }], { duration: 220, delay: done ? 360 : 0, easing: "cubic-bezier(.5, 0, .9, .4)", fill: "forwards" }).finished;
    await el.animate([{ height: `${h}px` }, { height: "0px", paddingTop: "0px", paddingBottom: "0px" }], { duration: 180, easing: "cubic-bezier(.4, 0, .2, 1)", fill: "forwards" }).finished;
  } catch {
    /* cancelled: Done was called off and the row is back */
  }
}
