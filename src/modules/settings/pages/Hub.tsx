import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useEffect, useState } from "react";
import { Badge, Button, Card, FormRow, Meter, Section, StatusDot, TextInput } from "../../../design/components";
import { Icon } from "../../../design/icons";
import { api, onHubMove } from "../../../lib/api";
import { bytes, timeAgo } from "../../../lib/format";
import { HubState, RunnerState, asFailure } from "../../../lib/protocol/cloud";
import type { HubMode, Tone } from "../../../lib/types";
import { CloudHubLine, CloudHubSetup, SignInButtons, localDrawbacks, useAccount, useCloudHub } from "../../account/Account";
import { useDraft } from "../draft";

/**
 * The hub: where your work is kept (this computer or tab cloud, and moving
 * between them), the devices and cloud runner on it, and what it holds.
 */
export function HubPage() {
  const local = useQuery({ queryKey: ["hub-status"], queryFn: api.hubStatus, refetchInterval: 2000 });
  const moved = !!local.data?.cloud_hub;
  return (
    <>
      <HubsSection />
      <RunnersSection />
      {moved && <CloudRunnerSection />}
      <WhatSyncsSection />
      <LocalHubDetails />
    </>
  );
}

/**
 * Where this person's work is kept, said plainly, and the one thing to do
 * next. tab cloud takes three steps (sign in, set up the private hub, move
 * the work), each done right here; the current one has the only button.
 */
function HubsSection() {
  const { draft, set } = useDraft();
  const { data: account } = useAccount();
  const local = useQuery({ queryKey: ["hub-status"], queryFn: api.hubStatus, refetchInterval: 2000 });
  const signedIn = !!account?.account;
  const cloudHub = useCloudHub(signedIn);
  const [settingUp, setSettingUp] = useState(false);
  const h = local.data;
  const moved = !!h?.cloud_hub;
  const mode: HubMode = moved ? "cloud" : (draft.hub?.mode ?? "local");
  const hubReady = cloudHub.data?.hub?.state === HubState.READY;
  const use = (m: HubMode) => set((c) => void (c.hub = { ...c.hub, mode: m }));

  const headline = moved
    ? "Your work syncs with tab cloud."
    : mode === "cloud"
      ? "You picked tab cloud, but your work is still only on this computer."
      : "Your work is kept on this computer only.";
  const detail = moved
    ? h!.cloud_connected
      ? "Every computer you sign in on sees the same queue, chats and projects."
      : "Not connected right now. Changes you make wait here and sync when it reconnects."
    : mode === "cloud"
      ? "Finish the steps below to start syncing."
      : "Nothing syncs, and other computers and your phone can't see it.";

  return (
    <Section title="Where your work is kept" description="Your queue, chats, projects and workflows live in one place, called a hub.">
      <Card>
        <div className="hub-row">
          <Icon name={moved || mode === "cloud" ? "cloud" : "folder"} size={20} />
          <div className="hub-row-main">
            <strong>{headline}</strong> {moved && (h!.cloud_connected ? <Badge tone="success">Connected</Badge> : <Badge tone="warning">Offline</Badge>)}
            <div className="subtle">{detail}</div>
          </div>
          {!moved && mode === "local" && <Button size="sm" variant="primary" icon="cloud" onClick={() => use("cloud")}>Use tab cloud</Button>}
          {!moved && mode === "cloud" && <Button size="sm" variant="ghost" onClick={() => use("local")}>Keep it on this computer</Button>}
        </div>

        {!moved && mode === "cloud" && (
          <ol className="hub-steps">
            <HubStep n={1} done={signedIn} title="Sign in to your tab account">
              {!signedIn && <div style={{ maxWidth: 320 }}><SignInButtons /></div>}
            </HubStep>
            <HubStep n={2} done={hubReady} title="Set up your private hub" waiting={!signedIn}>
              {signedIn && !hubReady && (settingUp ? (
                <CloudHubSetup />
              ) : (
                <>
                  <p className="subtle">Your own hub in tab cloud. Only your devices can reach it. Takes a few seconds and starts your 14-day free trial.</p>
                  <Button size="sm" variant="primary" onClick={() => setSettingUp(true)}>Set up my private hub</Button>
                </>
              ))}
            </HubStep>
            <HubStep n={3} done={false} title="Move your work into it" waiting={!hubReady}>
              {hubReady && <MoveToCloud changes={h?.head ?? 0} />}
            </HubStep>
          </ol>
        )}

        {moved && <CloudHubLine />}
        {moved && <MoveToLocal />}
        {mode === "local" && !moved && (
          <ul className="hub-drawbacks hub-row-note">
            {localDrawbacks.map((d) => <li key={d}><Icon name="alert" size={13} /> <span>{d}</span></li>)}
          </ul>
        )}
      </Card>
    </Section>
  );
}

/** One step toward tab cloud: done, the current one (with its button), or still to come. */
function HubStep({ n, title, done, waiting = false, children }: { n: number; title: string; done: boolean; waiting?: boolean; children?: ReactNode }) {
  return (
    <li className={done ? "done" : waiting ? "waiting" : "current"}>
      <span className="hub-step-mark">{done ? <Icon name="check" size={11} /> : n}</span>
      <div>
        <strong>{title}</strong>
        {!done && !waiting && children && <div className="hub-step-body">{children}</div>}
      </div>
    </li>
  );
}

/** Moving this computer's work into the private hub, with its whole history. */
function MoveToCloud({ changes }: { changes: number }) {
  const qc = useQueryClient();
  const [state, setState] = useState<
    { kind: "idle" } | { kind: "moving"; done: number; total: number } | { kind: "failed"; message: string }
  >({ kind: "idle" });
  useEffect(() => {
    let off: (() => void) | undefined;
    void onHubMove(([done, total]) => setState({ kind: "moving", done, total })).then((f) => (off = f));
    return () => off?.();
  }, []);
  const move = async () => {
    setState({ kind: "moving", done: 0, total: changes });
    try {
      await api.hubMoveToCloud();
      // The page now shows it syncing; the saved config already says cloud.
      void qc.invalidateQueries({ queryKey: ["hub-status"] });
      void qc.invalidateQueries({ queryKey: ["config"] });
    } catch (e) {
      setState({ kind: "failed", message: asFailure(e).message });
    }
  };
  return (
    <>
      <p className="subtle">
        Copies all {changes.toLocaleString()} changes, in order, with who made each and when. This computer keeps its copy.
        If another computer already moved its work there, this one joins it instead.
      </p>
      <div className="cloud-setup-actions" role="status">
        <Button size="sm" variant="primary" icon="upload" disabled={state.kind === "moving"} onClick={() => void move()}>
          {state.kind === "failed" ? "Try again" : state.kind === "moving" ? "Moving…" : "Move my work"}
        </Button>
        {state.kind === "moving" && <span className="subtle">{state.done.toLocaleString()} of {state.total.toLocaleString()}</span>}
        {state.kind === "failed" && <span className="text-danger">Couldn't move: {state.message}</span>}
      </div>
      {state.kind === "moving" && state.total > 0 && <Meter value={state.done} max={state.total} />}
    </>
  );
}

/** Bringing the work back onto this computer, after asking. */
function MoveToLocal() {
  const qc = useQueryClient();
  const [state, setState] = useState<{ kind: "idle" | "asking" | "moving" } | { kind: "failed"; message: string }>({ kind: "idle" });
  const move = async () => {
    setState({ kind: "moving" });
    try {
      await api.hubMoveToLocal();
      setState({ kind: "idle" });
      void qc.invalidateQueries({ queryKey: ["hub-status"] });
      void qc.invalidateQueries({ queryKey: ["config"] });
    } catch (e) {
      setState({ kind: "failed", message: String(e) });
    }
  };
  if (state.kind === "idle") {
    return (
      <div className="hub-row">
        <div className="hub-row-main subtle">Want it back on this computer only? Your work comes back with everything that changed in tab cloud.</div>
        <Button size="sm" variant="ghost" icon="monitor" onClick={() => setState({ kind: "asking" })}>Move back to this computer</Button>
      </div>
    );
  }
  return (
    <div className="hub-row">
      <div className="hub-row-main">
        <strong>Keep your work on this computer only?</strong>
        <div className="subtle">
          This computer stops syncing. Connections set to run in tab cloud run here again. tab cloud keeps its copy, and your other computers stay on it.
        </div>
        {state.kind === "failed" && <div className="text-danger">Couldn't move it back: {state.message}</div>}
      </div>
      <div className="hub-row-actions">
        <Button size="sm" variant="ghost" disabled={state.kind === "moving"} onClick={() => setState({ kind: "idle" })}>Cancel</Button>
        <Button size="sm" variant="primary" disabled={state.kind === "moving"} onClick={() => void move()}>{state.kind === "moving" ? "Moving…" : "Move it back"}</Button>
      </div>
    </div>
  );
}

/** Every computer on this hub, and the cloud runner, with whether each is awake. */
function RunnersSection() {
  const runners = useQuery({ queryKey: ["runners"], queryFn: api.runners, refetchInterval: 15_000 });
  const list = runners.data ?? [];
  if (!list.length) return null;
  return (
    <Section title="Devices" description="Where chats, connections and workflow agents run. Each one picks up work it can do while it's awake.">
      <Card>
        {list.map((r) => (
          <div key={r.id} className="hub-row">
            <Icon name={r.kind === "cloud" ? "cloud" : "monitor"} size={18} />
            <div className="hub-row-main">
              <strong>{r.this_app ? `${r.name || "This computer"} (this one)` : r.kind === "cloud" ? "tab cloud runner" : r.name || r.id}</strong>
              <div className="subtle">
                {[
                  r.harnesses.length ? r.harnesses.map(harnessName).join(", ") : "No harnesses",
                  r.connections.length ? `${r.connections.length} connection${r.connections.length > 1 ? "s" : ""}` : null,
                  r.repos.length ? `${r.repos.length} repo${r.repos.length > 1 ? "s" : ""} checked out` : null,
                ].filter(Boolean).join(" · ")}
              </div>
            </div>
            {r.awake ? <Badge tone="success">Awake</Badge> : <Badge>{r.seen_at ? `Seen ${timeAgo(r.seen_at)}` : "Asleep"}</Badge>}
          </div>
        ))}
      </Card>
    </Section>
  );
}

const harnessKeys: { harness: string; name: string; vars: { name: string; help: string }[] }[] = [
  { harness: "claude_code", name: "Claude Code", vars: [
    { name: "CLAUDE_CODE_OAUTH_TOKEN", help: "Uses your Claude subscription. Make one with `claude setup-token`." },
    { name: "ANTHROPIC_API_KEY", help: "Bills your Anthropic API account instead." },
  ] },
  { harness: "codex", name: "Codex", vars: [{ name: "OPENAI_API_KEY", help: "From platform.openai.com." }] },
  { harness: "opencode", name: "OpenCode", vars: [
    { name: "ANTHROPIC_API_KEY", help: "For Anthropic models." },
    { name: "OPENAI_API_KEY", help: "For OpenAI models." },
    { name: "OPENROUTER_API_KEY", help: "For any model on OpenRouter." },
  ] },
  { harness: "pi", name: "Pi", vars: [
    { name: "ANTHROPIC_API_KEY", help: "For Anthropic models." },
    { name: "OPENAI_API_KEY", help: "For OpenAI models." },
  ] },
];

function harnessName(id: string): string {
  return harnessKeys.find((h) => h.harness === id)?.name ?? id;
}

const runnerTone: Record<RunnerState, { tone: Tone; label: string }> = {
  [RunnerState.UNSPECIFIED]: { tone: "neutral", label: "Unknown" },
  [RunnerState.ASLEEP]: { tone: "neutral", label: "Asleep" },
  [RunnerState.STARTING]: { tone: "info", label: "Starting" },
  [RunnerState.RUNNING]: { tone: "success", label: "Running" },
  [RunnerState.NOT_ON_PLAN]: { tone: "warning", label: "Not on your plan" },
};

/**
 * The hub's cloud runner (paid plans): tab running in tab cloud, so
 * connections set to run there and workflow agents keep going while every
 * computer is off. It uses your own harnesses with the keys you give it here.
 */
function CloudRunnerSection() {
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ["cloud-runner"], queryFn: api.cloudRunnerStatus, refetchInterval: 15_000, retry: false });
  const names = useQuery({ queryKey: ["cloud-secrets"], queryFn: api.cloudSecretNames, retry: false });
  const [waking, setWaking] = useState(false);
  const wake = async () => {
    setWaking(true);
    try {
      qc.setQueryData(["cloud-runner"], await api.cloudRunnerWake());
    } catch {
      void status.refetch();
    } finally {
      setWaking(false);
    }
  };
  const st = status.data;
  const look = st ? runnerTone[st.state] : null;
  const has = new Set(names.data ?? []);
  const setSecret = async (name: string, value: string | null) => {
    qc.setQueryData(["cloud-secrets"], await api.cloudSecretSet(name, value));
  };
  return (
    <Section title="Cloud runner" description="tab, running in tab cloud for you. It runs connections set to run there, and workflow agents allowed in the cloud, with your own harnesses and keys.">
      <Card>
        <div className="hub-row">
          <Icon name="server" size={20} />
          <div className="hub-row-main">
            <strong>{look ? look.label : status.isLoading ? "Checking…" : "Unavailable"}</strong> {look && <StatusDot tone={look.tone} />}
            <div className="subtle">
              {status.error ? asFailure(status.error).message : st?.detail || (st?.state === RunnerState.ASLEEP ? "It starts by itself when there's work." : "")}
            </div>
          </div>
          {st && st.state !== RunnerState.NOT_ON_PLAN && st.state !== RunnerState.RUNNING && (
            <Button size="sm" variant="ghost" icon="zap" disabled={waking} onClick={() => void wake()}>{waking ? "Starting…" : "Start now"}</Button>
          )}
        </div>
      </Card>
      <Card>
        <p className="subtle" style={{ margin: "0 0 var(--space-2)" }}>
          Keys are kept encrypted in your hub and only its cloud runner can read them. You can't see them again here, only replace or remove them.
        </p>
        {harnessKeys.map((h) => h.vars.map((v) => (
          <SecretRow key={`${h.harness}/${v.name}`} label={`${h.name} · ${v.name}`} help={v.help} set={has.has(`harness/${h.harness}/${v.name}`)}
            onSave={(val) => setSecret(`harness/${h.harness}/${v.name}`, val)} />
        )))}
        <SecretRow label="GitHub access" help="A token that can push branches and open pull requests on the repos agents work on." set={has.has("git/token")} onSave={(val) => setSecret("git/token", val)} />
      </Card>
    </Section>
  );
}

function SecretRow({ label, help, set, onSave }: { label: string; help: string; set: boolean; onSave: (value: string | null) => Promise<void> }) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const save = async (v: string | null) => {
    setBusy(true);
    setError(null);
    try {
      await onSave(v);
      setEditing(false);
      setValue("");
    } catch (e) {
      setError(asFailure(e).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <FormRow label={label} help={error ?? help}>
      {editing ? (
        <form className="secret-row" onSubmit={(e) => (e.preventDefault(), void save(value))}>
          <TextInput type="password" autoComplete="off" autoFocus value={value} onChange={(e) => setValue(e.target.value)} />
          <Button size="sm" variant="primary" type="submit" disabled={busy || !value.trim()}>Save</Button>
          <Button size="sm" variant="ghost" type="button" onClick={() => setEditing(false)}>Cancel</Button>
        </form>
      ) : (
        <div className="secret-row">
          {set ? <Badge tone="success">Set</Badge> : <Badge>Not set</Badge>}
          <Button size="sm" variant="ghost" onClick={() => setEditing(true)}>{set ? "Replace" : "Add"}</Button>
          {set && <Button size="sm" variant="ghost" disabled={busy} onClick={() => void save(null)}>Remove</Button>}
        </div>
      )}
    </FormRow>
  );
}

/**
 * What's in the hub, by kind, so it's plain that projects, notes and the
 * knowledge graph sync with the queue, and what stays on this computer.
 * Counted from this computer's copy, which matches the hub once nothing is
 * waiting.
 */
function WhatSyncsSection() {
  const status = useQuery({ queryKey: ["hub-status"], queryFn: api.hubStatus, refetchInterval: 2000 });
  const h = status.data;
  if (!h) return null;
  const where = h.cloud_hub ? "tab cloud" : "this computer's hub";
  return (
    <Section title="What syncs" description={`Everything here is kept in ${where}, so every device on it sees the same.`}>
      <Card>
        {h.synced.map((c) => (
          <FormRow key={c.what} label={c.what}>
            <span className="mono">{c.count.toLocaleString()}</span>
          </FormRow>
        ))}
        <FormRow label="Sync" help="Local changes not yet in the hub, and the last hub change this computer has.">
          {h.pending === 0
            ? <Badge tone="success">Up to date · change {h.pulled_to.toLocaleString()}</Badge>
            : <Badge tone="warning">{h.pending.toLocaleString()} waiting</Badge>}
        </FormRow>
        <p className="subtle" style={{ margin: "var(--space-2) 0 0" }}>
          Stays on this computer: caches, search indexes (rebuilt on each device), usage counts, and agents' tool output.
        </p>
      </Card>
    </Section>
  );
}

/** The hub running inside the app: where it is, who's connected, what it holds. */
function LocalHubDetails() {
  const status = useQuery({ queryKey: ["hub-status"], queryFn: api.hubStatus, refetchInterval: 2000 });
  const h = status.data;
  if (status.isLoading) return null;
  if (!h) {
    return (
      <Section title="This computer's hub" description="The hub inside this app.">
        <Card><FormRow label="Status"><Badge tone="danger">Not running</Badge></FormRow></Card>
      </Section>
    );
  }
  const others = h.connections.filter((c) => !c.this_app);
  const cpuOfMachine = h.cpu_percent == null ? null : h.cpu_percent / Math.max(1, h.cpus);
  const moved = !!h.cloud_hub;
  const details = (
    <>
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
    </>
  );
  if (moved) {
    return (
      <Section
        title="This computer's old hub"
        description="Not in use since your work moved to tab cloud. It keeps the copy it had at the move and no longer changes, so its numbers leave out anything newer. Nothing is missing: see What syncs above."
      >
        <details className="hub-old-details">
          <summary className="subtle">Show its details</summary>
          {details}
        </details>
      </Section>
    );
  }
  return (
    <Section
      title="This computer's hub"
      description="It runs inside this app, so what it holds stays on this computer. Other devices on your network can connect to it here."
    >
      {details}
    </Section>
  );
}
