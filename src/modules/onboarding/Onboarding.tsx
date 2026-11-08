import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState, type ReactNode } from "react";
import { Button, Kbd, TextInput, Toggle, cx } from "../../design/components";
import { Brand, TabMark } from "../../design/brands";
import { Icon } from "../../design/icons";
import { api, onAgentUpgrade } from "../../lib/api";
import { GhConnect } from "../connections/GhConnect";
import { useConfig, useFolderProblem, useHarnesses, useSaveConfig } from "../../lib/hooks";
import type { AgentKind, AgentProbe, Harness, HubMode, TabConfig } from "../../lib/types";
import { HarnessIcon, ModelMenu, modelFor } from "../work/ModelMenu";
import { harnessMatches, orderHarnesses } from "../../lib/harnesses";
import { AccountBadge, CloudHubSetup, SignInButtons, cloudPerks, localDrawbacks, useAccount } from "../account/Account";

const steps = ["Welcome", "Agent", "Hub", "Connections", "Ready"];

/** First launch: pick and check an agent, choose where the hub runs, connect a source or two, then go. */
export function Onboarding({ onClose }: { onClose: () => void }) {
  const { data: config } = useConfig();
  const save = useSaveConfig();
  const [step, setStep] = useState(0);
  const [draft, setDraft] = useState<TabConfig | null>(null);
  const qc = useQueryClient();
  const { data: harnesses = [] } = useHarnesses();
  // Looks again every few seconds while no agent is installed, and when the
  // window comes back, so an agent installed in a terminal shows up.
  const probes = useQuery({
    queryKey: ["probes"],
    queryFn: api.detectAgents,
    staleTime: 0,
    enabled: step === 1,
    refetchOnWindowFocus: true,
    refetchInterval: (q) => (q.state.data && !q.state.data.some((p) => p.found) ? 3000 : false),
  });
  const found = probes.data?.filter((p) => p.found).map((p) => p.kind).join(",");
  useEffect(() => {
    void qc.invalidateQueries({ queryKey: ["agent-models"] });
  }, [found, qc]);
  // Nothing chosen yet: start on the first agent this computer has.
  const firstFound = harnesses.find((h) => found?.split(",").includes(h.id))?.id;
  useEffect(() => {
    if (firstFound) setDraft((d) => (d && !d.agent.kind ? { ...d, agent: { ...d.agent, kind: firstFound, model: d.agent.models?.[firstFound] ?? null } } : d));
  }, [firstFound]);
  const sandbox = useQuery({ queryKey: ["sandbox"], queryFn: api.sandboxStatus, staleTime: 60_000, enabled: step === 1 });

  useEffect(() => {
    if (config && !draft) setDraft(structuredClone(config));
  }, [config, draft]);
  const workdirProblem = useFolderProblem(draft?.agent.workdir);
  const [saveError, setSaveError] = useState<string | null>(null);
  // Onboarding starts on the cloud hub; Settings keeps whatever was saved.
  const [hubMode, setHubMode] = useState<HubMode>("cloud");
  const { data: account } = useAccount();
  const signedIn = !!account?.account;
  // Picking tab cloud sets up the person's private hub as soon as they're
  // signed in; Continue waits until it's ready.
  const [hubReady, setHubReady] = useState(false);
  const hubBlocked = step === 2 && hubMode === "cloud" && (!signedIn || !hubReady);

  if (!draft) return null;
  const set = (fn: (c: TabConfig) => void) => {
    const next = structuredClone(draft);
    fn(next);
    setDraft(next);
  };
  const github = draft.connectors.github ?? { enabled: false, settings: {} };

  const finish = async () => {
    setSaveError(null);
    try {
      await save.mutateAsync({ ...draft, hub: { mode: hubMode }, onboarded: true });
      onClose();
    } catch (e) {
      setSaveError(`Couldn't save: ${e}`);
    }
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
            <p className="subtle">tab runs a coding agent on this computer and streams it into the app. Pick one; you can switch later in Settings, and each chat can use any agent you have.</p>
            <AgentPicker
              harnesses={harnesses}
              probes={probes.data}
              loading={probes.isLoading}
              chosen={draft.agent.kind}
              onChoose={(k) => set((c) => void ((c.agent.kind = k), (c.agent.model = c.agent.models?.[k] ?? null)))}
              onInstalled={(k) => (probes.refetch(), set((c) => void (c.agent.kind ??= k)))}
            />
            <div className="onboarding-fields">
              <label>
                Work folder <small>Sessions start here, usually where your repos live.</small>
                <TextInput icon="folder" placeholder="C:\\Users\\you\\code" value={draft.agent.workdir ?? ""} onChange={(e) => set((c) => void (c.agent.workdir = e.target.value || null))} />
                {workdirProblem && <small className="text-danger" role="alert">{workdirProblem}</small>}
              </label>
              <div className="onboarding-field">
                Model <small>Optional. You can switch in any chat.</small>
                {draft.agent.kind ? (
                  <ModelMenu
                    variant="field"
                    only={[draft.agent.kind]}
                    value={{ agent: draft.agent.kind, model: modelFor(draft.agent, draft.agent.kind) }}
                    onChange={(p) => set((c) => void ((c.agent.models = { ...(c.agent.models ?? {}) }), p.model ? (c.agent.models[p.agent] = p.model) : delete c.agent.models[p.agent], (c.agent.model = p.model)))}
                  />
                ) : (
                  <TextInput disabled placeholder="Pick an agent first" />
                )}
              </div>
            </div>
            <SafetyOption
              icon="shield"
              title="Sandbox"
              text={`Agents can't change tab, your shell startup files or SSH keys, can't read stored credentials, and start without tab's tokens.${sandbox.data && !sandbox.data.enforced ? " On this computer the OS can't enforce file limits yet, so these protections are partly limited." : ""}`}
              checked={draft.sandbox.enabled}
              onChange={(v) => set((c) => void (c.sandbox.enabled = v))}
            />
            <SafetyOption
              icon="plug"
              title="Strict mode"
              text="Chats use only the connections you add to tab. Your personal agent setup stays out: MCP servers and connectors from your own account, plugins, hooks and user settings."
              checked={draft.agent.strict !== false}
              onChange={(v) => set((c) => void (c.agent.strict = v))}
            />
            <Button size="sm" variant="ghost" icon="undo" onClick={() => probes.refetch()}>Check again</Button>
          </div>
        )}

        {step === 2 && (
          <div className="onboarding-body">
            <h2>Choose where your work syncs</h2>
            <p className="subtle">Your queue, chats, projects and workflows live in a hub. You can move it later in Settings.</p>
            <div className="hub-choices">
              <HubChoice mode="cloud" chosen={hubMode === "cloud"} onChoose={setHubMode}>
                <div className="agent-choice-head">
                  <Icon name="cloud" size={20} />
                  <strong>tab cloud</strong>
                  <span className="onboarding-recommended">Recommended</span>
                  {hubMode === "cloud" && <Icon name="check" size={16} className="agent-chosen" />}
                </div>
                <ul className="hub-perks">
                  {cloudPerks.map((p) => (
                    <li key={p.title}><Icon name={p.icon} size={14} /> <span><strong>{p.title}.</strong> {p.text}</span></li>
                  ))}
                </ul>
                {hubMode === "cloud" && (
                  <div className="hub-sign-in" onClick={(e) => e.stopPropagation()}>
                    {account?.account ? (
                      <>
                        <AccountBadge account={account.account} />
                        <CloudHubSetup onReady={() => setHubReady(true)} onNotReady={() => setHubReady(false)} />
                      </>
                    ) : (
                      <SignInButtons />
                    )}
                  </div>
                )}
              </HubChoice>
              <HubChoice mode="local" chosen={hubMode === "local"} onChoose={setHubMode}>
                <div className="agent-choice-head">
                  <Icon name="folder" size={20} />
                  <strong>This computer only</strong>
                  {hubMode === "local" && <Icon name="check" size={16} className="agent-chosen" />}
                </div>
                <p>The hub runs inside tab on this computer, with no account. What you give up:</p>
                <ul className="hub-drawbacks">
                  {localDrawbacks.map((d) => (
                    <li key={d}><Icon name="alert" size={13} /> <span>{d}</span></li>
                  ))}
                </ul>
              </HubChoice>
            </div>
          </div>
        )}

        {step === 3 && (
          <div className="onboarding-body">
            <h2>Connect your work</h2>
            <p className="subtle">Start with a source or two. Slack, Gmail, Zoom and Jira are in the marketplace under Connections.</p>
            <div className="onboarding-connector">
              <Brand name="github" size={22} />
              <div style={{ flex: 1 }}>
                <strong>GitHub</strong>
                <p className="subtle">Review requests, CI failures and mentions, plus the built-in VS Code review tool.</p>
                <GhConnect
                  token={String(github.settings.token ?? "")}
                  onToken={(t) => set((c) => void (c.connectors.github = { enabled: !!t, settings: { ...github.settings, token: t } }))}
                  onSignedIn={() => set((c) => void (c.connectors.github = { enabled: true, settings: { ...(c.connectors.github?.settings ?? {}) } }))}
                />
              </div>
            </div>
          </div>
        )}

        {step === 4 && (
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
          {hubBlocked && <span className="subtle">{signedIn ? "Waiting for your private hub. You can also choose This computer only." : "Sign in to use tab cloud, or choose This computer only."}</span>}
          {step < steps.length - 1 ? (
            <Button variant="primary" disabled={hubBlocked} onClick={() => setStep(step + 1)}>{step === 1 && !draft.agent.kind ? "Skip" : "Continue"}</Button>
          ) : (
            <>
              {saveError && <small className="text-danger" role="alert">{saveError}</small>}
              <Button variant="primary" onClick={finish} disabled={save.isPending}>{save.isPending ? "Saving…" : "Open tab"}</Button>
            </>
          )}
        </footer>
      </div>
    </div>
  );
}

/** One place the hub can run, as a card to pick. */
function HubChoice({ mode, chosen, onChoose, children }: { mode: HubMode; chosen: boolean; onChoose: (m: HubMode) => void; children: ReactNode }) {
  return (
    <div role="radio" aria-checked={chosen} tabIndex={0} className={cx("agent-choice", "hub-choice", mode, chosen && "chosen")} onClick={() => onChoose(mode)} onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && onChoose(mode)}>
      {children}
    </div>
  );
}

/** A protection that's on unless the person turns it off. Both can be changed later in Settings. */
function SafetyOption({ icon, title, text, checked, onChange }: { icon: "shield" | "plug"; title: string; text: string; checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <div className={cx("onboarding-safety", icon, checked && "on")}>
      <Icon name={icon} size={18} />
      <div>
        <strong>{title}</strong> <span className="onboarding-recommended">Recommended</span>
        <p className="subtle">{text} You can change this in Settings.</p>
      </div>
      <Toggle label={title} checked={checked} onChange={onChange} />
    </div>
  );
}

/**
 * The agents to choose from. The ones on this computer come first; the rest
 * sit under a search, since there are many. With none installed, the search
 * list is all there is, starting with Claude Code and OpenCode.
 */
function AgentPicker({ harnesses, probes, loading, chosen, onChoose, onInstalled }: { harnesses: Harness[]; probes: AgentProbe[] | undefined; loading: boolean; chosen: AgentKind | null; onChoose: (k: AgentKind) => void; onInstalled: (k: AgentKind) => void }) {
  const [query, setQuery] = useState("");
  const probe = (k: AgentKind) => probes?.find((p) => p.kind === k);
  const installed = new Set(probes?.filter((p) => p.found).map((p) => p.kind) ?? []);
  const ordered = orderHarnesses(harnesses, installed);
  const here = ordered.filter((h) => installed.has(h.id));
  const others = ordered.filter((h) => !installed.has(h.id));
  const matching = others.filter((h) => harnessMatches(h, query));
  const card = (h: Harness) => (
    <AgentChoice key={h.id} harness={h} probe={probe(h.id)} loading={loading} chosen={chosen === h.id} onChoose={() => onChoose(h.id)} onInstalled={() => onInstalled(h.id)} />
  );
  if (loading) return <div className="agent-probe subtle">Looking for agents on this computer…</div>;
  const catalog = (
    <div className="agent-picker-search">
      <TextInput icon="search" placeholder="Search agents" value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search agents" />
      <div className="agent-choices">{matching.map(card)}</div>
      {!matching.length && <p className="subtle">No agent matches “{query.trim()}”.</p>}
    </div>
  );
  if (!here.length) {
    return (
      <div className="agent-picker">
        <p className="agent-picker-note">No agent found on this computer yet. Install one here, or install it in a terminal and it shows up.</p>
        {catalog}
      </div>
    );
  }
  return (
    <div className="agent-picker">
      <h3 className="agent-picker-title">On this computer</h3>
      <div className="agent-choices">{here.map(card)}</div>
      {!!others.length && (
        <details className="agent-picker-more" open={!!chosen && others.some((h) => h.id === chosen)}>
          <summary>Install another agent <span className="subtle">({others.length})</span></summary>
          {catalog}
        </details>
      )}
    </div>
  );
}

/** A harness to pick, with what tab found and, when it's missing, a button that installs it. */
function AgentChoice({ harness, probe, loading, chosen, onChoose, onInstalled }: { harness: Harness; probe: AgentProbe | undefined; loading: boolean; chosen: boolean; onChoose: () => void; onInstalled: () => void }) {
  const [log, setLog] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const install = async () => {
    setBusy(true);
    setError(null);
    const stop = await onAgentUpgrade((line) => setLog(line));
    try {
      await api.agentInstall(harness.id);
      onInstalled();
    } catch (e) {
      setError(String(e));
    } finally {
      stop();
      setBusy(false);
    }
  };
  const command = probe?.install_command;
  return (
    <div role="button" tabIndex={0} className={cx("agent-choice", chosen && "chosen")} onClick={onChoose} onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && onChoose()}>
      <div className="agent-choice-head">
        <HarnessIcon harness={harness} size={22} />
        <strong>{harness.name}</strong>
        {chosen && <Icon name="check" size={16} className="agent-chosen" />}
      </div>
      <p>{harness.pitch}</p>
      {loading ? (
        <div className="agent-probe subtle">Looking for it…</div>
      ) : probe?.found ? (
        <div className="agent-probe text-success"><Icon name="check" size={12} /> Found {probe.version}</div>
      ) : (
        <div className="agent-install" onClick={(e) => e.stopPropagation()}>
          <span className="subtle">Not installed.{command && <> tab runs <code>{command}</code></>}</span>
          {command ? (
            <Button size="sm" icon="plus" disabled={busy} onClick={install}>{busy ? "Installing…" : `Install ${harness.name}`}</Button>
          ) : (
            <span className="subtle">Install it yourself, then it shows up here.</span>
          )}
          {busy && log && <code className="agent-install-log">{log}</code>}
          {error && <span className="text-danger">{error}</span>}
        </div>
      )}
    </div>
  );
}
