import { useQuery } from "@tanstack/react-query";
import { openOnboarding } from "../../work/Chat";
import { Button, Card, FormRow, Section, TextArea, TextInput, Toggle } from "../../../design/components";
import { api } from "../../../lib/api";
import { useDraft } from "../draft";
import { AgentVersionRow } from "./AgentVersion";

export function AgentsPage() {
  const { draft, set } = useDraft();
  const titlerRules = useQuery({ queryKey: ["titler-rules"], queryFn: api.titlerDefaultRules, staleTime: Infinity });
  const titler = draft.agent.titler ?? { enabled: true, model: null, instructions: "" };
  const setTitler = (patch: Partial<typeof titler>) => set((c) => void (c.agent.titler = { ...titler, ...patch }));
  return (
    <>
      <Section title="Agent" description="The coding agent tab runs for chats. Tool approvals come to you in the chat; nothing runs with permissions bypassed." actions={<Button size="sm" onClick={openOnboarding}>Run setup again</Button>}>
        <Card>
          <FormRow label="Agent">
            <select className="select" value={draft.agent.kind ?? ""} onChange={(e) => set((c) => void (c.agent.kind = (e.target.value || null) as typeof c.agent.kind))}>
              <option value="">Not set up</option>
              <option value="opencode">OpenCode</option>
              <option value="claude_code">Claude Code</option>
            </select>
          </FormRow>
          <AgentVersionRow />
          <FormRow label="Command" help="Leave empty to find it on PATH, or give a full path.">
            <TextInput placeholder={draft.agent.kind === "opencode" ? "opencode" : "claude"} value={draft.agent.command ?? ""} onChange={(e) => set((c) => void (c.agent.command = e.target.value || null))} />
          </FormRow>
          <FormRow label="Work folder" help="Where sessions start.">
            <TextInput value={draft.agent.workdir ?? ""} onChange={(e) => set((c) => void (c.agent.workdir = e.target.value || null))} />
          </FormRow>
          <FormRow label="Model" help={draft.agent.kind === "opencode" ? "provider/model" : "e.g. sonnet, opus"}>
            <TextInput placeholder="Agent default" value={draft.agent.model ?? ""} onChange={(e) => set((c) => void (c.agent.model = e.target.value || null))} />
          </FormRow>
          {draft.agent.kind === "claude_code" && (
            <FormRow label="Permission mode" help="Anything that still needs approval is asked in the chat.">
              <select className="select" value={draft.agent.permission_mode} onChange={(e) => set((c) => void (c.agent.permission_mode = e.target.value))}>
                <option value="default">Ask for edits and commands</option>
                <option value="acceptEdits">Accept file edits, ask for commands</option>
                <option value="plan">Plan only</option>
                <option value="auto">Auto (Claude decides what's safe)</option>
              </select>
            </FormRow>
          )}
          {draft.agent.kind === "opencode" && (
            <FormRow label="Server URL" help="Leave empty and tab starts its own OpenCode server.">
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
          <FormRow label="Model" help={draft.agent.kind === "opencode" ? "OpenCode uses its own configured model." : "Runs through your agent. Defaults to claude-haiku-5-5."}>
            <TextInput placeholder="claude-haiku-5-5" disabled={draft.agent.kind === "opencode"} value={titler.model ?? ""} onChange={(e) => setTitler({ model: e.target.value || null })} />
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
