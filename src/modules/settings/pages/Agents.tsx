import { useQuery } from "@tanstack/react-query";
import { openOnboarding } from "../../work/Chat";
import { Button, Card, FormRow, Section, TextArea, TextInput, Toggle } from "../../../design/components";
import { api } from "../../../lib/api";
import { useFolderProblem, useHarnesses, useModels } from "../../../lib/hooks";
import { ModelMenu, modelFor, useInstalled } from "../../work/ModelMenu";
import { orderHarnesses } from "../../../lib/harnesses";
import { useDraft } from "../draft";
import { AgentVersionRow } from "./AgentVersion";
import { SandboxSettings } from "./Sandbox";

export function AgentsPage() {
  const { draft, set } = useDraft();
  const titlerRules = useQuery({ queryKey: ["titler-rules"], queryFn: api.titlerDefaultRules, staleTime: Infinity });
  const titler = draft.agent.titler ?? { enabled: true, model: null, instructions: "" };
  const { data: harnesses = [] } = useHarnesses();
  const { data: models = [] } = useModels();
  const installed = useInstalled();
  const workdirProblem = useFolderProblem(draft.agent.workdir);
  const titlerAgent = titler.agent ?? draft.agent.kind;
  const harness = harnesses.find((h) => h.id === draft.agent.kind);
  const setTitler = (patch: Partial<typeof titler>) => set((c) => void (c.agent.titler = { ...titler, ...patch }));
  return (
    <>
      <Section title="Agent" description="The coding agent tab runs for chats. Tool approvals come to you in the chat; nothing runs with permissions bypassed." actions={<Button size="sm" onClick={openOnboarding}>Run setup again</Button>}>
        <Card>
          <FormRow label="Agent">
            <select className="select" value={draft.agent.kind ?? ""} onChange={(e) => set((c) => void ((c.agent.kind = (e.target.value || null) as typeof c.agent.kind), (c.agent.model = c.agent.kind ? c.agent.models?.[c.agent.kind] ?? null : null)))}>
              <option value="">Not set up</option>
              {orderHarnesses(harnesses, installed).map((h) => <option key={h.id} value={h.id}>{h.name}{installed.has(h.id) ? "" : " (not found on this computer)"}</option>)}
            </select>
          </FormRow>
          <AgentVersionRow />
          <FormRow label="Command" help="Leave empty to find it on PATH, or give a full path.">
            <TextInput placeholder={harness?.command ?? ""} value={draft.agent.command ?? ""} onChange={(e) => set((c) => void (c.agent.command = e.target.value || null))} />
          </FormRow>
          <FormRow label="Work folder" help="Where sessions start.">
            <TextInput value={draft.agent.workdir ?? ""} onChange={(e) => set((c) => void (c.agent.workdir = e.target.value || null))} />
            {workdirProblem && <small className="text-danger" role="alert">{workdirProblem}</small>}
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

      <SandboxSettings />

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
