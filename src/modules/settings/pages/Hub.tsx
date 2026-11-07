import { useQuery } from "@tanstack/react-query";
import { Badge, Card, FormRow, Meter, Section, StatusDot } from "../../../design/components";
import { api } from "../../../lib/api";
import { bytes, timeAgo } from "../../../lib/format";

/** The hub running inside the app: where it is, who's connected, what it holds. */
export function HubPage() {
  const status = useQuery({ queryKey: ["hub-status"], queryFn: api.hubStatus, refetchInterval: 2000 });
  const h = status.data;
  if (status.isLoading) return null;
  if (!h) {
    return (
      <Section title="Hub" description="The hub keeps your work in sync across devices.">
        <Card><FormRow label="Status"><Badge tone="danger">Not running</Badge></FormRow></Card>
      </Section>
    );
  }
  const others = h.connections.filter((c) => !c.this_app);
  const cpuOfMachine = h.cpu_percent == null ? null : h.cpu_percent / Math.max(1, h.cpus);
  return (
    <Section
      title="Hub"
      description="Your queue, projects, context and workflows sync through the hub. It runs inside this app, so everything stays on this computer. Other devices can connect to it here."
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
