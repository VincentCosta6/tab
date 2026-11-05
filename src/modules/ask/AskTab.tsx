import { useCallback, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import { useProjects } from "../../lib/hooks";
import type { Chat, Entity, GraphNode } from "../../lib/types";
import { AgentStatus, Composer, Transcript, openItem } from "../work/Chat";
import { ProjectPicker, useProjectTint, useRememberedProject } from "../work/Projects";
import { GraphCanvas } from "./GraphCanvas";

const kindColor: Record<string, string> = {
  person: "#3b82f6",
  repo: "#8b5cf6",
  pull_request: "#10b981",
  issue: "#f59e0b",
  ticket: "#f59e0b",
  channel: "#ec4899",
  email: "#06b6d4",
  meeting: "#f97316",
};
const kindLabel = (k: string) => ({ pull_request: "PRs", person: "People", repo: "Repos", issue: "Issues", project: "Projects" })[k] ?? k.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase()) + "s";

const windows = [
  { days: 7, label: "7d" },
  { days: 30, label: "30d" },
  { days: 90, label: "90d" },
  { days: null, label: "All" },
] as const;

/**
 * Ask: the knowledge graph, filterable by project and time, with a chat at
 * the bottom about what it shows. Selecting a node shows what it touches,
 * and the chat gets that node as its focus.
 */
export function AskTab() {
  const qc = useQueryClient();
  const { data: projects = [] } = useProjects();
  // The same project as the queue, so the two pages agree.
  const [project, setProject] = useRememberedProject("tab.queueProject");
  const [days, setDays] = useState<number | null>(30);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<number | null>(null);
  const [hidden, setHidden] = useState<Set<string>>(new Set());
  const [chatOpen, setChatOpen] = useState(true);
  const tint = useProjectTint(project);

  const { data: graph } = useQuery({
    queryKey: ["graph", project ?? "", days ?? 0],
    queryFn: () => api.graphView(project, days, 200),
    staleTime: 0,
    placeholderData: (prev) => prev,
  });
  const { data: node } = useQuery({ queryKey: ["graph-node", selected], queryFn: () => api.graphNode(selected!), enabled: selected != null, staleTime: 0 });

  const projectColor = useMemo(() => new Map(projects.map((p) => [`project:${p.id}`, p.color])), [projects]);
  const colorOf = useCallback((e: Entity) => (e.kind === "project" ? projectColor.get(e.key) ?? "#8b8d98" : kindColor[e.kind] ?? "#8b8d98"), [projectColor]);

  const kinds = useMemo(() => {
    const c = new Map<string, number>();
    for (const n of graph?.nodes ?? []) c.set(n.kind, (c.get(n.kind) ?? 0) + 1);
    return [...c].sort((a, b) => b[1] - a[1]);
  }, [graph]);

  const shown = useMemo(() => {
    if (!graph) return { nodes: [], edges: [] };
    const nodes = graph.nodes.filter((n) => !hidden.has(n.kind));
    const ids = new Set(nodes.map((n) => n.id));
    return { nodes, edges: graph.edges.filter((e) => ids.has(e.src) && ids.has(e.dst)) };
  }, [graph, hidden]);

  const highlight = useMemo(() => {
    const q = query.trim().toLowerCase();
    return new Set(q ? shown.nodes.filter((n) => n.label.toLowerCase().includes(q)).map((n) => n.id) : []);
  }, [query, shown]);

  // One ongoing chat per project (or for everything).
  const chatKey = ["chat", `ask:${project ?? "all"}`];
  const { data: chat } = useQuery<Chat>({ queryKey: chatKey, queryFn: () => api.askChatOpen(project), staleTime: 0, refetchOnMount: "always" });
  const focus = node && selected != null ? [`${node.entity.label} (${node.entity.kind.replace(/_/g, " ")})`] : [];
  const send = async (text: string) => {
    setChatOpen(true);
    qc.setQueryData<Chat>(chatKey, (c) => ({
      session: c?.session ?? null,
      approvals: c?.approvals ?? [],
      running: true,
      messages: [...(c?.messages ?? []), { id: -Date.now(), session_id: c?.session?.id ?? "", role: "user", kind: "text", content: text, meta: null, created_at: new Date().toISOString() }],
    }));
    try {
      const session = await api.askChatSend(project, text, focus);
      qc.setQueryData<Chat>(chatKey, (c) => ({ session, approvals: c?.approvals ?? [], running: true, messages: (c?.messages ?? []).filter((m) => m.id > 0) }));
    } finally {
      qc.invalidateQueries({ queryKey: chatKey });
    }
  };

  const suggestions = selected != null && node
    ? [`What's going on with ${node.entity.label}?`, `Who works most with ${node.entity.label}?`, `What's open that involves ${node.entity.label}?`]
    : ["What changed this week?", "Which threads need me most?", "What is this project about?"];

  return (
    <div className={cx("ask", tint.className)} style={tint.style}>
      <header className="ask-bar">
        <ProjectPicker value={project} onChange={(p) => (setProject(p), setSelected(null))} none="All projects" />
        <div className="seg" role="group" aria-label="Time window">
          {windows.map((w) => (
            <button key={w.label} className={cx("seg-btn", days === w.days && "active")} onClick={() => setDays(w.days)}>{w.label}</button>
          ))}
        </div>
        <label className="ask-search">
          <Icon name="search" size={13} />
          <input placeholder="Find a node" value={query} onChange={(e) => setQuery(e.target.value)} onKeyDown={(e) => e.key === "Enter" && highlight.size && setSelected([...highlight][0])} />
        </label>
        <div className="ask-kinds">
          {kinds.map(([k, n]) => (
            <button key={k} className={cx("kind-chip", hidden.has(k) && "off")} onClick={() => setHidden((h) => { const x = new Set(h); if (x.has(k)) x.delete(k); else x.add(k); return x; })}>
              <span className="kind-dot" style={{ background: k === "project" ? "var(--fg-muted)" : kindColor[k] ?? "#8b8d98", borderRadius: k === "project" ? 3 : undefined }} />
              {kindLabel(k)} <span className="count">{n}</span>
            </button>
          ))}
        </div>
      </header>
      <div className="ask-main">
        <div className="ask-graph">
          {graph && !graph.nodes.length ? (
            <div className="ask-empty">
              <Icon name="graph" size={28} />
              <p>Nothing in the graph {project ? "for this project" : ""}{days ? ` in the last ${days} days` : ""} yet.</p>
              <p className="subtle">Connections add people, repos, PRs and threads as items arrive.</p>
            </div>
          ) : (
            <GraphCanvas graph={shown} selected={selected} highlight={highlight} colorOf={colorOf} onSelect={setSelected} />
          )}
        </div>
        {selected != null && node && <NodePanel node={node} color={colorOf(node.entity)} onSelect={setSelected} onClose={() => setSelected(null)} inGraph={(id) => shown.nodes.some((n) => n.id === id)} />}
      </div>
      <section className={cx("ask-chat", chatOpen && "open")}>
        <button className="ask-chat-toggle" onClick={() => setChatOpen((v) => !v)} aria-expanded={chatOpen}>
          <Icon name="message" size={13} /> Chat
          {chat?.session && <AgentStatus chat={chat} />}
          <Icon name="chevron-down" size={12} className="ask-chat-chevron" />
        </button>
        {chatOpen && (chat?.messages.length ?? 0) > 0 && (
          <div className="ask-transcript"><Transcript chat={chat} onSend={send} /></div>
        )}
        {focus.length > 0 && <div className="ask-focus"><Icon name="dot" size={10} /> Asking about <strong>{focus[0]}</strong></div>}
        {!(chat?.messages.length) && (
          <div className="suggestions ask-suggestions">
            {suggestions.map((s) => <button key={s} className="suggestion" onClick={() => send(s)}><Icon name="zap" size={13} /> {s}</button>)}
          </div>
        )}
        <Composer chat={chat} onSend={send} placeholder={project ? "Ask about this project…" : "Ask about your work…"} />
      </section>
    </div>
  );
}

function NodePanel({ node, color, onSelect, onClose, inGraph }: { node: GraphNode; color: string; onSelect: (id: number) => void; onClose: () => void; inGraph: (id: number) => boolean }) {
  const e = node.entity;
  return (
    <aside className="ask-node">
      <header>
        <span className="kind-dot big" style={{ background: color, borderRadius: e.kind === "project" ? 4 : undefined }} />
        <div className="ask-node-title">
          <strong title={e.label}>{e.label}</strong>
          <span className="subtle">{e.kind.replace(/_/g, " ")} · {e.item_count} item{e.item_count === 1 ? "" : "s"}</span>
        </div>
        <button className="icon-btn" onClick={onClose} aria-label="Close"><Icon name="x" size={14} /></button>
      </header>
      {node.notes.length > 0 && (
        <section>
          <h4 className="label-sm">Notes</h4>
          {node.notes.map((n) => <p key={n.id} className="ask-note"><strong>{n.topic}</strong> {n.text}</p>)}
        </section>
      )}
      {node.connected.length > 0 && (
        <section>
          <h4 className="label-sm">Connected</h4>
          <div className="ask-connected">
            {node.connected.map(({ entity: c, shared }) => (
              <button key={c.id} className="kind-chip" disabled={!inGraph(c.id)} onClick={() => onSelect(c.id)} title={`${shared} shared item${shared === 1 ? "" : "s"}`}>
                {c.label} <span className="count">{shared}</span>
              </button>
            ))}
          </div>
        </section>
      )}
      {node.items.length > 0 && (
        <section>
          <h4 className="label-sm">Recent</h4>
          <div className="ask-items">
            {node.items.map((i) => (
              <button key={i.id} className="ask-item" onClick={() => openItem(i.id)}>
                <span className="ask-item-title">{i.title}</span>
                <span className="subtle">{[i.actor, timeAgo(i.occurred_at)].filter(Boolean).join(" · ")}</span>
              </button>
            ))}
          </div>
        </section>
      )}
    </aside>
  );
}
