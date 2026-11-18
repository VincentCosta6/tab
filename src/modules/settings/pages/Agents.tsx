import { useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { openOnboarding } from "../../work/Chat";
import { Badge, Button, Card, FormRow, Section, TextArea, TextInput, Toggle } from "../../../design/components";
import { api, onAgentUpgrade } from "../../../lib/api";
import { useFolderProblem, useHarnesses, useModels } from "../../../lib/hooks";
import { HarnessIcon, ModelMenu, modelFor, useInstalled } from "../../work/ModelMenu";
import { orderHarnesses } from "../../../lib/harnesses";
import { useDraft } from "../draft";
import type { AgentProbe, Harness } from "../../../lib/types";
import { useAgentUpdate } from "../../updates/agentUpdate";
import { AgentVersionRow } from "./AgentVersion";
import { ModelFamiliesSettings } from "./ModelFamilies";
import { SandboxSettings } from "./Sandbox";

/** Claude Code's efforts, as `config::AgentConfig::effort` takes them. */
const EFFORTS: [string, string][] = [
  ["", "Your Claude Code setting"],
  ["low", "Low"],
  ["medium", "Medium"],
  ["high", "High"],
  ["xhigh", "Extra high"],
  ["max", "Max"],
];

export function AgentsPage() {
  const { draft, set } = useDraft();
  const titlerRules = useQuery({ queryKey: ["titler-rules"], queryFn: api.titlerDefaultRules, staleTime: Infinity });
  const titler = draft.agent.titler ?? { enabled: true, model: null, instructions: "" };
  const { data: harnesses = [] } = useHarnesses();
  const { data: models = [] } = useModels();
  const installed = useInstalled();
  const workdirProblem = useFolderProblem(draft.agent.workdir);
  const titlerAgent = titler.agent ?? draft.agent.kind;
  const { data: probes = [] } = useQuery({ queryKey: ["probes"], queryFn: api.detectAgents, staleTime: 60_000 });
  const setTitler = (patch: Partial<typeof titler>) => set((c) => void (c.agent.titler = { ...titler, ...patch }));
  return (
    <>
      <Section title="Agents" description="The coding agents tab runs for chats. Set up each one you have a subscription for: every chat picks its agent and model from the model menu, and new chats start on the default. Tool approvals come to you in the chat; nothing runs with permissions bypassed." actions={<Button size="sm" onClick={openOnboarding}>Run setup again</Button>}>
        <Card>
          <FormRow label="Default for new chats" help="Any chat can switch to another agent before it starts.">
            <select className="select" value={draft.agent.kind ?? ""} onChange={(e) => set((c) => void ((c.agent.kind = (e.target.value || null) as typeof c.agent.kind), (c.agent.model = c.agent.kind ? c.agent.models?.[c.agent.kind] ?? null : null)))}>
              <option value="">Not set up</option>
              {orderHarnesses(harnesses, installed).map((h) => <option key={h.id} value={h.id}>{h.name}{installed.has(h.id) ? "" : " (not found on this computer)"}</option>)}
            </select>
          </FormRow>
          <FormRow label="Work folder" help="Where sessions start.">
            <TextInput value={draft.agent.workdir ?? ""} onChange={(e) => set((c) => void (c.agent.workdir = e.target.value || null))} />
            {workdirProblem && <small className="text-danger" role="alert">{workdirProblem}</small>}
          </FormRow>
          <FormRow label="Strict mode" help="Chats load only tab's connections, never your personal MCP servers, connectors, plugins or hooks. Your model, effort, thinking setting and personal CLAUDE.md still come along, the repo's own config still loads, and review chats are always strict.">
            <Toggle label="Strict mode" checked={draft.agent.strict !== false} onChange={(v) => set((c) => void (c.agent.strict = v))} />
          </FormRow>
        </Card>
        <div className="harness-list">
          {orderHarnesses(harnesses, installed).map((h) => <HarnessSetup key={h.id} harness={h} probe={probes.find((p) => p.kind === h.id)} />)}
        </div>
      </Section>

      <SandboxSettings />

      <ModelFamiliesSettings />

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

      <Section title="Workflow ideas" description="When you mark a chat task done, a small model (the chat titles model) asks whether it could have been a workflow, looking at your other finished tasks and the workflows you have. A fit shows on the task and in Workflows, ready to hand to the builder. Nothing is built or turned on without you.">
        <Card>
          <FormRow label="Enabled">
            <Toggle label="Suggest workflows from finished tasks" checked={draft.agent.workflow_ideas !== false} onChange={(v) => set((c) => void (c.agent.workflow_ideas = v))} />
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
    </>
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

/**
 * One agent's own setup: whether tab found it (and installing it when not),
 * its command, the model its chats start on, and what only it has.
 */
function HarnessSetup({ harness, probe }: { harness: Harness; probe: AgentProbe | undefined }) {
  const { draft, set } = useDraft();
  const id = harness.id;
  const found = !!probe?.found;
  const isDefault = draft.agent.kind === id;
  const { behind, latest } = useAgentUpdate(id, found);
  // The default starts open; the rest open on a click.
  const shown = useRef(false);
  const opened = (el: HTMLDetailsElement | null) => {
    if (el && found && isDefault && !shown.current) (shown.current = true), (el.open = true);
  };
  // The legacy single command belongs to the default agent.
  const command = draft.agent.commands?.[id] ?? (isDefault ? draft.agent.command : null) ?? "";
  const setCommand = (v: string) =>
    set((c) => {
      const commands = { ...(c.agent.commands ?? {}) };
      if (v) commands[id] = v;
      else delete commands[id];
      c.agent.commands = commands;
      if (c.agent.kind === id) c.agent.command = null;
    });
  return (
    <details className="card harness-setup" ref={opened}>
      <summary>
        <HarnessIcon harness={harness} size={16} />
        <strong>{harness.name}</strong>
        {isDefault && <Badge tone="info">Default</Badge>}
        <span className="harness-setup-status">
          {!probe ? "Looking…" : found ? <span className="mono">{probe.version ?? "Installed"}</span> : <Badge>Not installed</Badge>}
          {behind && <Badge tone="info">{latest} available</Badge>}
        </span>
      </summary>
      <div className="harness-setup-body">
        <p className="harness-setup-pitch">{harness.pitch}</p>
        {probe && !found && <InstallHarness harness={harness} probe={probe} />}
        {found && <AgentVersionRow kind={id} />}
        <FormRow label="Command" help={found ? probe?.path ?? undefined : "Leave empty to find it on PATH, or give a full path."}>
          <TextInput placeholder={harness.command} value={command} onChange={(e) => setCommand(e.target.value.trim() ? e.target.value : "")} />
        </FormRow>
        {found && (
          <FormRow label="Model" help="For new chats on this agent. Each chat can switch too.">
            <ModelMenu
              variant="field"
              only={[id]}
              value={{ agent: id, model: modelFor(draft.agent, id) }}
              onChange={(p) => set((c) => void ((c.agent.models = { ...(c.agent.models ?? {}) }), p.model ? (c.agent.models[p.agent] = p.model) : delete c.agent.models[p.agent], c.agent.kind === p.agent && (c.agent.model = p.model)))}
            />
          </FormRow>
        )}
        {!!harness.capabilities.permission_modes.length && (
          <FormRow label="Permission mode" help="Anything that still needs approval is asked in the chat.">
            <select className="select" value={draft.agent.permission_mode} onChange={(e) => set((c) => void (c.agent.permission_mode = e.target.value))}>
              {harness.capabilities.permission_modes.map((m) => <option key={m.value} value={m.value}>{m.label}{m.help ? ` (${m.help})` : ""}</option>)}
            </select>
          </FormRow>
        )}
        {id === "claude_code" && (
          <FormRow label="Effort" help="How hard Claude thinks before answering. Higher is slower and uses more of your plan, and gives better work on hard tasks.">
            <select className="select" value={draft.agent.effort ?? ""} onChange={(e) => set((c) => void (c.agent.effort = e.target.value))}>
              {EFFORTS.map(([value, label]) => <option key={value} value={value}>{label}</option>)}
            </select>
          </FormRow>
        )}
        {!harness.capabilities.strict && <p className="harness-setup-pitch">Strict mode can't keep your own setup out of {harness.name} chats: they load your personal MCP servers too.</p>}
        {harness.acp?.preview && (
          <FormRow label="Agent Client Protocol" help={`Preview: run ${harness.name} chats over ACP, the open protocol Zed and other editors use, instead of tab's own adapter. Turn it off if a chat behaves differently.`}>
            <Toggle
              label="Use ACP"
              checked={!!draft.agent.acp?.includes(id)}
              onChange={(v) => set((c) => void (c.agent.acp = [...(c.agent.acp ?? []).filter((k) => k !== id), ...(v ? [id] : [])]))}
            />
            {harness.acp.strict === false && <small>Strict chats keep tab's own adapter: this one can't keep your own setup out.</small>}
            {draft.agent.acp?.includes(id) && <AcpAdapterInstall harness={harness} />}
          </FormRow>
        )}
        {found && !isDefault && (
          <div>
            <Button size="sm" onClick={() => set((c) => void ((c.agent.kind = id), (c.agent.model = c.agent.models?.[id] ?? null)))}>Make default</Button>
          </div>
        )}
      </div>
    </details>
  );
}

/** Installs a harness tab didn't find, with what the installer prints. */
function InstallHarness({ harness, probe }: { harness: Harness; probe: AgentProbe }) {
  const qc = useQueryClient();
  const [line, setLine] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const install = async () => {
    setBusy(true);
    setError(null);
    const stop = await onAgentUpgrade(setLine);
    try {
      await api.agentInstall(harness.id);
      await Promise.all([qc.invalidateQueries({ queryKey: ["probes"] }), qc.invalidateQueries({ queryKey: ["agent-models"] })]);
    } catch (e) {
      setError(String(e));
    } finally {
      stop();
      setBusy(false);
    }
  };
  return (
    <div className="settings-version">
      <div className="settings-version-line">
        <span>{probe.error ?? "Not found on this computer."}</span>
        {probe.install_command && <Button size="sm" variant="primary" disabled={busy} onClick={() => void install()}>{busy ? "Installing…" : "Install"}</Button>}
      </div>
      {probe.install_command && !busy && <small className="mono">{probe.install_command}</small>}
      {busy && line && <pre className="updater-log">{line}</pre>}
      {error && <p className="settings-error">{error}</p>}
    </div>
  );
}

/** When ACP runs through an adapter program (codex-acp) that isn't installed: what it is, its license, and a button that installs it. */
function AcpAdapterInstall({ harness }: { harness: Harness }) {
  const qc = useQueryClient();
  const adapter = harness.acp?.adapter;
  const installed = useQuery({ queryKey: ["acp-adapter", harness.id], queryFn: () => api.agentAcpAdapter(harness.id), enabled: !!adapter });
  const [line, setLine] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!adapter || installed.data !== false) return null;
  const install = async () => {
    setBusy(true);
    setError(null);
    const stop = await onAgentUpgrade(setLine);
    try {
      await api.agentInstallAdapter(harness.id);
      await qc.invalidateQueries({ queryKey: ["acp-adapter", harness.id] });
    } catch (e) {
      setError(String(e));
    } finally {
      stop();
      setBusy(false);
    }
  };
  return (
    <div className="settings-version">
      <div className="settings-version-line">
        <span>
          Needs <a href={adapter.repo} target="_blank" rel="noreferrer">{harness.acp?.command}</a> ({adapter.license}), which isn't installed.
        </span>
        {adapter.npm && <Button size="sm" variant="primary" disabled={busy} onClick={() => void install()}>{busy ? "Installing…" : "Install"}</Button>}
      </div>
      {busy && line && <pre className="updater-log">{line}</pre>}
      {error && <p className="settings-error">{error}</p>}
    </div>
  );
}
