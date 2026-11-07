import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, FormRow } from "../../../design/components";
import { api, onAgentUpgrade } from "../../../lib/api";
import { isOlder } from "../../updates/Updater";

const version = (v: string | null | undefined) => v?.match(/\d+(?:\.\d+)+/)?.[0] ?? v ?? null;

/** The agent CLI's version, the newest release, and updating it the way it was installed. */
export function AgentVersionRow() {
  const qc = useQueryClient();
  const health = useQuery({ queryKey: ["agent-health"], queryFn: api.agentHealth, staleTime: 30 * 60_000, retry: false });
  const [running, setRunning] = useState(false);
  const [lines, setLines] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const h = health.data;

  const update = async () => {
    setRunning(true); setLines([]); setError(null);
    const off = await onAgentUpgrade((l) => setLines((p) => [...p.slice(-199), l]));
    try {
      await api.agentUpgrade();
      await qc.refetchQueries({ queryKey: ["agent-health"] });
    } catch (e) {
      setError(String((e as { message?: string })?.message ?? e));
    } finally {
      off();
      setRunning(false);
    }
  };

  const check = () => void qc.refetchQueries({ queryKey: ["agent-health"] });
  const behind = !!h?.found && !!h.latest && isOlder(h.version, h.latest);
  const help = !h ? undefined : !h.found ? (h.error ?? "Not found.") : h.upgrade ? `Updates with ${h.upgrade}` : h.path ?? undefined;

  return (
    <FormRow label="Version" help={help}>
      <div className="settings-version">
        <div className="settings-version-line">
          <span className="mono">{health.isLoading ? "Checking…" : h?.found ? version(h.version) ?? "unknown" : "Not installed"}</span>
          {h?.found && !running && (behind ? <Badge tone="info">{version(h.latest)} available</Badge> : h.latest ? <Badge tone="success">Up to date</Badge> : null)}
          {behind && <Button size="sm" variant="primary" disabled={running} onClick={() => void update()}>{running ? "Updating…" : `Update to ${version(h?.latest)}`}</Button>}
          {!behind && !running && <Button size="sm" variant="ghost" icon="refresh" disabled={health.isFetching} onClick={check}>{health.isFetching ? "Checking…" : "Check again"}</Button>}
        </div>
        {error && <p className="settings-error">{error}</p>}
        {(running || error) && lines.length > 0 && <pre className="updater-log">{lines.join("\n")}</pre>}
      </div>
    </FormRow>
  );
}
