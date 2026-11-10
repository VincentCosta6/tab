// Remote MCP connections in the Connections tab: catalog cards, Add by
// URL, the sign-in wait, and each connection's page with its tools.
//
// Users see what a connection does ("Fills your queue", "Agent tools"),
// not how it's built; the word MCP only shows where people add a server
// by hand.

import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, Card, Section, StatusDot, TextInput, Toggle, cx } from "../../design/components";
import { Brand, hasBrand } from "../../design/brands";
import { Icon } from "../../design/icons";
import { api, onMcpSignIn, openUrl } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import { keys } from "../../lib/query";
import type { McpCatalogEntry, McpProbe, McpQueueState, McpServerInfo, McpTool, Tone, ToolPolicy } from "../../lib/types";
import { WhereItRuns } from "./ClaudeImport";

export function useMcpServers() {
  return useQuery({ queryKey: keys.mcpServers, queryFn: api.mcpServers, staleTime: 0 });
}

export function useMcpCatalog() {
  return useQuery({ queryKey: keys.mcpCatalog, queryFn: api.mcpCatalog, staleTime: Infinity });
}

/** What a connection does, as two chips: lit when it does it. */
export function Caps({ queue, tools }: { queue: boolean | "soon"; tools: boolean }) {
  return (
    <div className="caps">
      <span className={cx("cap", queue === true && "on")} title="Work from it shows up in your queue.">
        {queue === true && <Icon name="check" size={11} />} Fills your queue{queue === "soon" ? " · soon" : ""}
      </span>
      <span className={cx("cap", tools && "on")} title="Agents in your chats can act there as you.">
        {tools && <Icon name="check" size={11} />} Agent tools
      </span>
    </div>
  );
}

const brandFor: Record<string, string> = { linear: "linear", notion: "notion", atlassian: "jira", sentry: "sentry" };
/** Whose sign-in page it is, where that isn't the card's name. */
const signInWith: Record<string, string> = { atlassian: "Atlassian" };

export function McpMark({ name, catalog, color, size = 34 }: { name: string; catalog?: string | null; color?: string; size?: number }) {
  const brand = catalog ? brandFor[catalog] : undefined;
  if (brand && hasBrand(brand)) {
    return (
      <span className="source-glyph branded" style={{ width: size, height: size }}>
        <Brand name={brand} size={Math.round(size * 0.58)} />
      </span>
    );
  }
  return (
    <span className="mcp-mark" style={{ width: size, height: size, fontSize: Math.round(size * 0.44), background: color ?? "var(--fg-muted)" }}>
      {(name.trim()[0] ?? "?").toUpperCase()}
    </span>
  );
}

function statusOf(s: McpServerInfo): { tone: Tone; text: string } {
  if (!s.enabled) return { tone: "neutral", text: "Off" };
  if (s.error) return { tone: "danger", text: s.error };
  if (!s.ready) return { tone: "warning", text: "Needs you to sign in" };
  if (s.signed_in_at) return { tone: "success", text: `Signed in ${timeAgo(s.signed_in_at)}` };
  return { tone: "success", text: "Connected" };
}

export function McpInstalledRow({ s, color, onOpen }: { s: McpServerInfo; color?: string; onOpen: () => void }) {
  const st = statusOf(s);
  return (
    <button className="connector-row" onClick={onOpen}>
      <McpMark name={s.name} catalog={s.catalog} color={color} />
      <div className="connector-row-main">
        <div className="connector-name">{s.name}</div>
        <div className="connector-status"><StatusDot tone={st.tone} /> <span>{st.text}</span></div>
      </div>
      <div className="connector-row-kinds">
        {s.queue_sync?.on && <Badge tone={s.queue_sync.error ? "danger" : "neutral"}>{s.queue_sync.error ? "Queue sync failed" : "Fills your queue"}</Badge>}
        <Badge>{s.tool_count != null ? `${s.tool_count} agent tools` : "Agent tools"}</Badge>
      </div>
      <Icon name="chevron-right" size={16} />
    </button>
  );
}

export function McpCatalogCard({ entry, onConnect }: { entry: McpCatalogEntry; onConnect: () => void }) {
  return (
    <Card className="connector">
      <div className="connector-head">
        <McpMark name={entry.name} catalog={entry.id} color={entry.color} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <div className="connector-name">{entry.name}</div>
          <div className="subtle">Sign in with {signInWith[entry.id] ?? entry.name}</div>
        </div>
        <Button size="sm" variant="primary" onClick={onConnect}>Connect</Button>
      </div>
      <p className="connector-desc">{entry.description}</p>
      <Caps queue={entry.fills_queue} tools />
    </Card>
  );
}

/**
 * Connecting: adds the server, opens the sign-in page, and waits for the
 * browser to come back. `onDone` gets the new connection's id.
 */
export function useConnect(onDone: (id: string) => void) {
  const qc = useQueryClient();
  const [waiting, setWaiting] = useState<{ id: string; name: string; url: string; catalog?: string | null; color?: string } | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!waiting) return;
    let off: (() => void) | undefined;
    void onMcpSignIn((e) => {
      if (e.id !== waiting.id) return;
      void qc.invalidateQueries({ queryKey: keys.mcpServers });
      if (e.ok) {
        setWaiting(null);
        onDone(e.id);
      } else setError(e.error ?? "The sign-in didn't finish.");
    }).then((u) => (off = u));
    return () => off?.();
  }, [waiting, qc, onDone]);

  const add = async (a: { id: string; name: string; url: string; signIn: boolean; catalog: string | null; color?: string }) => {
    setError(null);
    const page = await api.mcpAdd(a);
    await qc.invalidateQueries({ queryKey: keys.mcpServers });
    if (!page) {
      onDone(a.id);
      return;
    }
    setWaiting({ id: a.id, name: a.name, url: page, catalog: a.catalog, color: a.color });
    await openUrl(page);
  };

  const again = async () => {
    if (!waiting) return;
    setError(null);
    const page = await api.mcpSignIn(waiting.id);
    setWaiting({ ...waiting, url: page });
    await openUrl(page);
  };

  const dialog = waiting && (
    <div className="mcp-overlay" role="dialog" aria-label={`Connecting ${waiting.name}`}>
      <div className="mcp-dialog">
        <div className="mcp-dialog-head">
          <McpMark name={waiting.name} catalog={waiting.catalog} color={waiting.color} />
          <div>
            <div className="mcp-dialog-title">Connecting {waiting.name}</div>
            <div className="subtle">Finish signing in in your browser</div>
          </div>
        </div>
        <div className="mcp-steps">
          <div className="done"><Icon name="check" size={12} /> Found {waiting.name}'s sign-in page</div>
          <div className="done"><Icon name="check" size={12} /> Registered tab with {waiting.name}, no setup needed</div>
          <div className={cx(error ? "failed" : "now")}><span className="mcp-spin" /> {error ?? "Waiting for you to approve in the browser…"}</div>
        </div>
        <p className="mcp-note">
          Agents in your chats will be able to use {waiting.name}'s tools as you. Anything that changes something asks you first. Disconnect anytime in Connections.
        </p>
        <div className="mcp-dialog-foot">
          <Button variant="ghost" size="sm" onClick={() => { setWaiting(null); setError(null); }}>Close</Button>
          <Button size="sm" onClick={() => void again()}>Open the browser again</Button>
        </div>
      </div>
    </div>
  );

  return { add, dialog, error: waiting ? null : error, setError };
}

/** The dashed "Add a custom MCP server" button and its dialog. */
export function AddByUrl({ onAdd }: { onAdd: (a: { id: string; name: string; url: string; signIn: boolean; catalog: string | null }) => Promise<void> }) {
  const [open, setOpen] = useState(false);
  const [url, setUrl] = useState("");
  const [probe, setProbe] = useState<McpProbe | null>(null);
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const close = () => {
    setOpen(false);
    setUrl("");
    setProbe(null);
    setName("");
    setError(null);
  };
  const check = async () => {
    setBusy(true);
    setError(null);
    setProbe(null);
    try {
      const p = await api.mcpProbe(url);
      setProbe(p);
      setName(p.name ?? p.id);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const connect = async () => {
    if (!probe) return;
    setBusy(true);
    setError(null);
    try {
      await onAdd({ id: probe.id, name: name.trim() || probe.id, url: probe.url, signIn: probe.sign_in, catalog: null });
      close();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <button className="connector-add" onClick={() => setOpen(true)}>
        <Icon name="link" size={15} /> Add a custom MCP server by URL
      </button>
      {open && (
        <div className="mcp-overlay" role="dialog" aria-label="Add an MCP server">
          <form className="mcp-dialog" onSubmit={(e) => (e.preventDefault(), void (probe ? connect() : check()))}>
            <div className="mcp-dialog-title">Add an MCP server by URL</div>
            <label className="mcp-field">
              Server URL
              <span className="input">
                <input autoFocus value={url} placeholder="https://example.com/mcp" spellCheck={false} onChange={(e) => (setUrl(e.target.value), setProbe(null))} />
              </span>
            </label>
            {probe && (
              <div className="mcp-probe">
                <div className="mcp-probe-head">
                  <StatusDot tone="success" />
                  <span className="input mcp-name">
                    <input value={name} aria-label="Name" onChange={(e) => setName(e.target.value)} />
                  </span>
                  <Badge>{probe.sign_in ? "Sign in with OAuth" : "No sign-in needed"}</Badge>
                </div>
                <div className="subtle">
                  Agents will see it as <code>{probe.id}</code>. tab doesn't know how to fill your queue from it, so it connects as agent tools only.
                </div>
              </div>
            )}
            {error && <p className="chat-error">{error}</p>}
            <div className="mcp-dialog-foot">
              <Button type="button" variant="ghost" size="sm" onClick={close}>Cancel</Button>
              <Button type="submit" variant="primary" size="sm" disabled={busy || !url.trim()}>
                {busy ? "Checking…" : probe ? (probe.sign_in ? "Sign in and connect" : "Connect") : "Check"}
              </Button>
            </div>
          </form>
        </div>
      )}
    </>
  );
}

const policies: { value: ToolPolicy; label: string }[] = [
  { value: "allow", label: "Allowed" },
  { value: "ask", label: "Ask" },
  { value: "off", label: "Off" },
];

/** One remote MCP connection: status, sign-in, and a choice per tool. */
export function McpDetail({ id, color, onBack }: { id: string; color?: string; onBack: () => void }) {
  const qc = useQueryClient();
  const { data: servers } = useMcpServers();
  const s = servers?.find((x) => x.id === id);
  const tools = useQuery({ queryKey: keys.mcpTools(id), queryFn: () => api.mcpTools(id, false), enabled: !!s?.ready, staleTime: 0, retry: false });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showAll, setShowAll] = useState(false);
  const { data: catalog = [] } = useMcpCatalog();
  const catalogEntry = catalog.find((e) => e.id === s?.catalog);

  useEffect(() => {
    let off: (() => void) | undefined;
    void onMcpSignIn((e) => {
      if (e.id !== id) return;
      void qc.invalidateQueries({ queryKey: keys.mcpServers });
      void qc.invalidateQueries({ queryKey: keys.mcpTools(id) });
      if (!e.ok) setError(e.error ?? "The sign-in didn't finish.");
    }).then((u) => (off = u));
    return () => off?.();
  }, [id, qc]);

  if (!s) {
    return (
      <div className="page page-narrow">
        <Button size="sm" variant="ghost" icon="arrow-left" onClick={onBack}>Connections</Button>
        <p className="subtle">This connection is gone.</p>
      </div>
    );
  }
  const st = statusOf(s);
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
      await qc.invalidateQueries({ queryKey: keys.mcpServers });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const signIn = () => run(async () => openUrl(await api.mcpSignIn(id)));
  const setTool = (t: McpTool, p: ToolPolicy) =>
    run(async () => {
      // Back to the default clears the choice, so it follows the tool's kind.
      const def: ToolPolicy = t.read_only ? "allow" : "ask";
      await api.mcpSetTool(id, t.name, p === def ? null : p);
      await qc.invalidateQueries({ queryKey: keys.mcpTools(id) });
    });
  const list = tools.data ?? [];
  const shown = showAll ? list : list.slice(0, 12);

  return (
    <div className="page page-narrow">
      <Button size="sm" variant="ghost" icon="arrow-left" onClick={onBack}>Connections</Button>
      <header className="connector-detail-head">
        <McpMark name={s.name} catalog={s.catalog} color={color} size={48} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <h1 className="page-title">{s.name}</h1>
          <div className="connector-status"><StatusDot tone={st.tone} /> <span>{st.text}</span></div>
        </div>
        {s.sign_in && (s.ready ? (
          <Button size="sm" disabled={busy} onClick={() => void signIn()}>Reconnect</Button>
        ) : (
          <Button size="sm" variant="primary" disabled={busy} onClick={() => void signIn()}>Sign in</Button>
        ))}
        <Button size="sm" disabled={busy} onClick={() => void run(() => api.mcpSetEnabled(id, !s.enabled))}>{s.enabled ? "Turn off" : "Turn on"}</Button>
        <Button
          size="sm"
          variant="danger"
          disabled={busy}
          onClick={() => void run(async () => {
            await api.mcpRemove(id);
            onBack();
          })}
        >
          Disconnect
        </Button>
      </header>
      <div style={{ marginBottom: "var(--space-4)" }}><Caps queue={!!s.queue_sync?.on} tools /></div>
      <p className="connector-desc large">
        {s.command
          ? `Agents in your chats can use ${s.name}'s tools. tab runs it on this computer and passes each request on, so agents never start it or see its keys.`
          : `Agents in your chats can use ${s.name}'s tools as you. tab signs each request in, so the agents never hold your sign-in.`}{" "}
        Chats on someone else's code (pull request reviews, jobs) don't get them.
      </p>
      {error && <p className="chat-error">{error}</p>}

      <Section title="Connection">
        <Card>
          <dl className="mcp-kv">
            {s.command ? (
              <>
                <dt>Runs</dt><dd><code>{[s.command.command, ...(s.command.args ?? [])].join(" ")}</code></dd>
                {s.command.cwd && (<><dt>In</dt><dd><code>{s.command.cwd}</code></dd></>)}
              </>
            ) : (
              <><dt>Server</dt><dd><code>{s.url}</code></dd></>
            )}
            <dt>Agents see it as</dt><dd><code>{s.id}</code></dd>
            <dt>Sign-in</dt>
            <dd>
              {s.sign_in
                ? (s.signed_in_at ? `OAuth, no keys · renews on its own · signed in ${timeAgo(s.signed_in_at)}` : "OAuth, not signed in")
                : s.headers?.length || s.command?.env?.length
                  ? `Keys: ${[...(s.headers ?? []), ...(s.command?.env ?? [])].join(", ")}`
                  : "None needed"}
            </dd>
            <dt>Kept in</dt><dd>{s.sign_in || s.keys ? "Your system's secure storage, on this computer only" : "Nothing to keep"}</dd>
            <dt>Where it runs</dt><dd><WhereItRuns fit={s.cloud} /></dd>
            {s.imported_from && (<><dt>Came from</dt><dd>Claude Code (a copy: changes there show up in Connections to import again)</dd></>)}
          </dl>
        </Card>
      </Section>

      {s.queue_sync && (
        <QueueSection
          s={s}
          q={s.queue_sync}
          hint={catalogEntry?.queue_query}
          busy={busy}
          onChange={(on, query) => void run(() => api.mcpSetQueue(id, on, query))}
        />
      )}

      <Section
        title="Agent tools"
        description="Tools that only read run without asking. Anything that changes something asks you in the chat first, unless you allow it here. Off tools are hidden from agents."
        actions={s.ready ? <Button size="sm" variant="ghost" icon="undo" disabled={tools.isFetching} onClick={() => void api.mcpTools(id, true).then(() => qc.invalidateQueries({ queryKey: keys.mcpTools(id) }))}>Refresh</Button> : undefined}
      >
        <Card padded={false}>
          {!s.ready && <div className="mcp-tools-empty subtle">{s.enabled ? "Sign in to see its tools." : "Turn it on to see its tools."}</div>}
          {s.ready && tools.isLoading && <div className="mcp-tools-empty subtle">Asking {s.name} for its tools…</div>}
          {s.ready && tools.error && <div className="mcp-tools-empty chat-error">{String(tools.error)}</div>}
          {shown.map((t) => (
            <div key={t.name} className="mcp-tool">
              <div className="mcp-tool-main">
                <code>{t.name}</code>
                {t.description && <span className="subtle mcp-tool-desc" title={t.description}>{t.description}</span>}
              </div>
              <Badge tone={t.read_only ? "neutral" : "warning"}>{t.read_only ? "reads" : "changes"}</Badge>
              <div className="seg" role="radiogroup" aria-label={`${t.name}: allowed, ask or off`}>
                {policies.map((p) => (
                  <button key={p.value} role="radio" aria-checked={t.policy === p.value} className={cx(t.policy === p.value && "on")} disabled={busy} onClick={() => void setTool(t, p.value)}>
                    {p.label}
                  </button>
                ))}
              </div>
            </div>
          ))}
          {list.length > shown.length && (
            <button className="mcp-tools-more" onClick={() => setShowAll(true)}>{list.length - shown.length} more tools</button>
          )}
        </Card>
      </Section>
    </div>
  );
}

/** "Fills your queue" on a connection's page: on or off, how the last look went, and what it watches. */
function QueueSection({ s, q, hint, busy, onChange }: { s: McpServerInfo; q: McpQueueState; hint?: string; busy: boolean; onChange: (on: boolean, query: string | null) => void }) {
  const [query, setQuery] = useState(s.queue_query ?? "");
  useEffect(() => setQuery(s.queue_query ?? ""), [s.queue_query]);
  const what: Record<string, string> = {
    linear: "Issues assigned to you come into your queue. Issues that are done or cancelled in Linear are marked done here.",
    sentry: "Unresolved issues assigned to you come into your queue, from every organization you're in. Resolved or ignored issues are marked done here.",
    notion: "Pages matching a search come into your queue. Notion has no list of what's assigned to you, so tab runs a search you pick.",
  };
  const status = !q.on
    ? "Off"
    : q.error
      ? q.error
      : !s.ready
        ? "Starts once you're signed in"
        : q.last_sync
          ? `Looked ${timeAgo(q.last_sync)} · ${q.found ?? 0} open`
          : "Looking…";
  const tone: Tone = !q.on ? "neutral" : q.error ? "danger" : q.last_sync ? "success" : "warning";
  return (
    <Section title="Fills your queue" description={(s.catalog && what[s.catalog]) ?? "Work from it comes into your queue."}>
      <Card>
        <div className="mcp-queue">
          <div className="mcp-queue-row">
            <Toggle checked={q.on} label="Bring its work into your queue" onChange={(on) => onChange(on, query || null)} />
            <span>Bring its work into your queue</span>
            <div className="connector-status"><StatusDot tone={tone} /> <span>{status}</span></div>
          </div>
          {hint && q.on && (
            <form
              className="mcp-queue-query"
              onSubmit={(e) => {
                e.preventDefault();
                onChange(true, query.trim() || null);
              }}
            >
              <TextInput value={query} placeholder={hint} onChange={(e) => setQuery(e.target.value)} aria-label="What to watch" />
              <Button size="sm" type="submit" disabled={busy || query.trim() === (s.queue_query ?? "")}>Save</Button>
            </form>
          )}
          <p className="subtle mcp-queue-note">tab only reads to fill your queue, and checks every few minutes. Turning this off clears its items from your queue.</p>
        </div>
      </Card>
    </Section>
  );
}
