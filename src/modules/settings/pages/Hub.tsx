import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useEffect, useState } from "react";
import { Badge, Button, Card, FormRow, Meter, Section, StatusDot } from "../../../design/components";
import { Icon } from "../../../design/icons";
import { api, onHubMove } from "../../../lib/api";
import { bytes, timeAgo } from "../../../lib/format";
import { HubState, asFailure } from "../../../lib/protocol/cloud";
import type { HubMode } from "../../../lib/types";
import { AccountBadge, CloudHubLine, CloudHubSetup, SignInButtons, accountKey, localDrawbacks, useAccount, useCloudHub } from "../../account/Account";
import { useDraft } from "../draft";

export function HubPage() {
  return (
    <>
      <AccountSection />
      <HubsSection />
      <LocalHubDetails />
    </>
  );
}

/** The tab account the cloud hub belongs to. Signing in is step 1 below, so this shows only once signed in. */
function AccountSection() {
  const qc = useQueryClient();
  const { data: status } = useAccount();
  const [busy, setBusy] = useState(false);
  if (!status?.account) return null;
  const signOut = async () => {
    setBusy(true);
    try {
      await api.accountSignOut();
      qc.setQueryData(accountKey, { configured: status.configured, account: null });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Section title="Account" description="Your tab account. tab cloud keeps your hub under it.">
      <Card>
        <div className="hub-row">
          <AccountBadge account={status.account} />
          <Button size="sm" variant="ghost" disabled={busy} onClick={signOut}>{busy ? "Signing out…" : "Sign out"}</Button>
        </div>
      </Card>
    </Section>
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
  return (
    <Section
      title="This computer's hub"
      description="It runs inside this app, so what it holds stays on this computer. Other devices on your network can connect to it here."
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
