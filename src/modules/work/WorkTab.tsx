import { createPortal, flushSync } from "react-dom";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Badge, Button, EmptyState, Kbd, TextInput, cx } from "../../design/components";
import { Brand, SourceGlyph, hasBrand } from "../../design/brands";
import { FlapTitle } from "../../design/FlapTitle";
import { Icon } from "../../design/icons";
import { useApp } from "../../lib/app-context";
import { timeAgo, timeUntil } from "../../lib/format";
import { useAgentActivity, useItems, useWorkflowActivity } from "../../lib/hooks";
import { SessionPill, StatusPill, sessionLook } from "./SessionPill";
import { taskStatus, unread, useContextStatus, useMarkSeen, useSeen, type TaskStatus } from "./taskStatus";
import { setHere, takePendingDone } from "../../lib/nav";
import { keys } from "../../lib/query";
import type { AgentActivity, ItemState, ItemWorkflowStatus, QueueItem, Urgency } from "../../lib/types";
import { WorkflowPill } from "./WorkflowStatus";
import { takePendingOpen } from "./Chat";
import { ItemPane } from "./ItemPane";
import { NewChat } from "./NewChat";
import { ProjectDot, ProjectPicker, useRememberedProject } from "./Projects";
import { SuggestionChip } from "./Routing";
import { useHeaderSlot } from "../shell/AppHeader";
import { SnoozeMenu, snoozeOptions } from "./SnoozeMenu";
import { ScoreBreakdown } from "./ScoreBreakdown";
import { api } from "../../lib/api";
import { actOnItem, type ItemAction } from "./optimistic";
import { afterBlockers, nest } from "./followups";
import { isStackId, stack, type Stack } from "./stacks";
import { StackPane, plural } from "./StackPane";

const urgencyOrder: Urgency[] = ["now", "today", "this_week", "whenever"];
const urgencyHeading: Record<Urgency, string> = { now: "Now", today: "Today", this_week: "This week", whenever: "Whenever" };
const NEW = "__new__";
const viewLabel: Record<ItemState, string> = { open: "Queue", snoozed: "Snoozed", done: "Done" };
const appIcon = (icon: string) => (hasBrand(icon) ? <Brand name={icon} size={12} /> : <Icon name={icon} size={12} />);
type Leaving = "done" | "away";
/** A row animating out, kept drawn after the optimistic update drops it from the list. */
type Ghost = { kind: Leaving; item: QueueItem; after: string | null };
const WIDTH_KEY = "tab.queueWidth";
const OPEN_FOLLOWUPS_KEY = "tab.openFollowups";
const DEFAULT_LIST_WIDTH = 420;
const clampWidth = (w: number) => Math.round(Math.min(Math.max(w, 300), Math.max(320, window.innerWidth - 560)));

/** Queue rows drawn at a time. */
const PAGE = 150;

export function WorkTab() {
  const { apps, kind } = useApp();
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
  const [searchOpen, setSearchOpen] = useState(false);
  const headerSlot = useHeaderSlot();
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
  // Follow-ups fold under the task they came from, and swarms of one kind
  // (a project's errors) into a stack; search and the other views stay flat.
  const nested = view === "open" && !query;
  const { top: threads, children } = useMemo(() => (nested ? nest(drawn) : { top: drawn, children: new Map<string, QueueItem[]>() }), [drawn, nested]);
  const { top, stacks } = useMemo(() => (nested ? stack(threads, (k) => !!kind(k).stack) : { top: threads, stacks: new Map<string, Stack>() }), [threads, nested, kind]);
  // Each stack, and each of its members, to the stack's lead row.
  const leadOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const [lead, st] of stacks) for (const k of [st.id, ...st.items.map((i) => i.id)]) m.set(k, lead);
    return m;
  }, [stacks]);
  const openStack = isStackId(selected) ? [...stacks.values()].find((st) => st.id === selected) : undefined;
  const activity = useAgentActivity();
  // Threads with an agent at work on them are today's business, whatever they scored.
  const busy = [...activity.values()].filter((a) => a.status === "working" || a.status === "needs_input").map((a) => a.item_id).sort().join(" ");
  const grouped = useMemo(() => {
    if (!nested) return [{ urgency: null as Urgency | null, items: top }];
    const atWork = new Set(busy.split(" "));
    const urgency = (i: QueueItem): Urgency => {
      const u = i.ranking?.urgency ?? "today";
      return atWork.has(i.id) && (u === "this_week" || u === "whenever") ? "today" : u;
    };
    return urgencyOrder
      .map((u) => ({ urgency: u as Urgency | null, items: afterBlockers(top.filter((i) => urgency(i) === u)) }))
      .filter((g) => g.items.length);
  }, [top, nested, busy]);
  const titles = useMemo(() => new Map(all.map((i) => [i.id, i.title])), [all]);
  const [openFollowups, setOpenFollowups] = useState<ReadonlySet<string>>(() => {
    try {
      return new Set(JSON.parse(localStorage.getItem(OPEN_FOLLOWUPS_KEY) ?? "[]"));
    } catch {
      return new Set();
    }
  });
  const toggleFollowups = (id: string) => setOpenFollowups((s) => {
    const n = new Set(s);
    if (!n.delete(id)) n.add(id);
    try {
      localStorage.setItem(OPEN_FOLLOWUPS_KEY, JSON.stringify([...n]));
    } catch {
      /* private mode: it just isn't remembered */
    }
    return n;
  });
  // The open item's parents unfold, so it's never hidden.
  const unfolded = useMemo(() => {
    const out = new Set(openFollowups);
    const byId = new Map(drawn.map((i) => [i.id, i]));
    for (let p = byId.get(selected ?? "")?.parent_id; p && !out.has(p); p = byId.get(p)?.parent_id) out.add(p);
    // And the stack it's in, when it's open from the pane or a search.
    let root = byId.get(selected ?? "");
    while (root?.parent_id && byId.has(root.parent_id)) root = byId.get(root.parent_id);
    const lead = root && !isStackId(selected) ? leadOf.get(root.id) : undefined;
    if (lead) out.add(stacks.get(lead)!.id);
    return out;
  }, [openFollowups, drawn, selected, leadOf, stacks]);
  // Rows in the order they're drawn, for the arrow keys.
  const shown = useMemo(() => {
    const out: string[] = [];
    const add = (i: QueueItem) => {
      if (!leaving.has(i.id)) out.push(i.id);
      if (unfolded.has(i.id)) children.get(i.id)?.forEach(add);
    };
    for (const g of grouped) for (const i of g.items) {
      const st = stacks.get(i.id);
      if (!st) add(i);
      else if (!st.items.every((m) => leaving.has(m.id))) {
        out.push(st.id);
        if (unfolded.has(st.id)) st.items.forEach(add);
      }
    }
    return out;
  }, [grouped, children, stacks, unfolded, leaving]);
  useEffect(() => setHere({ itemId: selected === NEW || isStackId(selected) ? null : selected }), [selected]);

  // Draw the queue a page at a time: every row costs DOM, and a queue of
  // thousands kept the webview at over a gigabyte. More rows draw as the list
  // nears its end, and always enough to include the open item.
  const [limit, setLimit] = useState(PAGE);
  const tops = useMemo(() => grouped.flatMap((g) => g.items), [grouped]);
  const drawnTops = useMemo(() => {
    const byId = new Map(drawn.map((i) => [i.id, i]));
    let root = byId.get(selected ?? "");
    while (root?.parent_id && byId.has(root.parent_id)) root = byId.get(root.parent_id);
    const lead = byId.get(leadOf.get(root?.id ?? selected ?? "") ?? "");
    const target = lead ?? root;
    const at = target ? tops.indexOf(target) : -1;
    return new Set(tops.slice(0, Math.max(limit, at + 1)).map((i) => i.id));
  }, [tops, drawn, selected, limit, leadOf]);
  const more = tops.length - drawnTops.size;
  const moreRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = moreRef.current;
    if (!el) return;
    const io = new IntersectionObserver((es) => es.some((e) => e.isIntersecting) && setLimit((l) => l + PAGE), { root: el.parentElement, rootMargin: "600px 0px" });
    io.observe(el);
    return () => io.disconnect();
  }, [more > 0, drawnTops]);

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
      if (s === NEW || (s && live.some((i) => i.id === s)) || (isStackId(s) && leadOf.has(s))) return s;
      recheck.current = null;
      return live[0]?.id ?? null;
    });
  }, [live, leadOf]);
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
      const idx = shown.indexOf(selected ?? "");
      const next = shown[Math.max(0, Math.min(shown.length - 1, idx + delta))];
      if (next) {
        setSelected(next);
        document.getElementById(`row-${next}`)?.scrollIntoView({ block: "nearest" });
      }
    },
    [shown, selected],
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

  // A whole stack at once: the row folds away like one item's, then each
  // member goes the usual way. Done steps on to the next row.
  const actOnStack = useCallback(
    async (st: Stack, action: ItemAction) => {
      if (st.id === selected) {
        const rest = shown.filter((id) => id !== st.id && !st.items.some((m) => m.id === id));
        const idx = shown.indexOf(st.id);
        const next = rest.find((id) => shown.indexOf(id) > idx) ?? rest[rest.length - 1] ?? (action === "done" ? NEW : null);
        setSelected(next);
        if (next && next !== NEW) document.getElementById(`row-${next}`)?.scrollIntoView({ block: "nearest", behavior: "smooth" });
      }
      void animateOut(st.id, action === "done");
      await Promise.all(st.items.map((m) => act(m, action)));
      // Some stayed (a Done called off): show the row again.
      document.getElementById(`row-${st.id}`)?.getAnimations().forEach((a) => a.cancel());
    },
    [act, selected, shown],
  );

  // Closed from somewhere else (a merged PR's "Close task"): select it, so
  // closing it opens the next one, then close it like the Done button.
  const [closing, setClosing] = useState<string | null>(() => takePendingDone());
  // A row opened by a click focuses its chat box; j/k leave focus in the queue.
  const [clicked, setClicked] = useState({ id: "", n: 0 });
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
      // Unless the box has a use for it (the composer's Esc Esc).
      if (e.key === "Escape" && typing && !e.defaultPrevented) (e.target as HTMLElement).blur();
      if (typing || e.metaKey || e.ctrlKey) return;
      if (e.key === "j" || e.key === "ArrowDown") (e.preventDefault(), move(1));
      if (e.key === "k" || e.key === "ArrowUp") (e.preventDefault(), move(-1));
      if (e.key === "/") (e.preventDefault(), setSearchOpen(true), searchRef.current?.focus());
      if (e.key === "n") (e.preventDefault(), setSelected(NEW));
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [move]);

  const workflows = useWorkflowActivity();
  const seen = useSeen();
  // Opening a chat reads its latest reply.
  const openId = selected && selected !== NEW && !isStackId(selected) ? selected : null;
  useMarkSeen(openId, openId ? activity.get(openId) : undefined);
  // Only items with a chat get a task status; that keeps the lookups few.
  const contextStatus = useContextStatus(drawn.filter((i) => activity.has(i.id)).map((i) => i.id));
  const stackRow = (st: Stack): React.ReactNode => {
    const open = unfolded.has(st.id);
    return (
      <div key={st.id} className="thread has-kids stack" style={{ "--depth": 0 } as React.CSSProperties}>
        <StackRow stack={st} selected={st.id === selected} onSelect={() => setSelected(st.id)} onAct={(a) => actOnStack(st, a)} />
        {!open ? (
          <button className="thread-more" aria-expanded={false} onClick={() => toggleFollowups(st.id)}>
            Show {st.items.length}
          </button>
        ) : (
          <div className="thread-kids">
            {st.items.map((k) => row(k, 1))}
            <button className="thread-fold" aria-expanded title="Fold the stack" aria-label="Fold the stack" onClick={() => toggleFollowups(st.id)} />
          </div>
        )}
      </div>
    );
  };
  const row = (i: QueueItem, depth = 0): React.ReactNode => {
    const st = depth === 0 ? stacks.get(i.id) : undefined;
    if (st) return stackRow(st);
    const kids = children.get(i.id);
    const open = unfolded.has(i.id);
    const waiting = (i.blocked_by ?? []).map((b) => titles.get(b) ?? "another task");
    return (
      <div key={i.id} className={cx("thread", kids && "has-kids")} style={{ "--depth": depth } as React.CSSProperties}>
        <QueueRow item={i} agent={activity.get(i.id)} workflow={workflows.get(i.id)} status={taskStatus(activity.get(i.id), contextStatus.get(i.id), unread(activity.get(i.id), seen))} waiting={waiting} hideProject={!!project} selected={i.id === selected} leaving={leaving.get(i.id)?.kind} onSelect={() => (setClicked((c) => ({ id: i.id, n: c.n + 1 })), setSelected(i.id))} onAct={(a) => act(i, a)} />
        {kids && !open && (
          <button className="thread-more" aria-expanded={false} onClick={() => toggleFollowups(i.id)}>
            {kids.length} follow-up{kids.length === 1 ? "" : "s"}
            {kids.some((k) => k.blocked_by?.length) && <span className="followups-ready">· {kids.filter((k) => !k.blocked_by?.length).length} ready</span>}
          </button>
        )}
        {kids && open && (
          <div className="thread-kids">
            {kids.map((k) => row(k, depth + 1))}
            <button className="thread-fold" aria-expanded title="Hide follow-ups" aria-label="Hide follow-ups" onClick={() => toggleFollowups(i.id)} />
          </div>
        )}
      </div>
    );
  };

  const searching = searchOpen || !!query;
  const sourceApp = apps.find((a) => a.id === app);
  const toolbar = (
    <div className="queue-bar" style={headerSlot ? { width: listWidth } : undefined} data-tauri-drag-region>
      {searching ? (
        <span className="queue-search">
          <TextInput ref={searchRef} icon="search" autoFocus placeholder="Search everything" value={query} onChange={(e) => setQuery(e.target.value)}
            onBlur={() => !query && setSearchOpen(false)}
            onKeyDown={(e) => e.key === "Escape" && (setQuery(""), setSearchOpen(false))} />
          <button className="icon-btn" title="Close search" aria-label="Close search" onMouseDown={(e) => e.preventDefault()} onClick={() => (setQuery(""), setSearchOpen(false))}>
            <Icon name="x" size={13} />
          </button>
        </span>
      ) : (
        <>
          <ProjectPicker value={project} onChange={setProject} none="All projects" compact />
          <BarMenu
            label={sourceApp ? <>{appIcon(sourceApp.icon)} {sourceApp.label}</> : <>All <span className="count">{items.length}</span></>}
            title="Filter by source"
            value={app}
            onChange={setApp}
            options={[
              { value: null, label: "All sources", count: items.length },
              ...apps.filter((a) => counts[a.id]).map((a) => ({ value: a.id, label: a.label, icon: appIcon(a.icon), count: counts[a.id] })),
            ]}
          />
          <span className="queue-bar-gap" data-tauri-drag-region />
          <button className="icon-btn" title="Search  /" aria-label="Search" onClick={() => setSearchOpen(true)}>
            <Icon name="search" size={14} />
          </button>
        </>
      )}
      <BarMenu
        label={viewLabel[view]}
        title="Show"
        align="right"
        value={view}
        onChange={(v) => v && setView(v)}
        options={(Object.keys(viewLabel) as ItemState[]).map((v) => ({ value: v, label: viewLabel[v] }))}
      />
      <button className={cx("queue-new", selected === NEW && "active")} title="New chat" onClick={() => setSelected(NEW)}>
        <Icon name="plus" size={13} />
        <span>New</span>
        <Kbd>N</Kbd>
      </button>
    </div>
  );

  return (
    <div className="work" style={{ gridTemplateColumns: `${listWidth}px 1fr` }}>
      <section className="queue">
        <div className="queue-resize" onMouseDown={startResize} onDoubleClick={() => saveWidth(DEFAULT_LIST_WIDTH)} title="Drag to resize. Double-click to reset." />
        {/* The toolbar sits in the window's title bar, above the queue, when
            the header has room for it (see AppHeader's lead slot). */}
        {headerSlot ? createPortal(toolbar, headerSlot) : <div className="queue-top">{toolbar}</div>}
        <div className="queue-list" role="listbox" aria-label="Work queue">
          {!drawn.length && (
            <EmptyState icon={query ? "search" : "check"} title={query ? "No matches" : view === "open" ? "Queue is clear" : `Nothing ${view}`}>
              {query ? "Search covers titles, bodies, people and places." : view === "open" ? "New work from your connections lands here, ranked." : undefined}
            </EmptyState>
          )}
          {grouped.map((g) => {
            const rows = g.items.filter((i) => drawnTops.has(i.id));
            return rows.length > 0 && (
              <div key={g.urgency ?? "all"}>
                {g.urgency && <div className={cx("urgency-group", `urgency-${g.urgency}`)}>{urgencyHeading[g.urgency]}</div>}
                {rows.map((i) => row(i))}
              </div>
            );
          })}
          {more > 0 && (
            <div ref={moreRef} className="queue-more">
              <Button variant="ghost" size="sm" onClick={() => setLimit((l) => l + PAGE)}>Show {Math.min(more, PAGE)} more of {more}</Button>
            </div>
          )}
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
        ) : openStack ? (
          <StackPane stack={openStack} project={project} onAct={(a) => actOnStack(openStack, a)} onSelect={setSelected}
            onStarted={(itemId) => (setView("open"), setSelected(itemId))} />
        ) : isStackId(selected) ? (
          <EmptyState icon="check" title="Stack cleared">Pick something from the queue, or start a new chat.</EmptyState>
        ) : selected ? (
          <ItemPane key={selected} id={selected} arrived={arrived === selected} focusChat={clicked.id === selected ? clicked.n : undefined} onAct={act} onSelect={(id) => ((recheck.current = null), setSelected(id))} />
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

export function QueueRow({ item, agent, workflow, status, waiting, hideProject, selected, leaving, onSelect, onAct, compact }: {
  item: QueueItem;
  /** The queue is filtered to one project, so naming it on each row repeats it. */
  hideProject?: boolean;
  /** Titles of the tasks this one waits on. */
  waiting?: string[];
  agent?: AgentActivity;
  /** Where the item's workflows stand, shown beside the agent's status. */
  workflow?: ItemWorkflowStatus;
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
  const by = byline(item);
  const snippet = !compact && item.body ? item.body : "";
  const sub = by || snippet;
  const project = hideProject ? null : item.project_id ? <ProjectDot id={item.project_id} /> : item.suggestion ? <SuggestionChip item={item} /> : null;
  const pill = status ? <StatusPill status={agent ? { ...status, title: `${status.title} · chat updated ${timeAgo(agent.updated_at)}` } : status} />
    : agent ? <SessionPill status={agent.status} title={`Chat ${sessionLook[agent.status].label.toLowerCase()} · updated ${timeAgo(agent.updated_at)}`} /> : null;
  const waitingOn = !!waiting?.length && item.state !== "done" && (
    <span className="row-waiting" title={`Waiting on: ${waiting.join(", ")}`}>
      <Icon name="clock" size={11} /> Waiting{waiting.length > 1 ? ` on ${waiting.length}` : ""}
    </span>
  );
  const snoozed = item.state === "snoozed" && item.snoozed_until && <Badge icon="clock">{timeUntil(item.snoozed_until)}</Badge>;
  return (
    <div id={compact ? undefined : `row-${item.id}`} role="option" aria-selected={selected} className={cx("row", selected && "selected", menu && "menu-open", compact && "compact", leaving && "leaving", leaving === "done" && "leaving-done")} onClick={onSelect}
      onContextMenu={(e) => (e.preventDefault(), setMenu(false), setCtx({ x: e.clientX, y: e.clientY }))}>
      <span className="row-glyph">
        <SourceGlyph kind={item.kind} source={item.source} icon={k.icon} tone={k.tone} size={compact ? 24 : 28} />
        {leaving === "done" && <span className="row-check"><Icon name="check" size={compact ? 13 : 15} /></span>}
      </span>
      {/* Two lines of text, then the status line only when there's one. */}
      <div className="row-main">
        <div className="row-head">
          <FlapTitle as="div" className="row-title" id={item.id} text={item.title} />
          <span className="row-time">{timeAgo(item.occurred_at)}</span>
        </div>
        {(sub || project) && (
          <div className="row-sub">
            {project}
            {sub && <span className="row-sub-text">{by && <span className="row-byline">{by}</span>}{by && snippet && " · "}{snippet}</span>}
          </div>
        )}
        {(pill || workflow || waitingOn || snoozed) && (
          <div className="row-status">
            {pill}
            {workflow && <WorkflowPill status={workflow} />}
            {waitingOn}
            {snoozed}
          </div>
        )}
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

/**
 * A stack's row: the shared entity, how many, and the newest one. Its
 * actions apply to every member.
 */
function StackRow({ stack: st, selected, onSelect, onAct }: {
  stack: Stack;
  selected: boolean;
  onSelect: () => void;
  onAct: (a: ItemAction) => void;
}) {
  const { kind } = useApp();
  const k = kind(st.kind);
  const [menu, setMenu] = useState(false);
  return (
    <div id={`row-${st.id}`} role="option" aria-selected={selected} className={cx("row", "stack-row", selected && "selected", menu && "menu-open")} onClick={onSelect}>
      <span className="row-glyph stack-glyph">
        <SourceGlyph kind={st.kind} source={st.newest.source} icon={k.icon} tone={k.tone} size={28} />
      </span>
      <div className="row-main">
        <div className="row-head">
          <div className="row-title">{st.label} <span className="stack-count">{st.items.length} {plural(k.label, st.items.length)}</span></div>
          <span className="row-time">{timeAgo(st.newest.occurred_at)}</span>
        </div>
        <div className="row-sub"><span className="row-sub-text">{st.newest.title}</span></div>
      </div>
      <div className="row-actions" onClick={(e) => e.stopPropagation()}>
        <button className={cx("row-action", menu && "active")} title="Snooze all" onClick={() => setMenu((v) => !v)}>
          <Icon name="clock" size={15} />
        </button>
        <button className="row-action" title="All done" onClick={() => onAct("done")}>
          <Icon name="check" size={15} />
        </button>
        {menu && <SnoozeMenu onPick={(m) => (setMenu(false), onAct({ snooze: m }))} onClose={() => setMenu(false)} />}
      </div>
    </div>
  );
}

/** A dropdown in the queue's toolbar: a quiet button naming the choice, and a menu of the rest. */
function BarMenu<T extends string | null>({ label, title, value, onChange, options, align }: {
  label: React.ReactNode;
  title: string;
  value: T;
  onChange: (v: T) => void;
  options: { value: T; label: string; icon?: React.ReactNode; count?: number }[];
  align?: "right";
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => (window.removeEventListener("mousedown", onDown), window.removeEventListener("keydown", onKey));
  }, [open]);
  return (
    <span className="menu-anchor bar-menu" ref={ref}>
      <button className={cx("bar-trigger", open && "active")} title={title} aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        {label}
        <Icon name="chevron-down" size={11} />
      </button>
      {open && (
        <div className={cx("menu", align === "right" && "right")} role="menu">
          {options.map((o) => (
            <button key={o.value ?? ""} role="menuitemradio" aria-checked={o.value === value} className={cx(o.value === value && "active")} onClick={() => (onChange(o.value), setOpen(false))}>
              <span className="bar-option">{o.icon}{o.label}{o.count !== undefined && <span className="count">{o.count}</span>}</span>
              {o.value === value && <Icon name="check" size={13} />}
            </button>
          ))}
        </div>
      )}
    </span>
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
