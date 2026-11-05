import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { Button, Kbd, TextInput, Toggle, cx } from "../../design/components";
import { Brand, TabMark } from "../../design/brands";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { useConfig, useSaveConfig } from "../../lib/hooks";
import type { AgentKind, AgentProbe, TabConfig } from "../../lib/types";

const agents: { kind: AgentKind; name: string; brand: string; pitch: string; install: string }[] = [
  {
    kind: "opencode",
    name: "OpenCode",
    brand: "opencode",
    pitch: "Open source and works with any model provider. tab starts its server and streams every session.",
    install: "npm i -g opencode-ai",
  },
  {
    kind: "claude_code",
    name: "Claude Code",
    brand: "claude",
    pitch: "If you use Claude, tab drives the official Claude Code CLI with your own login, so your usage stays within Anthropic's terms.",
    install: "npm i -g @anthropic-ai/claude-code",
  },
];

const steps = ["Welcome", "Agent", "Connections", "Ready"];

/** First launch: pick and check an agent, connect a source or two, then go. */
export function Onboarding({ onClose }: { onClose: () => void }) {
  const { data: config } = useConfig();
  const save = useSaveConfig();
  const [step, setStep] = useState(0);
  const [draft, setDraft] = useState<TabConfig | null>(null);
  const probes = useQuery({ queryKey: ["probes"], queryFn: api.detectAgents, staleTime: 0, gcTime: 0, enabled: step === 1 });

  useEffect(() => {
    if (config && !draft) setDraft(structuredClone(config));
  }, [config, draft]);

  if (!draft) return null;
  const set = (fn: (c: TabConfig) => void) => {
    const next = structuredClone(draft);
    fn(next);
    setDraft(next);
  };
  const probe = (k: AgentKind): AgentProbe | undefined => probes.data?.find((p) => p.kind === k);
  const github = draft.connectors.github ?? { enabled: false, settings: {} };

  const finish = async () => {
    await save.mutateAsync({ ...draft, onboarded: true });
    onClose();
  };

  return (
    <div className="onboarding-backdrop">
      <div className="onboarding">
        <ol className="onboarding-steps">
          {steps.map((s, i) => (
            <li key={s} className={cx(i === step && "active", i < step && "done")}>
              <span>{i < step ? <Icon name="check" size={11} /> : i + 1}</span> {s}
            </li>
          ))}
        </ol>

        {step === 0 && (
          <div className="onboarding-body welcome">
            <TabMark size={56} />
            <h1>tab</h1>
            <p className="tagline"><strong>T</strong>AB <strong>A</strong>nnihilates <strong>B</strong>acklogs.</p>
            <p className="subtle">One queue for everything that needs you, ranked. Open anything and hand it to an agent: it works in a chat right next to the item, with the context rolled up for it.</p>
          </div>
        )}

        {step === 1 && (
          <div className="onboarding-body">
            <h2>Set up your agent</h2>
            <p className="subtle">tab runs your coding agent on this computer and streams it into the app. Pick one; you can switch later in Settings.</p>
            <div className="agent-choices">
              {agents.map((a) => {
                const p = probe(a.kind);
                const chosen = draft.agent.kind === a.kind;
                return (
                  <button key={a.kind} className={cx("agent-choice", chosen && "chosen")} onClick={() => set((c) => void (c.agent.kind = a.kind))}>
                    <div className="agent-choice-head">
                      <Brand name={a.brand} size={22} />
                      <strong>{a.name}</strong>
                      {chosen && <Icon name="check" size={16} className="agent-chosen" />}
                    </div>
                    <p>{a.pitch}</p>
                    <div className={cx("agent-probe", p?.found ? "text-success" : "subtle")}>
                      {probes.isLoading ? "Looking for it…" : p?.found ? <><Icon name="check" size={12} /> Found {p.version}</> : <>Not found. Install: <code>{a.install}</code></>}
                    </div>
                  </button>
                );
              })}
            </div>
            <div className="onboarding-fields">
              <label>
                Work folder <small>Sessions start here, usually where your repos live.</small>
                <TextInput icon="folder" placeholder="C:\\Users\\you\\code" value={draft.agent.workdir ?? ""} onChange={(e) => set((c) => void (c.agent.workdir = e.target.value || null))} />
              </label>
              <label>
                Model <small>Optional. {draft.agent.kind === "opencode" ? "provider/model, e.g. anthropic/claude-sonnet-5-5" : "e.g. sonnet or opus"}</small>
                <TextInput placeholder="Agent default" value={draft.agent.model ?? ""} onChange={(e) => set((c) => void (c.agent.model = e.target.value || null))} />
              </label>
            </div>
            <Button size="sm" variant="ghost" icon="undo" onClick={() => probes.refetch()}>Check again</Button>
          </div>
        )}

        {step === 2 && (
          <div className="onboarding-body">
            <h2>Connect your work</h2>
            <p className="subtle">Start with a source or two. Slack, Gmail, Zoom and Jira are in the marketplace under Connections.</p>
            <div className="onboarding-connector">
              <Brand name="github" size={22} />
              <div style={{ flex: 1 }}>
                <strong>GitHub</strong>
                <p className="subtle">Review requests, CI failures and mentions, plus the built-in VS Code review tool.</p>
                <TextInput
                  type="password"
                  placeholder="Personal access token (notifications, repo read)"
                  value={String(github.settings.token ?? "")}
                  onChange={(e) => set((c) => void (c.connectors.github = { enabled: !!e.target.value, settings: { ...github.settings, token: e.target.value } }))}
                />
              </div>
            </div>
            <div className="onboarding-connector">
              <Icon name="sparkles" size={22} />
              <div style={{ flex: 1 }}>
                <strong>Sample data</strong>
                <p className="subtle">Realistic work from every kind of source, so the queue has something in it while you try tab.</p>
              </div>
              <Toggle label="Sample data" checked={!!draft.connectors.demo?.enabled} onChange={(v) => set((c) => void (c.connectors.demo = { enabled: v, settings: {} }))} />
            </div>
          </div>
        )}

        {step === 3 && (
          <div className="onboarding-body welcome">
            <Icon name="check-square" size={40} />
            <h2>You're set</h2>
            <ul className="tips">
              <li><Kbd>J</Kbd> <Kbd>K</Kbd> move through the queue, <Kbd>E</Kbd> done, <Kbd>S</Kbd> snooze</li>
              <li>Hover a row to snooze or finish it without opening it</li>
              <li>Open anything and chat: the agent gets the item and its context</li>
              <li><Kbd>N</Kbd> starts a new chat. "Work on PAY-12" becomes a task with its own session</li>
            </ul>
          </div>
        )}

        <footer className="onboarding-foot">
          {step > 0 ? <Button variant="ghost" onClick={() => setStep(step - 1)}>Back</Button> : <span />}
          {step === 1 && !draft.agent.kind && <span className="subtle">Pick one to chat with agents, or skip for now.</span>}
          {step < steps.length - 1 ? (
            <Button variant="primary" onClick={() => setStep(step + 1)}>{step === 1 && !draft.agent.kind ? "Skip" : "Continue"}</Button>
          ) : (
            <Button variant="primary" onClick={finish} disabled={save.isPending}>{save.isPending ? "Saving…" : "Open tab"}</Button>
          )}
        </footer>
      </div>
    </div>
  );
}
