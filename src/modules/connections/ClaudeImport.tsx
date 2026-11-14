// Import from Claude Code: the banner in Connections and the dialog that
// turns the person's Claude Code MCP servers into tab connections, so strict
// chats (and later cloud runs) can use them. See docs/connect-via-mcp.md §4.
//
// Importing copies. Servers that sign in with OAuth are signed in to again
// in tab (Claude Code's tokens stay its own); keys go to secure storage;
// local commands run as tab's children.

import { useEffect, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, StatusDot, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api, onMcpSignIn, openUrl } from "../../lib/api";
import { keys } from "../../lib/query";
import type { McpCloudFit, McpImportCandidate, McpImportResult } from "../../lib/types";
import { McpMark } from "./RemoteMcp";

export function useClaudeImport() {
  return useQuery({ queryKey: keys.mcpImport, queryFn: api.mcpImportScan, staleTime: 30_000, retry: false });
}

const DISMISSED = "tab.mcpImport.dismissed";
const actionable = (c: McpImportCandidate) => c.status === "new" || c.status === "changed";

/** "You have 5 MCP servers set up in Claude Code…", until imported or dismissed. */
export function ImportBanner({ onReview }: { onReview: () => void }) {
  const { data = [] } = useClaudeImport();
  const todo = data.filter(actionable);
  const sig = todo.map((c) => c.key).sort().join("\n");
  const [dismissed, setDismissed] = useState(() => localStorage.getItem(DISMISSED) ?? "");
  // Dismissing hides these servers; new ones bring the banner back.
  if (!todo.length || todo.every((c) => dismissed.split("\n").includes(c.key))) return null;
  const changed = todo.filter((c) => c.status === "changed").length;
  const n = todo.length;
  return (
    <div className="import-banner" role="region" aria-label="Import from Claude Code">
      <span className="import-banner-icon"><Icon name="download" size={16} /></span>
      <div className="import-banner-text">
        <strong>
          {changed === n
            ? `${n} MCP server${n === 1 ? "" : "s"} changed in Claude Code`
            : `You have ${n} MCP server${n === 1 ? "" : "s"} set up in Claude Code`}
        </strong>
        <span>Strict chats and cloud runs can't see them until they're tab connections.</span>
      </div>
      <Button size="sm" variant="ghost" onClick={() => (localStorage.setItem(DISMISSED, sig), setDismissed(sig))}>Not now</Button>
      <Button size="sm" variant="primary" onClick={onReview}>Review and import</Button>
    </div>
  );
}

const sourceLabel = (c: McpImportCandidate) => {
  const folder = c.path?.split(/[\\/]/).filter(Boolean).pop();
  switch (c.source) {
    case "user": return "Claude Code · your servers";
    case "project": return `Claude Code · ${folder ?? "a project"}`;
    case "repo": return `.mcp.json in ${folder ?? "a project"}`;
    case "claude_ai": return "claude.ai connector";
  }
};

function howItImports(c: McpImportCandidate): string {
  switch (c.kind) {
    case "claude_ai": return "Becomes tab's connection. Sign in once.";
    case "remote": return "Sign in again in tab, if it asks.";
    case "remote_keys": return `${c.header_names.join(", ")} goes to your system's secure storage.`;
    case "local":
      return c.env_names.length
        ? `tab runs it. ${c.env_names.join(", ")} goes to secure storage.`
        : "tab runs it on this computer.";
  }
}

/** ✓ This computer ✓ Cloud, or why it can't run in the cloud. */
export function WhereItRuns({ fit }: { fit: McpCloudFit }) {
  return (
    <span className="where-runs">
      <span className="on"><Icon name="check" size={11} /> This computer</span>
      {fit.portable ? (
        <span className="on" title={fit.needs_key ? "Its key goes to tab cloud only if you allow it." : undefined}>
          <Icon name="check" size={11} /> Cloud{fit.needs_key ? " (with your OK)" : ""}
        </span>
      ) : (
        <span className="off" title={fit.why_not ?? undefined}><Icon name="x" size={11} /> Cloud{fit.why_not ? `: ${fit.why_not}` : ""}</span>
      )}
    </span>
  );
}

const statusBadge: Record<McpImportCandidate["status"], { tone: "neutral" | "info" | "warning" | "success"; text: string }> = {
  new: { tone: "info", text: "New" },
  changed: { tone: "warning", text: "Changed" },
  imported: { tone: "success", text: "In tab" },
  unsupported: { tone: "neutral", text: "Can't import" },
};

/** The dialog: pick servers, import, then sign in to the ones that need it. */
export function ImportDialog({ onClose, onOpen }: { onClose: () => void; onOpen: (id: string) => void }) {
  const qc = useQueryClient();
  const scan = useClaudeImport();
  const list = useMemo(() => scan.data ?? [], [scan.data]);
  const [picked, setPicked] = useState<Set<string> | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [results, setResults] = useState<McpImportResult[] | null>(null);
  const [signedIn, setSignedIn] = useState<Record<string, "waiting" | "done" | string>>({});

  useEffect(() => {
    void qc.invalidateQueries({ queryKey: keys.mcpImport });
  }, [qc]);
  // Everything importable starts ticked.
  const chosen = picked ?? new Set(list.filter(actionable).map((c) => c.key));
  const toggle = (k: string) => {
    const next = new Set(chosen);
    if (next.has(k)) next.delete(k);
    else next.add(k);
    setPicked(next);
  };

  useEffect(() => {
    if (!results) return;
    let off: (() => void) | undefined;
    void onMcpSignIn((e) => {
      void qc.invalidateQueries({ queryKey: keys.mcpServers });
      setSignedIn((s) => (e.id in s ? { ...s, [e.id]: e.ok ? "done" : (e.error ?? "The sign-in didn't finish.") } : s));
    }).then((u) => (off = u));
    return () => off?.();
  }, [results, qc]);

  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await api.mcpImport(list.filter((c) => chosen.has(c.key)).map((c) => c.key));
      setResults(r);
      await qc.invalidateQueries({ queryKey: keys.mcpServers });
      await qc.invalidateQueries({ queryKey: keys.mcpImport });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const signIn = async (id: string) => {
    setSignedIn((s) => ({ ...s, [id]: "waiting" }));
    try {
      await openUrl(await api.mcpSignIn(id));
    } catch (e) {
      setSignedIn((s) => ({ ...s, [id]: String(e) }));
    }
  };

  const count = list.filter((c) => chosen.has(c.key) && actionable(c)).length;

  return (
    <div className="mcp-overlay" role="dialog" aria-label="Import from Claude Code">
      <div className="mcp-dialog wide">
        <div className="mcp-dialog-head">
          <span className="import-banner-icon big"><Icon name="download" size={18} /></span>
          <div>
            <div className="mcp-dialog-title">{results ? "Imported from Claude Code" : "Import from Claude Code"}</div>
            <div className="subtle">
              {results
                ? "Sign in to the ones that ask. Their sign-ins in Claude Code stay its own."
                : "These become tab connections, so strict chats can use them. Nothing in Claude Code changes."}
            </div>
          </div>
        </div>

        {!results && (
          <div className="import-list">
            {scan.isLoading && <div className="subtle import-empty">Looking at Claude Code's settings…</div>}
            {scan.error && <div className="chat-error import-empty">{String(scan.error)}</div>}
            {!scan.isLoading && !list.length && !scan.error && (
              <div className="subtle import-empty">No MCP servers in Claude Code. tab looks in ~/.claude.json and the .mcp.json of projects it knows.</div>
            )}
            {list.map((c) => {
              const can = actionable(c);
              const b = statusBadge[c.status];
              return (
                <label key={c.key} className={cx("import-row", !can && "muted")}>
                  <input type="checkbox" disabled={!can} checked={can && chosen.has(c.key)} onChange={() => toggle(c.key)} />
                  <McpMark name={c.name} catalog={c.catalog} size={28} />
                  <div className="import-row-main">
                    <div className="import-row-head">
                      <span className="import-row-name">{c.name}</span>
                      {c.id !== c.name && c.status !== "unsupported" && <code className="subtle" title="What agents see it as">{c.id}</code>}
                      <Badge tone={b.tone}>{b.text}</Badge>
                    </div>
                    <div className="import-row-source subtle">{sourceLabel(c)}</div>
                    {(c.url || c.command) && <code className="import-row-what" title={c.command ?? c.url ?? undefined}>{c.command ?? c.url}</code>}
                    {c.status === "unsupported" ? (
                      <div className="import-row-note">{c.note}</div>
                    ) : (
                      <>
                        <div className="import-row-how">
                          <span>{c.status === "imported" ? "Already a tab connection." : howItImports(c)}</span>
                          <WhereItRuns fit={c.cloud} />
                        </div>
                        {c.note && <div className="import-row-note warn">{c.note}</div>}
                      </>
                    )}
                  </div>
                </label>
              );
            })}
          </div>
        )}

        {results && (
          <div className="import-list">
            {results.map((r) => {
              const st = signedIn[r.id];
              return (
                <div key={r.key} className="import-row">
                  <McpMark name={r.name || r.key} catalog={list.find((c) => c.key === r.key)?.catalog} size={28} />
                  <div className="import-row-main">
                    <div className="import-row-head"><span className="import-row-name">{r.name || r.key}</span></div>
                    <div className="import-row-source subtle">
                      {r.error ? (
                        <span className="chat-error">{r.error}</span>
                      ) : !r.sign_in || st === "done" ? (
                        <><StatusDot tone="success" /> Ready for agents</>
                      ) : st === "waiting" ? (
                        <><span className="mcp-spin" /> Waiting for you in the browser…</>
                      ) : st ? (
                        <span className="chat-error">{st}</span>
                      ) : (
                        <><StatusDot tone="warning" /> Sign in to finish</>
                      )}
                    </div>
                  </div>
                  {!r.error && r.sign_in && st !== "done" && (
                    <Button size="sm" variant={st ? "secondary" : "primary"} onClick={() => void signIn(r.id)}>{st ? "Open again" : "Sign in"}</Button>
                  )}
                  {!r.error && (!r.sign_in || st === "done") && (
                    <Button size="sm" variant="ghost" onClick={() => (onClose(), onOpen(r.id))}>Tools</Button>
                  )}
                </div>
              );
            })}
          </div>
        )}

        {error && <p className="chat-error">{error}</p>}
        <div className="mcp-dialog-foot">
          {results ? (
            <>
              <span />
              <Button size="sm" variant="primary" onClick={onClose}>Done</Button>
            </>
          ) : (
            <>
              <Button variant="ghost" size="sm" onClick={onClose}>Cancel</Button>
              <Button size="sm" variant="primary" disabled={busy || !count} onClick={() => void run()}>
                {busy ? "Importing…" : `Import ${count || ""}`.trim()}
              </Button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
