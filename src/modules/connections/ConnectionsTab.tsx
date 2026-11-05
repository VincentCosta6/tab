import { useCallback, useEffect, useState } from "react";
import { Badge, Button, Card, PageHeader, Section, SourceIcon, StatusDot, Toggle } from "../../design/components";
import { api } from "../../lib/api";
import { useApp } from "../../lib/app-context";
import { timeAgo, timeUntil } from "../../lib/format";
import type { ConnectorInfo, ConnectorStatus, TabConfig, Tone } from "../../lib/types";
import { ApiUsage } from "./ApiUsage";

function statusView(s: ConnectorStatus, available: boolean): { tone: Tone; text: string } {
  if (!available) return { tone: "neutral", text: "Coming soon" };
  switch (s.state) {
    case "disabled": return { tone: "neutral", text: "Not connected" };
    case "starting": return { tone: "info", text: "Starting…" };
    case "running": return { tone: "success", text: s.last_sync ? `Synced ${timeAgo(s.last_sync)}` : "Connected" };
    case "rate_limited": return { tone: "warning", text: `Rate limited, resumes in ${timeUntil(s.until)}` };
    case "error": return { tone: "danger", text: s.message };
    case "stopped": return { tone: "neutral", text: "Stopped" };
  }
}

export function ConnectionsTab() {
  const { connectors, refreshConnectors } = useApp();
  const [config, setConfig] = useState<TabConfig | null>(null);

  useEffect(() => {
    api.getConfig().then(setConfig);
  }, []);

  const save = useCallback(
    async (next: TabConfig) => {
      setConfig(await api.saveConfig(next));
      await refreshConnectors();
    },
    [refreshConnectors],
  );

  const connected = connectors.filter((c) => c.enabled).length;
  return (
    <div className="page">
      <PageHeader title="Connections" subtitle={`${connected} connected. Everything tab knows comes in through a connection.`} />
      <Section title="Sources" description="Each connection turns activity in another tool into ranked work.">
        <div className="connector-grid">
          {connectors.map((c) => (
            <ConnectorCard key={c.id} connector={c} config={config} onSave={save} />
          ))}
        </div>
      </Section>
      <ApiUsage />
    </div>
  );
}

function ConnectorCard({ connector: c, config, onSave }: { connector: ConnectorInfo; config: TabConfig | null; onSave: (c: TabConfig) => Promise<void> }) {
  const saved = config?.connectors[c.id];
  const [draft, setDraft] = useState<Record<string, unknown>>(saved?.settings ?? {});
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState(false);
  const status = statusView(c.status, c.available);

  useEffect(() => setDraft(saved?.settings ?? {}), [saved]);

  const write = async (enabled: boolean, settings: Record<string, unknown>) => {
    if (!config) return;
    setBusy(true);
    try {
      await onSave({ ...config, connectors: { ...config.connectors, [c.id]: { enabled, settings } } });
      setEditing(false);
    } finally {
      setBusy(false);
    }
  };

  const missingRequired = c.settings.some((s) => s.required && !String(draft[s.key] ?? "").trim());

  return (
    <Card className={c.available ? "connector" : "connector unavailable"}>
      <div className="connector-head">
        <SourceIcon icon={c.icon} tone={c.enabled ? "accent" : "neutral"} size={34} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <div className="connector-name">{c.name}</div>
          <div className="connector-status" title={status.text}>
            <StatusDot tone={status.tone} />
            <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{status.text}</span>
          </div>
        </div>
        {c.available && (
          <Toggle
            label={`Enable ${c.name}`}
            checked={c.enabled}
            onChange={(on) => (on && c.settings.length && missingRequired ? setEditing(true) : write(on, draft))}
          />
        )}
      </div>
      <p className="connector-desc">{c.description}</p>
      {c.kinds.length > 0 && (
        <div className="block-badges">
          {c.kinds.slice(0, 4).map((k) => (
            <Badge key={k.kind} tone={k.tone}>{k.label}</Badge>
          ))}
          {c.kinds.length > 4 && <Badge>+{c.kinds.length - 4}</Badge>}
        </div>
      )}
      {c.available && c.settings.length > 0 && !editing && (
        <div className="connector-foot">
          <span className="subtle">{saved?.settings && Object.keys(saved.settings).length ? "Configured" : "Needs setup"}</span>
          <Button size="sm" icon="settings" onClick={() => setEditing(true)}>Configure</Button>
        </div>
      )}
      {editing && (
        <form
          className="connector-settings"
          onSubmit={(e) => {
            e.preventDefault();
            write(true, draft);
          }}
        >
          {c.settings.map((s) => (
            <label key={s.key} className="setting">
              {s.label}
              <span className="input">
                <input
                  type={s.kind === "secret" ? "password" : s.kind === "number" ? "number" : "text"}
                  value={String(draft[s.key] ?? "")}
                  required={s.required}
                  autoComplete="off"
                  onChange={(e) => setDraft({ ...draft, [s.key]: s.kind === "number" ? (e.target.value ? Number(e.target.value) : "") : e.target.value })}
                />
              </span>
              {s.help && <small>{s.help}</small>}
            </label>
          ))}
          <div className="toolbar">
            <Button type="submit" variant="primary" disabled={busy || missingRequired}>{busy ? "Connecting…" : "Save and connect"}</Button>
            <Button type="button" variant="ghost" onClick={() => setEditing(false)}>Cancel</Button>
          </div>
        </form>
      )}
    </Card>
  );
}
