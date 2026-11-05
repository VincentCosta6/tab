import { useEffect, useState } from "react";
import { Badge, Button, Card, PageHeader, Section, StatusDot, Tabs, Toggle, cx } from "../../design/components";
import { Brand, hasBrand } from "../../design/brands";
import { Icon } from "../../design/icons";
import { useApp } from "../../lib/app-context";
import { timeAgo, timeUntil } from "../../lib/format";
import { useConfig, useSaveConfig, useSearchIndex } from "../../lib/hooks";
import type { ConnectorInfo, ConnectorStatus, Tone } from "../../lib/types";
import { startSetupChat } from "../work/Chat";
import { ApiUsage } from "./ApiUsage";
import { GhConnect } from "./GhConnect";

type View = "installed" | "marketplace" | "usage";

export function statusView(s: ConnectorStatus, available: boolean): { tone: Tone; text: string } {
  if (!available) return { tone: "neutral", text: "Coming soon" };
  switch (s.state) {
    case "disabled": return { tone: "neutral", text: "Not installed" };
    case "starting": return { tone: "info", text: "Starting…" };
    case "running": return { tone: "success", text: s.last_sync ? `Synced ${timeAgo(s.last_sync)}` : "Connected" };
    case "rate_limited": return { tone: "warning", text: `Rate limited, resumes in ${timeUntil(s.until)}` };
    case "error": return { tone: "danger", text: s.message };
    case "stopped": return { tone: "neutral", text: "Stopped" };
  }
}

export function ConnectorMark({ c, size = 34 }: { c: ConnectorInfo; size?: number }) {
  return (
    <span className="source-glyph branded" style={{ width: size, height: size }}>
      {hasBrand(c.icon) ? <Brand name={c.icon} size={Math.round(size * 0.58)} /> : <Icon name={c.icon} size={Math.round(size * 0.55)} />}
    </span>
  );
}

const shelves: { id: string; label: string }[] = [
  { id: "code", label: "Code" },
  { id: "communication", label: "Communication" },
  { id: "work", label: "Planning" },
  { id: "built_in", label: "Built in" },
];

export function ConnectionsTab() {
  const { connectors } = useApp();
  const [view, setView] = useState<View>("installed");
  const [open, setOpen] = useState<string | null>(null);
  const installed = connectors.filter((c) => c.enabled);
  const current = connectors.find((c) => c.id === open);

  if (current) return <ConnectorDetail c={current} onBack={() => setOpen(null)} />;

  return (
    <div className="page">
      <PageHeader
        title="Connections"
        subtitle="Everything tab knows comes in through a connection. Each one brings its own kinds of work, settings and rate budgets."
        actions={<AssistButton />}
      />
      <div className="page-tabs">
        <Tabs<View>
          value={view}
          onChange={setView}
          items={[
            { value: "installed", label: "Installed", count: installed.length },
            { value: "marketplace", label: "Marketplace", count: connectors.filter((c) => !c.enabled).length },
            { value: "usage", label: "API usage" },
          ]}
        />
      </div>
      {view === "installed" && (
        <div className="connector-list">
          {installed.map((c) => (
            <InstalledRow key={c.id} c={c} onOpen={() => setOpen(c.id)} />
          ))}
          <button className="connector-add" onClick={() => setView("marketplace")}>
            <Icon name="store" size={16} /> Browse the marketplace
          </button>
        </div>
      )}
      {view === "marketplace" &&
        shelves.map((shelf) => {
          const items = connectors.filter((c) => (c.category || "built_in") === shelf.id && !c.enabled);
          if (!items.length) return null;
          return (
            <Section key={shelf.id} title={shelf.label}>
              <div className="connector-grid">
                {items.map((c) => (
                  <MarketCard key={c.id} c={c} onOpen={() => setOpen(c.id)} />
                ))}
              </div>
            </Section>
          );
        })}
      {view === "usage" && <ApiUsage />}
    </div>
  );
}

/** Hands setup to the agent in a new chat. */
function AssistButton({ name, primary }: { name?: string; primary?: boolean }) {
  const { data: config } = useConfig();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (config && !config.agent.kind) return null;
  return (
    <span className="assist">
      <Button
        size="sm"
        variant={primary ? "primary" : "secondary"}
        icon="bot"
        disabled={busy}
        title="An agent works out the settings with you in a chat. Passwords go into a card, never the chat."
        onClick={async () => {
          setBusy(true);
          setError(null);
          try {
            await startSetupChat(name);
          } catch (e) {
            setError(String(e));
            setBusy(false);
          }
        }}
      >
        {busy ? "Starting…" : name ? "Set up with assistant" : "Connect with assistant"}
      </Button>
      {error && <small className="text-danger">{error}</small>}
    </span>
  );
}

function InstalledRow({ c, onOpen }: { c: ConnectorInfo; onOpen: () => void }) {
  const s = statusView(c.status, c.available);
  return (
    <button className="connector-row" onClick={onOpen}>
      <ConnectorMark c={c} />
      <div className="connector-row-main">
        <div className="connector-name">
          {c.name} {c.builtin && <Badge>Built in</Badge>}
        </div>
        <div className="connector-status"><StatusDot tone={s.tone} /> <span>{s.text}</span></div>
      </div>
      <div className="connector-row-kinds">
        {c.kinds.slice(0, 3).map((k) => <Badge key={k.kind} tone={k.tone}>{k.label}</Badge>)}
      </div>
      <Icon name="chevron-right" size={16} />
    </button>
  );
}

function MarketCard({ c, onOpen }: { c: ConnectorInfo; onOpen: () => void }) {
  return (
    <Card className={cx("connector", !c.available && "unavailable")}>
      <div className="connector-head">
        <ConnectorMark c={c} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <div className="connector-name">{c.name}</div>
          <div className="subtle">{c.available ? (c.id === "github" ? "Sign in with GitHub" : c.id === "email" ? "Sign in with IMAP" : c.settings.some((s) => s.required) ? "Needs a token" : "One click") : "Coming soon"}</div>
        </div>
        {c.available && <Button size="sm" variant="primary" onClick={onOpen}>Install</Button>}
      </div>
      <p className="connector-desc">{c.description}</p>
      {c.features.length > 0 && (
        <ul className="feature-list">
          {c.features.slice(0, 2).map((f) => (
            <li key={f}><Icon name="check" size={12} /> {f}</li>
          ))}
        </ul>
      )}
    </Card>
  );
}

/** One connection: what it brings, its settings, its rate budget, install / remove. */
function ConnectorDetail({ c, onBack }: { c: ConnectorInfo; onBack: () => void }) {
  const { data: config } = useConfig();
  const save = useSaveConfig();
  const saved = config?.connectors[c.id];
  const [draft, setDraft] = useState<Record<string, unknown>>(saved?.settings ?? {});
  const [error, setError] = useState<string | null>(null);
  const s = statusView(c.status, c.available);

  useEffect(() => setDraft(saved?.settings ?? {}), [saved]);

  // GitHub's token lives in the sign-in card, next to the GitHub CLI login.
  const settings = c.id === "github" ? c.settings.filter((f) => f.key !== "token") : c.settings;
  const missingRequired = c.settings.some((f) => f.required && !String(draft[f.key] ?? "").trim());
  const write = async (enabled: boolean) => {
    if (!config) return;
    setError(null);
    try {
      await save.mutateAsync({ ...config, connectors: { ...config.connectors, [c.id]: { enabled, settings: draft } } });
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="page page-narrow">
      <Button size="sm" variant="ghost" icon="arrow-left" onClick={onBack}>Connections</Button>
      <header className="connector-detail-head">
        <ConnectorMark c={c} size={48} />
        <div style={{ flex: 1 }}>
          <h1 className="page-title">{c.name}</h1>
          <div className="connector-status"><StatusDot tone={s.tone} /> <span>{s.text}</span></div>
        </div>
        {c.available && !c.builtin && (c.enabled ? (
          <Button onClick={() => write(false)} disabled={save.isPending}>Uninstall</Button>
        ) : (
          <Button variant="primary" onClick={() => write(true)} disabled={save.isPending || missingRequired}>
            {save.isPending ? "Installing…" : "Install"}
          </Button>
        ))}
      </header>
      <p className="connector-desc large">{c.description}</p>
      {c.available && !c.builtin && (
        <div className="assist-row">
          <AssistButton name={c.name} primary={!c.enabled && c.id === "email"} />
          <span className="subtle">{c.id === "email" ? "Give it your address and it finds your mail server, signs in and checks your folders." : "An agent fills in the settings with you in a chat."}</span>
        </div>
      )}
      {error && <p className="chat-error">{error}</p>}

      {c.features.length > 0 && (
        <Section title="What it adds">
          <ul className="feature-list">
            {c.features.map((f) => <li key={f}><Icon name="check" size={13} /> {f}</li>)}
          </ul>
        </Section>
      )}

      {c.id === "github" && (
        <Section title="Sign in">
          <Card>
            <GhConnect
              token={String(draft.token ?? "")}
              onToken={(t) => setDraft({ ...draft, token: t })}
              onSignedIn={(_, fresh) => { if (fresh && !c.enabled) void write(true); }}
            />
          </Card>
        </Section>
      )}

      {c.id === "tab" && <SearchIndexCard />}

      {settings.length > 0 && (
        <Section title="Settings">
          <Card>
            <form className="connector-settings" onSubmit={(e) => (e.preventDefault(), write(true))}>
              {settings.map((f) => (
                <label key={f.key} className="setting">
                  {f.label}
                  {f.kind === "toggle" ? (
                    <Toggle label={f.label} checked={!!draft[f.key]} onChange={(v) => setDraft({ ...draft, [f.key]: v })} />
                  ) : (
                    <span className="input">
                      <input
                        type={f.kind === "secret" ? "password" : f.kind === "number" ? "number" : "text"}
                        value={String(draft[f.key] ?? "")}
                        required={f.required}
                        autoComplete="off"
                        onChange={(e) => setDraft({ ...draft, [f.key]: f.kind === "number" ? (e.target.value ? Number(e.target.value) : "") : e.target.value })}
                      />
                    </span>
                  )}
                  {f.help && <small>{f.help}</small>}
                </label>
              ))}
              <div className="toolbar">
                <Button type="submit" variant="primary" disabled={save.isPending || missingRequired}>
                  {c.enabled ? "Save" : "Save and install"}
                </Button>
              </div>
            </form>
          </Card>
        </Section>
      )}

      {c.kinds.length > 0 && (
        <Section title="Kinds of work" description="What lands in your queue from this connection. Scores are set in Settings → Ranking.">
          <div className="block-badges">
            {c.kinds.map((k) => <Badge key={k.kind} tone={k.tone}>{k.label}</Badge>)}
          </div>
        </Section>
      )}

      {c.rate_limits.length > 0 && (
        <Section title="Rate budget" description="Declared limits. tab budgets against these, backs off background refreshes when headroom is low or after a burst, and shows live usage under API usage.">
          <div className="limits">
            {c.rate_limits.map((l) => (
              <Card key={`${l.provider}:${l.resource}`}>
                <div className="limit">
                  <div className="limit-head"><strong>{l.resource}</strong><span className="limit-value">{l.requests.toLocaleString()}</span></div>
                  <div className="subtle">per {l.per_seconds >= 3600 ? `${l.per_seconds / 3600} hour` : l.per_seconds >= 60 ? `${l.per_seconds / 60} min` : `${l.per_seconds}s`} · burst over {l.burst} in 10s cools down</div>
                  {l.note && <div className="subtle">{l.note}</div>}
                </div>
              </Card>
            ))}
          </div>
        </Section>
      )}
    </div>
  );
}

const mb = (n: number) => `${(n / 1e6).toFixed(n < 1e7 ? 1 : 0)} MB`;
const modelLabel = (m: string) => (m.endsWith("32M") ? "Larger model" : "Small model") + ` · ${m.split("/")[1]}`;

/** Search by meaning: download, indexing progress, and when it last caught up. */
function SearchIndexCard() {
  const { data: s } = useSearchIndex();
  if (!s) return null;
  let tone: Tone = "neutral";
  let title = "Off";
  let detail = "Turn on Search by meaning below to find things by what they're about.";
  let progress: number | null = null;
  switch (s.state) {
    case "downloading":
      tone = "info";
      title = "Downloading model";
      detail = s.total ? `${mb(s.received)} of ${mb(s.total)}` : mb(s.received);
      progress = s.total ? s.received / s.total : null;
      break;
    case "loading":
      tone = "info";
      title = "Loading model";
      detail = modelLabel(s.model);
      break;
    case "indexing":
      tone = "info";
      title = "Indexing";
      detail = `${s.done.toLocaleString()} of ${s.total.toLocaleString()} items and notes`;
      progress = s.total ? s.done / s.total : null;
      break;
    case "ready":
      tone = "success";
      title = "Ready";
      detail = `${s.docs.toLocaleString()} items and notes indexed${s.indexed_at ? ` · last ${timeAgo(s.indexed_at)}` : ""} · ${modelLabel(s.model)}`;
      break;
    case "error":
      tone = "danger";
      title = "Couldn't get the model";
      detail = `${s.message} · retrying in ${timeUntil(s.retry_at)}`;
      break;
  }
  return (
    <Section title="Search index">
      <Card>
        <div className="index-status">
          <div className="index-status-line">
            <StatusDot tone={tone} />
            <strong>{title}</strong>
            <span className="subtle">{detail}</span>
          </div>
          {progress !== null && (
            <div className="index-progress" role="progressbar" aria-valuenow={Math.round(progress * 100)} aria-valuemin={0} aria-valuemax={100}>
              <div style={{ width: `${Math.min(100, progress * 100)}%` }} />
            </div>
          )}
        </div>
      </Card>
    </Section>
  );
}
