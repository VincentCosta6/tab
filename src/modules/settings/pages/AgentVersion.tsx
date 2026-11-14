import { useQueryClient } from "@tanstack/react-query";
import { Badge, Button, FormRow } from "../../../design/components";
import { useAgentUpdate } from "../../updates/agentUpdate";

/** The agent CLI's version, the newest release, and updating it the way it was installed. */
export function AgentVersionRow({ label = "Version" }: { label?: string }) {
  const qc = useQueryClient();
  const { health, behind, have, latest, upgrade, update } = useAgentUpdate();
  const h = health.data;
  const running = upgrade.phase === "running";
  const lines = upgrade.phase === "running" || upgrade.phase === "failed" ? upgrade.lines : [];

  const check = () => void qc.refetchQueries({ queryKey: ["agent-health"] });
  const help = !h ? undefined : !h.found ? (h.error ?? "Not found.") : h.upgrade ? `Updates with ${h.upgrade}` : h.path ?? undefined;

  return (
    <FormRow label={label} help={help}>
      <div className="settings-version">
        <div className="settings-version-line">
          <span className="mono">{health.isLoading ? "Checking…" : h?.found ? have ?? "unknown" : "Not installed"}</span>
          {h?.found && !running && (behind ? <Badge tone="info">{latest} available</Badge> : h.latest ? <Badge tone="success">Up to date</Badge> : null)}
          {behind && <Button size="sm" variant="primary" disabled={running} onClick={() => void update()}>{running ? "Updating…" : `Update to ${latest}`}</Button>}
          {!behind && !running && <Button size="sm" variant="ghost" icon="refresh" disabled={health.isFetching} onClick={check}>{health.isFetching ? "Checking…" : "Check again"}</Button>}
        </div>
        {upgrade.phase === "failed" && <p className="settings-error">{upgrade.error}</p>}
        {lines.length > 0 && <pre className="updater-log">{lines.join("\n")}</pre>}
      </div>
    </FormRow>
  );
}
