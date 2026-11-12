// Claude Code's interactive commands (/mcp, /model, /context…) only work in
// its terminal UI; tab runs it headless. The composer catches these and
// answers them itself: a card above the box, a picker, or a Settings page.

import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, StatusDot, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api, onMcpSignIn, openUrl } from "../../lib/api";
import { keys } from "../../lib/query";
import type { Chat, McpServerInfo, McpTool, Tone, ToolPolicy } from "../../lib/types";
import { openConnection } from "../connections/ConnectionsTab";
import { policies, useMcpServers } from "../connections/RemoteMcp";
import { openSettings } from "../settings/SettingsTab";
import { ContextDetails, CostDetails } from "./ContextMeter";

export type Panel = "mcp" | "context" | "cost" | "clear" | "resume" | "rewind";

/** What a tab command does: show a card, open a composer picker, or go somewhere. */
type Action = { panel: Panel } | { picker: "model" | "mode" } | { go: () => void };

export const tabCommands: Record<string, { description: string; action: Action }> = {
  mcp: { description: "MCP servers, their status and tools", action: { panel: "mcp" } },
  context: { description: "What's using the context window", action: { panel: "context" } },
  cost: { description: "This chat's cost and agent time", action: { panel: "cost" } },
  model: { description: "Pick the model for this chat", action: { picker: "model" } },
  permissions: { description: "Pick the permission mode", action: { picker: "mode" } },
  config: { description: "Open Settings", action: { go: () => openSettings("agents") } },
  login: { description: "Sign the agent in (Settings → Agents)", action: { go: () => openSettings("agents") } },
  logout: { description: "Sign the agent out (Settings → Agents)", action: { go: () => openSettings("agents") } },
  upgrade: { description: "Update the agent (Settings → Updates)", action: { go: () => openSettings("updates") } },
  clear: { description: "Start over with a fresh context", action: { panel: "clear" } },
  resume: { description: "Go back to an earlier chat", action: { panel: "resume" } },
  rewind: { description: "Go back to an earlier point in this chat", action: { panel: "rewind" } },
};

/** The tab command a message is, if it's one on its own (`/mcp`, not `/mcp foo`). */
export function tabCommand(text: string) {
  const m = /^\/([\w-]+)$/.exec(text.trim());
  return m ? tabCommands[m[1].toLowerCase()] : undefined;
}

const titles: Record<Panel, string> = {
  mcp: "MCP servers",
  context: "Context",
  cost: "Cost",
  clear: "Clear",
  resume: "Resume",
  rewind: "Rewind",
};

/** A tab command's card, above the composer box. */
export function SlashPanel({ panel, chat, onClose, onSend }: { panel: Panel; chat: Chat | undefined; onClose: () => void; onSend: (text: string) => void }) {
  const messages = chat?.messages ?? [];
  useEffect(() => {
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", esc);
    return () => window.removeEventListener("keydown", esc);
  }, [onClose]);
  return (
    <div className={cx("slash-card", `slash-card-${panel}`)} role="region" aria-label={titles[panel]}>
      <div className="slash-card-head">
        <strong>/{panel}</strong>
        <Badge>tab</Badge>
        <button className="bar-icon slash-card-close" onClick={onClose} title="Close (Esc)" aria-label="Close"><Icon name="x" size={13} /></button>
      </div>
      <div className="slash-card-body">
        {panel === "mcp" && <McpPanel chat={chat} />}
        {panel === "context" && <ContextDetails messages={messages} />}
        {panel === "cost" && <CostDetails messages={messages} />}
        {panel === "clear" && (
          <>
            <p>tab keeps one chat per task, so it can't empty this one yet. <code>/compact</code> summarises the conversation so far and frees most of the context.</p>
            <div className="slash-card-actions">
              <Button size="sm" variant="primary" disabled={!chat?.session || chat.running} onClick={() => (onClose(), onSend("/compact"))}>Compact this chat</Button>
            </div>
          </>
        )}
        {panel === "resume" && <p>Each task keeps its own chat: open the task in your queue to pick it back up.</p>}
        {panel === "rewind" && <p>Rewinding to a checkpoint isn't in tab yet.</p>}
      </div>
    </div>
  );
}

// What Claude Code's init line says about each server.
const agentStatus: Record<string, { tone: Tone; text: string }> = {
  connected: { tone: "success", text: "Connected" },
  failed: { tone: "danger", text: "Failed to start" },
  "needs-auth": { tone: "warning", text: "Needs a sign-in" },
  pending: { tone: "neutral", text: "Starting" },
};

type Row =
  | { kind: "tab"; name: string; agent?: string | null }
  | { kind: "connection"; server: McpServerInfo; agent?: string | null }
  | { kind: "agent"; name: string; agent: string | null };

/**
 * Every MCP server this chat can reach: tab's own, tab's connections
 * (reached through tab at /mcp/<id>, with their tools' allow/ask/off), and
 * any the agent loads from its own config, with the status it reported when
 * the last turn started.
 */
function McpPanel({ chat }: { chat: Chat | undefined }) {
  const session = chat?.session?.id;
  const { data: servers = [], isLoading } = useMcpServers();
  const { data: seen = [] } = useQuery({
    queryKey: ["agent-mcp", session],
    queryFn: () => api.agentMcpServers(session!),
    enabled: !!session,
    staleTime: 0,
  });
  const [open, setOpen] = useState<string | null>(null);
  const status = (name: string) => seen.find((s) => s.name === name)?.status;
  const rows: Row[] = [
    { kind: "tab", name: "tab", agent: status("tab") },
    // A connection missing from what the agent reported wasn't given to this chat.
    ...servers.map((s): Row => ({ kind: "connection", server: s, agent: seen.length && !seen.some((x) => x.name === s.id) ? "absent" : status(s.id) })),
    ...seen
      .filter((s) => s.name !== "tab" && !servers.some((x) => x.id === s.name))
      .map((s): Row => ({ kind: "agent", name: s.name, agent: s.status })),
  ];
  return (
    <>
      <div className="slash-mcp">
        {rows.map((r) => {
          const id = r.kind === "connection" ? r.server.id : r.name;
          return <McpRow key={`${r.kind}:${id}`} row={r} open={open === id} onToggle={() => setOpen(open === id ? null : id)} />;
        })}
        {isLoading && <div className="subtle">Loading…</div>}
      </div>
      <p className="subtle">
        {session ? "Status is from the start of the last turn; the agent reconnects to each server at every message." : "The agent connects to these when the chat starts."}{" "}
        <button className="link-btn" onClick={() => openConnection()}>Connections</button>
      </p>
    </>
  );
}

function connectionStatus(s: McpServerInfo, agent: string | null | undefined): { tone: Tone; text: string } {
  if (!s.enabled) return { tone: "neutral", text: "Off" };
  if (s.error) return { tone: "danger", text: s.error };
  if (!s.ready) return { tone: "warning", text: "Needs you to sign in" };
  if (agent === "absent") return { tone: "neutral", text: "Not in this chat" };
  if (agent && agent !== "connected") return agentStatus[agent] ?? { tone: "neutral", text: agent };
  return { tone: "success", text: "Connected" };
}

function McpRow({ row, open, onToggle }: { row: Row; open: boolean; onToggle: () => void }) {
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const s = row.kind === "connection" ? row.server : null;
  useEffect(() => {
    if (!s) return;
    let off: (() => void) | undefined;
    void onMcpSignIn((e) => {
      if (e.id !== s.id) return;
      void qc.invalidateQueries({ queryKey: keys.mcpServers });
      void qc.invalidateQueries({ queryKey: keys.mcpTools(s.id) });
      if (!e.ok) setError(e.error ?? "The sign-in didn't finish.");
    }).then((u) => (off = u));
    return () => off?.();
  }, [s?.id, qc]);

  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const st = s
    ? connectionStatus(s, row.agent)
    : row.agent
      ? agentStatus[row.agent] ?? { tone: "neutral" as Tone, text: row.agent }
      : { tone: "success" as Tone, text: row.kind === "tab" ? "Built in" : "Loaded" };
  const name = row.kind === "connection" ? row.server.name : row.name;
  const where = row.kind === "tab" ? "Your queue, tasks and context" : row.kind === "agent" ? "From the agent's own config" : s?.tool_count != null ? `${s.tool_count} tools` : "Tools";
  return (
    <div className={cx("slash-mcp-row", open && "open")}>
      <div className="slash-mcp-line">
        <button className="slash-mcp-name" onClick={s?.ready ? onToggle : undefined} disabled={!s?.ready} aria-expanded={s?.ready ? open : undefined}>
          {s?.ready && <Icon name={open ? "chevron-down" : "chevron-right"} size={12} />}
          <strong>{name}</strong>
          <small className="subtle">{where}</small>
        </button>
        <span className="slash-mcp-status"><StatusDot tone={st.tone} /> {st.text}</span>
        {s && s.enabled && !s.ready && s.sign_in && <Button size="sm" disabled={busy} onClick={() => void run(async () => openUrl(await api.mcpSignIn(s.id)))}>Sign in</Button>}
        {s && s.ready && (
          <Button size="sm" variant="ghost" icon="refresh" disabled={busy} title="Reconnect and fetch its tools again"
            onClick={() => void run(async () => (qc.setQueryData(keys.mcpTools(s.id), await api.mcpTools(s.id, true)), qc.invalidateQueries({ queryKey: keys.mcpServers })))}>Reconnect</Button>
        )}
        {s && <Button size="sm" variant="ghost" onClick={() => openConnection(`mcp:${s.id}`)}>Open</Button>}
      </div>
      {error && <div className="chat-error">{error}</div>}
      {open && s && <McpTools id={s.id} />}
    </div>
  );
}

/** A connection's tools, each allowed, asked or off in chats. */
function McpTools({ id }: { id: string }) {
  const qc = useQueryClient();
  const tools = useQuery({ queryKey: keys.mcpTools(id), queryFn: () => api.mcpTools(id, false), staleTime: 0, retry: false });
  const [busy, setBusy] = useState(false);
  const setTool = async (t: McpTool, p: ToolPolicy) => {
    setBusy(true);
    try {
      // Back to the default clears the choice, so it follows the tool's kind.
      await api.mcpSetTool(id, t.name, p === (t.read_only ? "allow" : "ask") ? null : p);
      await qc.invalidateQueries({ queryKey: keys.mcpTools(id) });
    } finally {
      setBusy(false);
    }
  };
  if (tools.isLoading) return <div className="subtle slash-mcp-tools">Loading tools…</div>;
  if (tools.error) return <div className="chat-error">{String(tools.error)}</div>;
  return (
    <div className="slash-mcp-tools">
      {(tools.data ?? []).map((t) => (
        <div key={t.name} className="mcp-tool">
          <div className="mcp-tool-main">
            <code>{t.name}</code>
            {t.description && <span className="subtle mcp-tool-desc" title={t.description}>{t.description}</span>}
          </div>
          <div className="seg" role="radiogroup" aria-label={`${t.name}: allowed, ask or off`}>
            {policies.map((p) => (
              <button key={p.value} role="radio" aria-checked={t.policy === p.value} className={cx(t.policy === p.value && "on")} disabled={busy} onClick={() => void setTool(t, p.value)}>
                {p.label}
              </button>
            ))}
          </div>
        </div>
      ))}
      {!tools.data?.length && <div className="subtle">No tools.</div>}
    </div>
  );
}
