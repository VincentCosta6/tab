import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Badge, Button, Card, FormRow, Meter, Section, StatusDot } from "../../../design/components";
import { Icon } from "../../../design/icons";
import { api } from "../../../lib/api";
import { bytes, timeAgo } from "../../../lib/format";
import type { HubMode } from "../../../lib/types";
import { AccountBadge, SignInButtons, accountKey, cloudPerks, localDrawbacks, useAccount } from "../../account/Account";
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

/** The tab account the cloud hub belongs to. */
function AccountSection() {
  const qc = useQueryClient();
  const { data: status } = useAccount();
  const [busy, setBusy] = useState(false);
  const signOut = async () => {
    setBusy(true);
    try {
      await api.accountSignOut();
      qc.setQueryData(accountKey, { configured: status?.configured ?? false, account: null });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Section title="Account" description="Your tab account. tab cloud keeps your hub under it.">
      <Card>
        {status?.account ? (
          <div className="hub-row">
            <AccountBadge account={status.account} />
            <Button size="sm" variant="ghost" disabled={busy} onClick={signOut}>{busy ? "Signing out…" : "Sign out"}</Button>
          </div>
        ) : (
          <div className="hub-account-out">
            <p className="subtle">Not signed in. Sign in to use tab cloud: it keeps your work private, syncs your devices and works with the mobile app.</p>
            <div style={{ maxWidth: 320 }}><SignInButtons /></div>
          </div>
        )}
      </Card>
    </Section>
  );
}

/** The hubs this person has, which one is in use, adding one and moving between them. */
function HubsSection() {
  const { draft, set } = useDraft();
  const { data: account } = useAccount();
  const local = useQuery({ queryKey: ["hub-status"], queryFn: api.hubStatus, refetchInterval: 2000 });
  const mode: HubMode = draft.hub?.mode ?? "local";
  const signedIn = !!account?.account;
  const [adding, setAdding] = useState(false);
  const [moving, setMoving] = useState(false);
  const use = (m: HubMode) => set((c) => void (c.hub = { mode: m }));
  const h = local.data;

  return (
    <Section
      title="Hubs"
      description="Your queue, chats, projects and workflows sync through a hub. tab uses one at a time; Save applies a change."
      actions={!adding && <Button size="sm" icon="plus" onClick={() => setAdding(true)}>Add a hub</Button>}
    >
      {adding && <AddHub signedIn={signedIn} onClose={() => setAdding(false)} onAdded={() => (use("cloud"), setAdding(false))} />}
      <Card>
        {(signedIn || mode === "cloud") && (
          <div className="hub-row">
            <Icon name="cloud" size={20} />
            <div className="hub-row-main">
              <strong>tab cloud</strong> {mode === "cloud" && <Badge tone="accent">In use</Badge>} <Badge>Coming soon</Badge>
              <div className="subtle">
                {signedIn ? `${account!.account!.email ?? account!.account!.name ?? "Your account"}. ` : "Sign in above to use it. "}
                Until tab cloud launches, your work stays in this computer's hub.
              </div>
            </div>
            {mode !== "cloud" && <Button size="sm" disabled={!signedIn} onClick={() => use("cloud")}>Use this hub</Button>}
          </div>
        )}
        <div className="hub-row">
          <Icon name="folder" size={20} />
          <div className="hub-row-main">
            <strong>This computer</strong> {mode === "local" && <Badge tone="accent">In use</Badge>}{" "}
            {h ? <Badge tone="success">Running</Badge> : !local.isLoading && <Badge tone="danger">Not running</Badge>}
            <div className="subtle">
              {h ? `${h.records.toLocaleString()} records · ${h.head.toLocaleString()} changes · ${bytes(h.storage_bytes)}` : "The hub inside this app."}
            </div>
          </div>
          <div className="hub-row-actions">
            {mode !== "local" && <Button size="sm" onClick={() => use("local")}>Use this hub</Button>}
            <Button size="sm" icon="upload" onClick={() => setMoving((v) => !v)}>Move to tab cloud</Button>
          </div>
        </div>
        {mode === "local" && (
          <ul className="hub-drawbacks hub-row-note">
            {localDrawbacks.map((d) => <li key={d}><Icon name="alert" size={13} /> <span>{d}</span></li>)}
          </ul>
        )}
      </Card>
      {moving && <MoveToCloud signedIn={signedIn} changes={h?.head ?? 0} onClose={() => setMoving(false)} />}
    </Section>
  );
}

/** Adding a hub: tab cloud by signing in; your own server later. */
function AddHub({ signedIn, onClose, onAdded }: { signedIn: boolean; onClose: () => void; onAdded: () => void }) {
  return (
    <Card>
      <div className="hub-add">
        <div className="hub-add-head">
          <strong>Add a hub</strong>
          <Button size="sm" variant="ghost" icon="x" aria-label="Close" onClick={onClose} />
        </div>
        <div className="hub-choices">
          <div className="agent-choice hub-choice chosen">
            <div className="agent-choice-head"><Icon name="cloud" size={20} /><strong>tab cloud</strong><span className="onboarding-recommended">Recommended</span></div>
            <ul className="hub-perks">
              {cloudPerks.map((p) => <li key={p.title}><Icon name={p.icon} size={14} /> <span><strong>{p.title}.</strong> {p.text}</span></li>)}
            </ul>
            <div className="hub-sign-in">
              {signedIn ? <Button variant="primary" onClick={onAdded}>Use tab cloud</Button> : <SignInButtons onSignedIn={onAdded} />}
            </div>
          </div>
          <div className="agent-choice hub-choice disabled" aria-disabled>
            <div className="agent-choice-head"><Icon name="globe" size={20} /><strong>Your own server</strong><Badge>Coming soon</Badge></div>
            <p>Run the hub on a server you control, or connect to another computer's hub by its address and certificate.</p>
          </div>
        </div>
      </div>
    </Card>
  );
}

/** Moving this computer's hub into tab cloud, with its whole history. */
function MoveToCloud({ signedIn, changes, onClose }: { signedIn: boolean; changes: number; onClose: () => void }) {
  return (
    <Card>
      <div className="hub-add">
        <div className="hub-add-head">
          <strong>Move this computer's hub to tab cloud</strong>
          <Button size="sm" variant="ghost" icon="x" aria-label="Close" onClick={onClose} />
        </div>
        <ul className="hub-perks">
          <li><Icon name="check" size={14} /> <span>Copies all {changes.toLocaleString()} changes, in order, with who made each and when.</span></li>
          <li><Icon name="check" size={14} /> <span>Devices synced to this hub pick up where they left off. No one starts over.</span></li>
          <li><Icon name="check" size={14} /> <span>This computer keeps its copy until you remove it, so you can move back.</span></li>
          <li><Icon name="alert" size={14} /> <span>Only into an empty cloud hub. A cloud hub already in use is never merged with this one.</span></li>
        </ul>
        <div className="hub-row">
          <span className="subtle">
            {signedIn ? "tab cloud isn't live yet, so nothing can be moved today." : "Sign in first. tab cloud isn't live yet, so nothing can be moved today."}
          </span>
          <Button variant="primary" icon="upload" disabled title="Available when tab cloud launches">Move to tab cloud</Button>
        </div>
      </div>
    </Card>
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
