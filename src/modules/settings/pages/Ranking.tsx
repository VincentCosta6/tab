import { Card, FormRow, Section, TextArea, TextInput, Toggle } from "../../../design/components";
import { useDraft } from "../draft";

const lines = (s: string) => s.split("\n").map((l) => l.trim()).filter(Boolean);

export function RankingPage() {
  const { draft, set } = useDraft();
  const rules = draft.ranking.rules;
  const ai = draft.ranking.ai;
  return (
    <>
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
    </>
  );
}
