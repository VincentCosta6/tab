import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { openOnboarding } from "../work/Chat";
import { keys } from "../../lib/query";
import { Button, Card, FormRow, PageHeader, Section, TextArea, TextInput, Toggle } from "../../design/components";
import { api } from "../../lib/api";
import type { TabConfig } from "../../lib/types";

const lines = (s: string) => s.split("\n").map((l) => l.trim()).filter(Boolean);

export function SettingsTab() {
  const [saved, setSaved] = useState<TabConfig | null>(null);
  const [draft, setDraft] = useState<TabConfig | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const qc = useQueryClient();
  const mcp = useQuery({ queryKey: ["mcp"], queryFn: api.mcpInfo });

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
            <select className="select" value={draft.agent.kind ?? ""} onChange={(e) => set((c) => void (c.agent.kind = (e.target.value || null) as typeof c.agent.kind))}>
              <option value="">Not set up</option>
              <option value="opencode">OpenCode</option>
              <option value="claude_code">Claude Code</option>
            </select>
          </FormRow>
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

      <Section title="AI layer" description={`Claude reads each item with your instructions and nudges the rules score by up to ±25, with a one-line reason. Verdicts are cached by their inputs, so re-ranking is repeatable.`}>
        <Card>
          <FormRow label="Enabled">
            <Toggle label="AI layer" checked={ai.enabled} onChange={(v) => set((c) => void (c.ranking.ai.enabled = v))} />
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
