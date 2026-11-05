import { useEffect, useState } from "react";
import { Badge, Card, Meter, Section, cx } from "../../design/components";
import { api } from "../../lib/api";
import { usePressure } from "../../lib/hooks";
import { timeAgo, timeUntil } from "../../lib/format";
import type { AuditEntry, Outcome, RateLimitState, Tone, UsageRow } from "../../lib/types";

const outcomeTone: Record<Outcome, Tone> = {
  network: "accent",
  cache_hit: "success",
  revalidated: "info",
  stale: "warning",
  rate_limited: "danger",
  deferred: "neutral",
  error: "danger",
};
const outcomeLabel: Record<Outcome, string> = {
  network: "Network",
  cache_hit: "Cache hit",
  revalidated: "Revalidated (304)",
  stale: "Served stale",
  rate_limited: "Rate limited",
  deferred: "Deferred (background)",
  error: "Error",
};

/** Rate limits, who is spending them, and every call, from the gateway's audit log. */
export function ApiUsage() {
  const [limits, setLimits] = useState<RateLimitState[]>([]);
  const [usage, setUsage] = useState<UsageRow[]>([]);
  const [audit, setAudit] = useState<AuditEntry[]>([]);
  const [hours, setHours] = useState(24);
  const [provider, setProvider] = useState("");
  const [outcome, setOutcome] = useState("");

  useEffect(() => {
    let live = true;
    const load = async () => {
      const [l, u, a] = await Promise.all([
        api.rateLimits(),
        api.apiUsage(hours),
        api.auditLog({ provider: provider || undefined, outcome: outcome || undefined, limit: 200 }),
      ]);
      if (live) (setLimits(l), setUsage(u), setAudit(a));
    };
    load();
    const t = setInterval(load, 5000);
    return () => {
      live = false;
      clearInterval(t);
    };
  }, [hours, provider, outcome]);

  const providers = [...new Set(usage.map((u) => u.provider))];
  const totals = usage.reduce((t, u) => ({ total: t.total + u.total, network: t.network + u.network }), { total: 0, network: 0 });
  const saved = totals.total ? Math.round(((totals.total - totals.network) / totals.total) * 100) : 0;

  return (
    <>
      <Budgets />
      <Section title="Rate limits" description="What each provider last reported. Calls are refused locally once a limit is spent, and cached data is served instead.">
        {limits.length === 0 ? (
          <p className="subtle">No provider has reported limits yet.</p>
        ) : (
          <div className="limits">
            {limits.map((l) => {
              const blocked = l.blocked_until && new Date(l.blocked_until) > new Date();
              const pct = l.limit && l.remaining !== null ? l.remaining / l.limit : 1;
              const tone: Tone = blocked ? "danger" : pct < 0.1 ? "danger" : pct < 0.3 ? "warning" : "success";
              return (
                <Card key={`${l.provider}:${l.resource}`} className="limit">
                  <div className="limit-head">
                    <strong>{l.provider} <span className="subtle">· {l.resource}</span></strong>
                    {blocked && <Badge tone="danger">Blocked {timeUntil(l.blocked_until!)}</Badge>}
                  </div>
                  <div>
                    <span className="limit-value">{l.remaining ?? "?"}</span>
                    <span className="subtle"> / {l.limit ?? "?"} left</span>
                  </div>
                  <Meter value={l.remaining ?? 0} max={l.limit ?? 1} tone={tone} />
                  <span className="subtle">{l.reset_at ? `Resets in ${timeUntil(l.reset_at)}` : "Reset unknown"} · updated {timeAgo(l.updated_at)}</span>
                </Card>
              );
            })}
          </div>
        )}
      </Section>

      <Section
        title="Usage by caller"
        description={`${totals.total} calls, ${totals.network} hit the network. ${saved}% answered by cache or a free revalidation.`}
        actions={
          <select className="select" value={hours} onChange={(e) => setHours(Number(e.target.value))}>
            <option value={1}>Last hour</option>
            <option value={24}>Last 24 hours</option>
            <option value={168}>Last 7 days</option>
          </select>
        }
      >
        <Card padded={false}>
          <table className="table">
            <thead>
              <tr>
                <th>Provider</th>
                <th>Caller</th>
                <th>Purpose</th>
                <th className="num">Calls</th>
                <th className="num">Network</th>
                <th>Outcomes</th>
                <th className="num">Avg</th>
              </tr>
            </thead>
            <tbody>
              {usage.map((u) => (
                <tr key={`${u.provider}:${u.connector}:${u.purpose}`}>
                  <td>{u.provider}</td>
                  <td>{u.connector}</td>
                  <td>{u.purpose}</td>
                  <td className="num">{u.total}</td>
                  <td className="num">{u.network}</td>
                  <td>
                    <OutcomeBar row={u} />
                  </td>
                  <td className="num">{Math.round(u.avg_ms)}ms</td>
                </tr>
              ))}
              {!usage.length && (
                <tr>
                  <td colSpan={7} className="subtle">No calls in this window.</td>
                </tr>
              )}
            </tbody>
          </table>
        </Card>
        <div className="legend" style={{ marginTop: 8 }}>
          {(Object.keys(outcomeLabel) as Outcome[]).map((o) => (
            <span key={o}>
              <span className={cx("status-dot", `tone-${outcomeTone[o]}`)} /> {outcomeLabel[o]}
            </span>
          ))}
        </div>
      </Section>

      <Section
        title="Audit log"
        description="Every outbound call attempt, including cache hits and calls refused for rate limits. Kept for 30 days."
        actions={
          <div className="toolbar">
            <select className="select" value={provider} onChange={(e) => setProvider(e.target.value)}>
              <option value="">All providers</option>
              {providers.map((p) => (
                <option key={p}>{p}</option>
              ))}
            </select>
            <select className="select" value={outcome} onChange={(e) => setOutcome(e.target.value)}>
              <option value="">All outcomes</option>
              {(Object.keys(outcomeLabel) as Outcome[]).map((o) => (
                <option key={o} value={o}>{outcomeLabel[o]}</option>
              ))}
            </select>
          </div>
        }
      >
        <Card padded={false}>
          <table className="table">
            <thead>
              <tr>
                <th>When</th>
                <th>Caller</th>
                <th>Request</th>
                <th>Outcome</th>
                <th className="num">Time</th>
                <th className="num">Limit left</th>
              </tr>
            </thead>
            <tbody>
              {audit.map((a) => (
                <tr key={a.id}>
                  <td className="subtle" title={a.ts}>{timeAgo(a.ts)}</td>
                  <td>
                    {a.connector}
                    <div className="subtle">{a.purpose}</div>
                  </td>
                  <td className="mono">
                    {a.method} {a.url}
                    {a.error && <div className="text-danger">{a.error}</div>}
                  </td>
                  <td>
                    <Badge tone={outcomeTone[a.outcome]}>{outcomeLabel[a.outcome]}</Badge>
                  </td>
                  <td className="num">{a.duration_ms}ms</td>
                  <td className="num">{a.rl_remaining !== null ? `${a.rl_remaining}/${a.rl_limit ?? "?"}` : "–"}</td>
                </tr>
              ))}
              {!audit.length && (
                <tr>
                  <td colSpan={6} className="subtle">Nothing logged yet.</td>
                </tr>
              )}
            </tbody>
          </table>
        </Card>
      </Section>
    </>
  );
}

function OutcomeBar({ row }: { row: UsageRow }) {
  const parts: [Outcome, number][] = [
    ["network", row.network],
    ["cache_hit", row.cache_hits],
    ["revalidated", row.revalidated],
    ["stale", row.stale],
    ["rate_limited", row.rate_limited],
    ["error", row.errors],
  ];
  return (
    <div className="outcome-bar" title={parts.filter(([, n]) => n).map(([o, n]) => `${outcomeLabel[o]}: ${n}`).join("\n")}>
      {parts.map(([o, n]) => n > 0 && <span key={o} className={`tone-${outcomeTone[o]}`} style={{ flex: n }} />)}
    </div>
  );
}

/**
 * Client-side budgets from each connector's declared limits: how much of the
 * window we've used, recent bursts, and whether background work (preview
 * refreshes, polls) is cooling down. Interactive calls always go first.
 */
function Budgets() {
  const { data: pressure = [] } = usePressure();
  return (
    <Section title="Budgets" description="Every connector declares its provider's limits. tab tracks its own usage against them, backs off background refreshes when headroom drops below 30% or after a burst, and keeps interactive calls flowing.">
      {pressure.length === 0 ? (
        <p className="subtle">No calls yet.</p>
      ) : (
        <div className="limits">
          {pressure.map((p) => {
            const pct = Math.round(p.headroom * 100);
            const tone: Tone = pct < 10 ? "danger" : pct < 30 ? "warning" : "success";
            return (
              <Card key={`${p.provider}:${p.resource}`}>
                <div className="limit">
                  <div className="limit-head">
                    <strong>{p.provider} · {p.resource}</strong>
                    <span className="limit-value">{pct}%</span>
                  </div>
                  <Meter value={p.headroom} max={1} tone={tone} />
                  <div className="subtle">
                    {p.budget ? `${p.used} of ${p.budget} per ${p.window_seconds! >= 3600 ? `${p.window_seconds! / 3600}h` : `${p.window_seconds}s`}` : `${p.used} sent`}
                    {p.remote_limit != null && ` · provider says ${p.remote_remaining}/${p.remote_limit}`}
                  </div>
                  <div className="toolbar">
                    <Badge tone={p.recent > p.burst_threshold ? "warning" : "neutral"}>{p.recent} in 10s (burst {p.burst_threshold})</Badge>
                    {p.cooling_until && <Badge tone="warning" icon="clock">cooling {timeUntil(p.cooling_until)}</Badge>}
                    {p.deferred > 0 && <Badge>{p.deferred} deferred</Badge>}
                  </div>
                </div>
              </Card>
            );
          })}
        </div>
      )}
    </Section>
  );
}
