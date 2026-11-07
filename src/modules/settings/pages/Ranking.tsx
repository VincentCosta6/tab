import { Button, Card, FormRow, Section, TextArea, TextInput, Toggle } from "../../../design/components";
import { useDecisionProviders, useHarnesses } from "../../../lib/hooks";
import type { DecisionConfig, TabConfig } from "../../../lib/types";
import { useDraft } from "../draft";

const lines = (s: string) => s.split("\n").map((l) => l.trim()).filter(Boolean);

const noDecision: DecisionConfig = { provider: "", model: null, api_keys: {}, key_from: {}, base_url: null };

/** The decision config in the draft, made if the saved config has none. */
const decisionOf = (c: TabConfig) => (c.ranking.decision ??= { ...noDecision, api_keys: {}, key_from: {} });

/** A copy of `m` without `key`. */
const without = (m: Record<string, string>, key: string) => Object.fromEntries(Object.entries(m).filter(([k]) => k !== key));

export function RankingPage() {
  const { draft, set } = useDraft();
  const rules = draft.ranking.rules;
  const ai = draft.ranking.ai;
  const decision = draft.ranking.decision ?? noDecision;
  const providers = useDecisionProviders();
  const harnesses = useHarnesses();
  const current = providers.data?.find((p) => p.id === decision.provider);
  const model = current?.models.find((m) => m.id === decision.model) ?? current?.models[0];
  const harnessName = (id?: string) => (id ? (harnesses.data?.find((h) => h.id === id)?.name ?? id) : undefined);
  const reusable = harnessName(current?.reusable_from[0]);
  const ownKey = !!(current && decision.api_keys[current.id]);
  const reusing = !!(current && decision.key_from[current.id]);
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

      <Section
        title="AI layer"
        description="Reads each item with your instructions and nudges the rules score by up to ±25, with a one-line reason. Verdicts are cached by their inputs, so re-ranking is repeatable."
      >
        <Card>
          <FormRow label="Enabled">
            <Toggle label="AI layer" checked={ai.enabled} onChange={(v) => set((c) => void (c.ranking.ai.enabled = v))} />
          </FormRow>
          <FormRow
            label="Ranks with"
            help={
              current && model
                ? `${model.label} answers typed questions instead of writing text, for about $${model.input_per_mtok.toFixed(3)} per million input tokens and nothing for output. If a call fails, Claude Haiku ranks the item, or the rules score stands.`
                : "Claude Haiku reads each item and writes the reason. Decision models (Jev, Luna) are faster and cheaper, and give a confidence instead of a sentence."
            }
          >
            <select
              className="select"
              value={current && model ? `${current.id}:${model.id}` : "haiku"}
              onChange={(e) =>
                set((c) => {
                  const d = decisionOf(c);
                  if (e.target.value === "haiku") {
                    d.provider = "";
                    d.model = null;
                  } else {
                    const [p, ...m] = e.target.value.split(":");
                    d.provider = p;
                    d.model = m.join(":") || null;
                  }
                })
              }
            >
              <option value="haiku">Claude Haiku</option>
              {providers.data?.map((p) => (
                <optgroup key={p.id} label={p.name}>
                  {p.models.map((m) => (
                    <option key={m.id} value={`${p.id}:${m.id}`}>
                      {m.label} via {p.name}
                    </option>
                  ))}
                </optgroup>
              ))}
            </select>
          </FormRow>
          {current ? (
            <>
              {reusable && !ownKey && (
                <FormRow
                  label="Key"
                  help={
                    reusing
                      ? `Uses the ${current.name} key ${reusable} already has, billed to the same account.`
                      : `${reusable} already has a ${current.name} key. tab reads it only if you say so.`
                  }
                >
                  <Button
                    variant={reusing ? "secondary" : "primary"}
                    onClick={() =>
                      set((c) => {
                        const d = decisionOf(c);
                        d.key_from = reusing ? without(d.key_from, current.id) : { ...d.key_from, [current.id]: current.reusable_from[0] };
                      })
                    }
                  >
                    {reusing ? `Stop using ${reusable}'s key` : `Use ${reusable}'s key`}
                  </Button>
                </FormRow>
              )}
              <FormRow
                label={`${current.name} API key`}
                help={
                  <>
                    Or set {current.env} before launching.{" "}
                    <a href={current.key_url} target="_blank" rel="noreferrer">
                      Get a key
                    </a>
                  </>
                }
              >
                <TextInput
                  type="password"
                  autoComplete="off"
                  value={decision.api_keys[current.id] ?? ""}
                  onChange={(e) =>
                    set((c) => {
                      const d = decisionOf(c);
                      d.api_keys = e.target.value ? { ...d.api_keys, [current.id]: e.target.value } : without(d.api_keys, current.id);
                    })
                  }
                />
              </FormRow>
            </>
          ) : (
            <>
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
            </>
          )}
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
