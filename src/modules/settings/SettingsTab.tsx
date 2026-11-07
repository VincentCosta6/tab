import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { openOnboarding } from "../work/Chat";
import { keys } from "../../lib/query";
import { Badge, Button, Card, FormRow, Meter, PageHeader, Section, StatusDot, TextArea, TextInput, Toggle } from "../../design/components";
import { api } from "../../lib/api";
import type { TabConfig } from "../../lib/types";
import { useHarnesses, useModels } from "../../lib/hooks";
import { ModelMenu, modelFor } from "../work/ModelMenu";
import { bytes, timeAgo } from "../../lib/format";

const lines = (s: string) => s.split("\n").map((l) => l.trim()).filter(Boolean);

export function SettingsTab() {
  const [saved, setSaved] = useState<TabConfig | null>(null);
  const [draft, setDraft] = useState<TabConfig | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const qc = useQueryClient();
  const mcp = useQuery({ queryKey: ["mcp"], queryFn: api.mcpInfo });
  const { data: harnesses = [] } = useHarnesses();
  const { data: models = [] } = useModels();
  const titlerRules = useQuery({ queryKey: ["titler-rules"], queryFn: api.titlerDefaultRules, staleTime: Infinity });

  useEffect(() => {
    api.getConfig().then((c) => (setSaved(c), setDraft(c)));
  }, []);

  if (!draft || !saved) return <div className="page"><div className="skeleton" /></div>;

  const set = (fn: (c: TabConfig) => void) => {
    const next = structuredClone(draft);
    fn(next);
    setDraft(next);
  };
  const dirty = JSON.stringify(draft) !== JSON.stringify(saved);
  const rules = draft.ranking.rules;
  const ai = draft.ranking.ai;
  const titler = draft.agent.titler ?? { enabled: true, model: null, instructions: "" };
  const titlerAgent = titler.agent ?? draft.agent.kind;
  const harness = harnesses.find((h) => h.id === draft.agent.kind);
  const setTitler = (patch: Partial<typeof titler>) => set((c) => void (c.agent.titler = { ...titler, ...patch }));

  const save = async () => {
    setBusy(true);
    try {
      const c = await api.saveConfig(draft);
      qc.setQueryData(keys.config, c);
      setSaved(c);
      setDraft(c);
      setNote("Saved. Open items are being re-ranked; unchanged AI verdicts come from cache.");
    } catch (e) {
      setNote(`Couldn't save: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="page page-narrow">
      <PageHeader
        title="Settings"
        subtitle={note ?? "How tab decides what needs you first."}
        actions={<Button variant="primary" disabled={!dirty || busy} onClick={save}>{busy ? "Saving…" : "Save changes"}</Button>}
      />

      <Section title="You" description="Used by the rules (mentions of your handles) and given to the AI layer as context.">
        <Card>
          <FormRow label="Name">
            <TextInput value={draft.profile.name} onChange={(e) => set((c) => void (c.profile.name = e.target.value))} />
          </FormRow>
          <FormRow label="Role" help="e.g. Staff engineer on payments">
            <TextInput value={draft.profile.role} onChange={(e) => set((c) => void (c.profile.role = e.target.value))} />
          </FormRow>
          <FormRow label="Handles" help="Your usernames elsewhere, comma separated">
            <TextInput
              value={draft.profile.handles.join(", ")}
              onChange={(e) => set((c) => void (c.profile.handles = e.target.value.split(",").map((h) => h.trim()).filter(Boolean)))}
            />
          </FormRow>
        </Card>
      </Section>

      <Section title="Rules" description="Deterministic: the same item always gets the same base score. Changes re-rank the queue.">
        <Card>
          <FormRow label="Important people" help="One per line. Their items get a boost.">
            <TextArea value={rules.vip_actors.join("\n")} onChange={(e) => set((c) => void (c.ranking.rules.vip_actors = lines(e.target.value)))} />
          </FormRow>
          <FormRow label="Muted" help="Channels, repos or senders, one per line. Scored 0 and never sent to the AI.">
            <TextArea value={rules.muted.join("\n")} onChange={(e) => set((c) => void (c.ranking.rules.muted = lines(e.target.value)))} />
          </FormRow>
          <FormRow label="Keywords" help="One per line as word: +10 or word: -15">
            <TextArea
              value={rules.keywords.map((k) => `${k.contains}: ${k.delta >= 0 ? "+" : ""}${k.delta}`).join("\n")}
              onChange={(e) =>
                set((c) =>
                  void (c.ranking.rules.keywords = lines(e.target.value).flatMap((l) => {
                    const m = /^(.+?):\s*([+-]?\d+)$/.exec(l);
                    return m ? [{ contains: m[1].trim(), delta: Number(m[2]) }] : [];
                  })),
                )
              }
            />
          </FormRow>
        </Card>
      </Section>

      <Section title="Agent" description="The coding agent tab runs for chats. Tool approvals come to you in the chat; nothing runs with permissions bypassed." actions={<Button size="sm" onClick={openOnboarding}>Run setup again</Button>}>
        <Card>
          <FormRow label="Agent">
            <select className="select" value={draft.agent.kind ?? ""} onChange={(e) => set((c) => void ((c.agent.kind = (e.target.value || null) as typeof c.agent.kind), (c.agent.model = c.agent.kind ? c.agent.models?.[c.agent.kind] ?? null : null)))}>
              <option value="">Not set up</option>
              {harnesses.map((h) => <option key={h.id} value={h.id}>{h.name}</option>)}
            </select>
          </FormRow>
          <FormRow label="Command" help="Leave empty to find it on PATH, or give a full path.">
            <TextInput placeholder={harness?.command ?? ""} value={draft.agent.command ?? ""} onChange={(e) => set((c) => void (c.agent.command = e.target.value || null))} />
          </FormRow>
          <FormRow label="Work folder" help="Where sessions start.">
            <TextInput value={draft.agent.workdir ?? ""} onChange={(e) => set((c) => void (c.agent.workdir = e.target.value || null))} />
          </FormRow>
          {draft.agent.kind && (
            <FormRow label="Model" help="For new chats on this agent. Each chat can switch too.">
              <ModelMenu
                variant="field"
                only={[draft.agent.kind]}
                value={{ agent: draft.agent.kind, model: modelFor(draft.agent, draft.agent.kind) }}
                onChange={(p) => set((c) => void ((c.agent.models = { ...(c.agent.models ?? {}) }), p.model ? (c.agent.models[p.agent] = p.model) : delete c.agent.models[p.agent], (c.agent.model = p.model)))}
              />
            </FormRow>
          )}
          <FormRow label="Strict mode" help="Chats load only tab's connections, never your personal MCP servers, connectors, plugins, hooks or user settings. The repo's own config still loads, and review chats are always strict.">
            <Toggle label="Strict mode" checked={draft.agent.strict !== false} onChange={(v) => set((c) => void (c.agent.strict = v))} />
          </FormRow>
          {!!harness?.capabilities.permission_modes.length && (
            <FormRow label="Permission mode" help="Anything that still needs approval is asked in the chat.">
              <select className="select" value={draft.agent.permission_mode} onChange={(e) => set((c) => void (c.agent.permission_mode = e.target.value))}>
                {harness.capabilities.permission_modes.map((m) => <option key={m.value} value={m.value}>{m.label}{m.help ? ` (${m.help})` : ""}</option>)}
              </select>
            </FormRow>
          )}
          {harness?.capabilities.external_server && (
            <FormRow label="Server URL" help="Leave empty and tab starts its own server.">
              <TextInput placeholder="http://127.0.0.1:4096" value={draft.agent.opencode_url ?? ""} onChange={(e) => set((c) => void (c.agent.opencode_url = e.target.value || null))} />
            </FormRow>
          )}
        </Card>
      </Section>

      <Section title="Chat titles" description="After the first few turns of a chat, a small model names it for what the thread is actually doing. It stops once it's confident, and never touches a title you've set yourself.">
        <Card>
          <FormRow label="Enabled">
            <Toggle label="Title chats" checked={titler.enabled} onChange={(v) => setTitler({ enabled: v })} />
          </FormRow>
          <FormRow label="Model" help="Any model on any installed agent, whichever agent the chat runs on.">
            <ModelMenu
              variant="field"
              value={titlerAgent ? { agent: titlerAgent, model: titler.model ?? null } : null}
              defaultLabel={(h) => (h.small_model ? models.find((m) => m.agent === h.id && m.id === h.small_model)?.label ?? h.small_model : "its own default")}
              onChange={(p) => setTitler({ agent: p.agent, model: p.model })}
            />
          </FormRow>
          <FormRow label="Your rules" help="Plain language. Added after the built-in rules, and yours win.">
            <div className="titler-rules">
              <TextArea
                rows={4}
                placeholder={"Start with the repo name, e.g. \"tab: Fix badge overflow\".\nUse the ticket id when there is one."}
                value={titler.instructions}
                onChange={(e) => setTitler({ instructions: e.target.value })}
              />
              {titlerRules.data && <p className="titler-defaults"><strong>Built in:</strong> {titlerRules.data}</p>}
            </div>
          </FormRow>
        </Card>
      </Section>

      <Section title="Spend limits" description="Costs come from what the agent reports: real spend on an API key, or the API-equivalent figure on a Claude plan. Leave a limit empty for none.">
        <Card>
          <FormRow label="Daily limit" help="New chat turns are refused once today's spend reaches this.">
            <DollarInput value={draft.agent.budget.daily_usd} onChange={(v) => set((c) => void (c.agent.budget.daily_usd = v))} />
          </FormRow>
          <FormRow label="Monthly limit">
            <DollarInput value={draft.agent.budget.monthly_usd} onChange={(v) => set((c) => void (c.agent.budget.monthly_usd = v))} />
          </FormRow>
          <FormRow label="Per turn" help="Claude Code stops a single turn past this.">
            <DollarInput value={draft.agent.budget.per_turn_usd} onChange={(v) => set((c) => void (c.agent.budget.per_turn_usd = v))} />
          </FormRow>
          <FormRow label="Save plan headroom" help="On a Claude plan, reading the queue pauses once a usage window passes this, so the rest is yours.">
            <select className="select" value={String(draft.agent.budget.ranker_plan_cap)} onChange={(e) => set((c) => void (c.agent.budget.ranker_plan_cap = Number(e.target.value)))}>
              {[0.5, 0.6, 0.7, 0.8, 0.9, 1].map((v) => <option key={v} value={v}>{v === 1 ? "Never pause" : `Pause at ${v * 100}%`}</option>)}
            </select>
          </FormRow>
        </Card>
      </Section>

      <Section title="MCP server" description="Agents tab starts are connected automatically. Point any other MCP client here to let it search your queue, file what it finds, and create tasks.">
        <Card>
          <FormRow label="URL">
            <code className="mono">{mcp.data?.url ?? "not running"}</code>
          </FormRow>
          <FormRow label="Token" help="Send as Authorization: Bearer …">
            <Button size="sm" icon="copy" onClick={() => mcp.data && navigator.clipboard.writeText(mcp.data.token)}>Copy token</Button>
          </FormRow>
        </Card>
      </Section>

      <HubSection />

      <SandboxSection config={draft.sandbox} onChange={(v) => set((c) => void (c.sandbox = v))} />

      <StorageSection config={draft.storage} onChange={(v) => set((c) => void (c.storage = v))} />

      <Section title="AI layer" description={`Claude Haiku reads each item with your instructions and nudges the rules score by up to ±25, with a one-line reason. Verdicts are cached by their inputs, so re-ranking is repeatable.`}>
        <Card>
          <FormRow label="Enabled">
            <Toggle label="AI layer" checked={ai.enabled} onChange={(v) => set((c) => void (c.ranking.ai.enabled = v))} />
          </FormRow>
          <FormRow label="Runs on" help="Reading the queue uses Claude Haiku. On your Claude plan it goes through the Claude Code CLI, so there's no key to manage.">
            <select className="select" value={ai.backend} onChange={(e) => set((c) => void (c.ranking.ai.backend = e.target.value as typeof c.ranking.ai.backend))}>
              <option value="auto">Automatic (API key if set, otherwise your Claude plan)</option>
              <option value="claude_code">Your Claude plan (Claude Code)</option>
              <option value="api">Anthropic API key</option>
            </select>
          </FormRow>
          <FormRow label="Anthropic API key" help="Or set ANTHROPIC_API_KEY before launching.">
            <TextInput type="password" autoComplete="off" value={ai.api_key ?? ""} onChange={(e) => set((c) => void (c.ranking.ai.api_key = e.target.value || null))} />
          </FormRow>
          <FormRow label="Model" help="Defaults to claude-haiku-5-5">
            <TextInput placeholder="claude-haiku-5-5" value={ai.model ?? ""} onChange={(e) => set((c) => void (c.ranking.ai.model = e.target.value || null))} />
          </FormRow>
          <FormRow label="Your instructions" help="Plain language. What matters to you, and what can wait.">
            <TextArea
              rows={5}
              placeholder="Anything from on-call or my manager is urgent. Dependabot PRs can wait until Friday. I don't review frontend PRs."
              value={ai.instructions}
              onChange={(e) => set((c) => void (c.ranking.ai.instructions = e.target.value))}
            />
          </FormRow>
        </Card>
      </Section>
    </div>
  );
}

/** Dollars, or empty for no limit. */
function DollarInput({ value, onChange }: { value: number | null; onChange: (v: number | null) => void }) {
  return (
    <TextInput
      type="number"
      min={0}
      step={0.5}
      placeholder="No limit"
      value={value ?? ""}
      onChange={(e) => onChange(e.target.value === "" ? null : Math.max(0, Number(e.target.value)))}
      style={{ maxWidth: 160 }}
    />
  );
}

/** What agents, language servers and workflow commands may touch. */
function SandboxSection({ config, onChange }: { config: TabConfig["sandbox"]; onChange: (v: TabConfig["sandbox"]) => void }) {
  const status = useQuery({ queryKey: ["sandbox"], queryFn: api.sandboxStatus });
  const s = status.data;
  const list = (v: string[]) => v.join("\n");
  return (
    <Section
      title="Sandbox"
      description="Agents, language servers on pull requests, and workflow commands run with limits. Your own chats work as before; they just can't change tab itself or your shell setup, or read stored credentials."
    >
      <Card>
        <FormRow label="On" help="Applies from the next message or run.">
          <Toggle label="Sandbox" checked={config.enabled} onChange={(v) => onChange({ ...config, enabled: v })} />
        </FormRow>
        <FormRow label="On this computer">
          <span className="subtle">{s ? <>{s.summary}{s.fix && <> {s.fix}</>}</> : "Checking…"}</span>
        </FormRow>
        <FormRow label="GitHub in chats" help="gh and git reach GitHub through tab, signed in with your GitHub connection. Force pushes, merges and deletes ask you first, and agents never see the token.">
          <span className="subtle">Always on while the sandbox is on.</span>
        </FormRow>
        <FormRow label="SSH keys in chats" help="Off keeps ~/.ssh and your SSH agent out of chats, so GitHub is only reachable through tab. Leave on if agents push to other hosts over SSH.">
          <Toggle label="SSH keys in chats" checked={config.ssh_in_chats ?? true} onChange={(v) => onChange({ ...config, ssh_in_chats: v })} />
        </FormRow>
        <FormRow label="Also protect" help="Folders agents may read but not change, one per line. tab's own data and, in a dev build, its checkout are always protected.">
          <TextArea rows={3} placeholder={"~/code/production-config"} value={list(config.protected)}
            onChange={(e) => onChange({ ...config, protected: e.target.value.split("\n") })} />
        </FormRow>
        <FormRow label="Also hide" help="Files or folders agents can't read at all, one per line. Cloud credentials, browser profiles and tab's config are always hidden.">
          <TextArea rows={3} placeholder={"~/.config/acme/credentials"} value={list(config.hidden)}
            onChange={(e) => onChange({ ...config, hidden: e.target.value.split("\n") })} />
        </FormRow>
        <FormRow label="Language servers may reach" help="Hosts code intelligence on a pull request may fetch dependencies from, one per line. *.example.com covers subdomains.">
          <TextArea rows={4} value={list(config.lsp_domains)}
            onChange={(e) => onChange({ ...config, lsp_domains: e.target.value.split("\n") })} />
        </FormRow>
        <FormRow label="Containers" help={s?.container ? `Workflow commands that name an image run in ${s.container}.` : "Workflow commands that name an image need Docker or Podman running."}>
          <select className="select" value={config.container_runtime} onChange={(e) => onChange({ ...config, container_runtime: e.target.value })}>
            <option value="">Whichever is installed</option>
            <option value="docker">Docker</option>
            <option value="podman">Podman</option>
          </select>
        </FormRow>
      </Card>
    </Section>
  );
}

/** What tab keeps on disk, and clearing it out. */
function StorageSection({ config, onChange }: { config: TabConfig["storage"]; onChange: (v: TabConfig["storage"]) => void }) {
  const qc = useQueryClient();
  const usage = useQuery({ queryKey: ["storage"], queryFn: api.storageUsage });
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const run = async (key: string, fn: () => Promise<{ removed: number; bytes: number }>) => {
    setBusy(key);
    setNote(null);
    try {
      const s = await fn();
      setNote(s.removed ? `Freed ${bytes(s.bytes)}.` : "Nothing to clear.");
    } catch (e) {
      setNote(`Couldn't clear: ${e}`);
    } finally {
      setBusy(null);
      qc.invalidateQueries({ queryKey: ["storage"] });
    }
  };
  const u = usage.data;
  const size = (s?: { bytes: number; files: number }) => (s ? `${bytes(s.bytes)} · ${s.files.toLocaleString()} files` : "…");

  return (
    <Section
      title="Storage"
      description="Cleared automatically every few hours. Review checkouts also go as soon as their PR closes or merges."
      actions={<Button size="sm" disabled={busy !== null} onClick={() => run("clean", api.storageClean)}>{busy === "clean" ? "Cleaning…" : "Clean up now"}</Button>}
    >
      <Card>
        <FormRow label="Keep for" help="Attachments of chats idle this long, and review checkouts not opened this long, are removed.">
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <TextInput type="number" min={1} step={1} value={config.keep_days} style={{ maxWidth: 100 }}
              onChange={(e) => onChange({ ...config, keep_days: Math.max(1, Math.round(Number(e.target.value) || 1)) })} />
            <span>days</span>
          </div>
        </FormRow>
        <FormRow label="Size limit" help="Past this, the least recently used attachments and checkouts are removed until it fits. Anything used in the last hour stays.">
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <TextInput type="number" min={0} step={0.5} placeholder="No limit" value={config.max_gb ?? ""} style={{ maxWidth: 100 }}
              onChange={(e) => onChange({ ...config, max_gb: e.target.value === "" || Number(e.target.value) <= 0 ? null : Number(e.target.value) })} />
            <span>GB</span>
          </div>
        </FormRow>
        <FormRow label="Chat attachments" help="Files you attach to chats.">
          <span className="mono">{size(u?.attachments)}</span>
        </FormRow>
        <FormRow label="Chat folders" help="Each chat's prompt and settings for its agent.">
          <span className="mono">{size(u?.sessions)}</span>
        </FormRow>
        <FormRow label="Review checkouts" help="Local copies of repositories for PR reviews. Opening a PR again brings its copy back.">
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <span className="mono">{u ? bytes(u.workspaces.bytes) : "…"}</span>
            {!!u?.checkouts.length && (
              <Button size="sm" disabled={busy !== null} onClick={() => run("all", () => api.storageRemoveCheckouts())}>
                {busy === "all" ? "Removing…" : "Remove all"}
              </Button>
            )}
          </div>
        </FormRow>
        {u?.checkouts.map((c) => (
          <FormRow key={`${c.repo}#${c.number}`} label={`${c.repo} #${c.number}`} help={opened(c.last_used)}>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span className="mono">{bytes(c.bytes)}</span>
              <Button size="sm" variant="ghost" icon="trash" aria-label={`Remove ${c.repo} #${c.number}`} disabled={busy !== null}
                onClick={() => run(`${c.repo}#${c.number}`, () => api.storageRemoveCheckouts(c.repo, c.number))} />
            </div>
          </FormRow>
        ))}
        {note && <FormRow label=""><span>{note}</span></FormRow>}
      </Card>
    </Section>
  );
}

/** The hub running inside the app: where it is, who's connected, what it holds. */
function HubSection() {
  const status = useQuery({ queryKey: ["hub-status"], queryFn: api.hubStatus, refetchInterval: 2000 });
  const h = status.data;
  if (status.isLoading) return null;
  if (!h) {
    return (
      <Section title="Hub" description="The hub keeps your work in sync across devices.">
        <Card><FormRow label="Status"><Badge tone="danger">Not running</Badge></FormRow></Card>
      </Section>
    );
  }
  const others = h.connections.filter((c) => !c.this_app);
  const cpuOfMachine = h.cpu_percent == null ? null : h.cpu_percent / Math.max(1, h.cpus);
  return (
    <Section
      title="Hub"
      description="Your queue, projects, context and workflows sync through the hub. It runs inside this app, so everything stays on this computer. Other devices can connect to it here."
    >
      <Card>
        <FormRow label="Runs on" help={h.started_at ? `Started ${timeAgo(h.started_at)}${/^\d/.test(timeAgo(h.started_at)) ? " ago" : ""}` : undefined}>
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <StatusDot tone="success" />
            <span>{h.runs_on === "local" ? "This computer" : "Cloud"}</span>
            <Badge>protocol v{h.protocol_version}</Badge>
          </div>
        </FormRow>
        <FormRow label="Devices connect at" help="TLS 1.3 only. Devices send the MCP server's token in a header and pin this hub's certificate.">
          <div style={{ display: "grid", gap: 4 }}>
            <code className="mono">{h.url ?? "not listening"}</code>
            {h.url && <span className="mono" style={{ fontSize: 12, opacity: 0.7 }}>{h.http3 ? "HTTP/3, HTTP/2" : "HTTP/2"}</span>}
          </div>
        </FormRow>
        {h.fingerprint && (
          <FormRow label="Certificate" help="Made by this computer for its hub. A device checks it matches this before it connects.">
            <code className="mono" style={{ wordBreak: "break-all" }}>{h.fingerprint}</code>
          </FormRow>
        )}
        <FormRow label="CPU" help="The hub runs inside the app, so this is the whole app.">
          <div style={{ display: "grid", gap: 4, minWidth: 220 }}>
            <span className="mono">{cpuOfMachine == null ? "unavailable" : `${cpuOfMachine.toFixed(1)}% of ${h.cpus} cores`}</span>
            {cpuOfMachine != null && <Meter value={cpuOfMachine} max={100} tone={cpuOfMachine > 50 ? "warning" : "accent"} />}
          </div>
        </FormRow>
        <FormRow label="Memory">
          <span className="mono">{h.memory_bytes == null ? "unavailable" : bytes(h.memory_bytes)}</span>
        </FormRow>
        <FormRow label="Changes" help="Every change ever made, in order. Devices sync by asking for what's after the last one they saw.">
          <span className="mono">{h.head.toLocaleString()}</span>
        </FormRow>
        <FormRow label="Records">
          <span className="mono">{h.records.toLocaleString()}</span>
        </FormRow>
        <FormRow label="Claimed work" help="Items or steps a device is working on right now.">
          <span className="mono">{h.live_leases.toLocaleString()}</span>
        </FormRow>
        <FormRow label="Storage" help={h.path}>
          <span className="mono">{bytes(h.storage_bytes)}</span>
        </FormRow>
        <FormRow label="Files" help="Chat attachments, kept by the hub while a message refers to them.">
          <span className="mono">{(h.files ?? 0).toLocaleString()} · {bytes(h.file_bytes ?? 0)}</span>
        </FormRow>
        <FormRow label="Waiting to sync" help="Local changes not yet recorded in the hub. Normally 0.">
          <span className="mono">{h.pending.toLocaleString()}</span>
        </FormRow>
      </Card>
      <Card>
        <FormRow label="Connections" help={others.length ? undefined : "Only this app. Phones and other computers will show up here."}>
          <span className="mono">{h.connections.length}</span>
        </FormRow>
        {h.connections.map((c) => (
          <FormRow
            key={c.id}
            label={c.this_app ? "This app" : c.device || "Connecting…"}
            help={`${c.client || "unknown client"} · ${c.address}${c.connected_at ? ` · connected ${timeAgo(c.connected_at)}` : ""}`}
          >
            {c.behind === 0 ? <Badge tone="success">Up to date</Badge> : <Badge tone="warning">{c.behind.toLocaleString()} behind</Badge>}
          </FormRow>
        ))}
      </Card>
    </Section>
  );
}

/** "Opened 3h ago", "Opened just now", or "Opened 10/28/2026" once it's old. */
function opened(iso: string) {
  const ago = timeAgo(iso);
  return /^\d+[mhd]$/.test(ago) ? `Opened ${ago} ago` : `Opened ${ago}`;
}
