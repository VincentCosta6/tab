import { Badge, Button, Card, FormRow, Meter, Section } from "../../../design/components";
import { Markdown } from "../../../design/markdown";
import { timeAgo } from "../../../lib/format";
import { percent, useAppUpdate, useInstall } from "../../updates/appUpdate";

/** tab's own version, the hourly check, and installing a newer release. */
export function UpdatesPage() {
  const { version, check } = useAppUpdate();
  const { state, install } = useInstall();
  const v = version.data;
  const update = check.data;
  const busy = state.phase === "downloading" || state.phase === "restarting";
  const pct = percent(state);
  const checked = check.dataUpdatedAt ? new Date(check.dataUpdatedAt).toISOString() : null;
  const ago = checked ? timeAgo(checked) : null;

  return (
    <Section title="Updates" description="tab checks for a new release every hour while it's open. Updates are signed, and tab only installs one it can verify. Installing restarts the app; chats that were running pick up where they left off.">
      <Card>
        <FormRow label="This version">
          <span className="mono">{v ? v.version : "…"}</span>
        </FormRow>
        <FormRow label="Status" help={v?.updates && ago ? `Last checked ${/^\d/.test(ago) ? `${ago} ago` : ago}` : undefined}>
          <div className="settings-version">
            <div className="settings-version-line">
              {!v ? <span className="subtle">…</span>
                : !v.updates ? <span className="subtle">{v.reason}</span>
                : check.isFetching && !busy ? <span className="subtle">Checking…</span>
                : check.error ? <Badge tone="danger">Couldn't check</Badge>
                : update ? <Badge tone="info">{update.version} available</Badge>
                : check.isSuccess ? <Badge tone="success">Up to date</Badge>
                : null}
              {v?.updates && !update && (
                <Button size="sm" icon="refresh" disabled={check.isFetching || busy} onClick={() => void check.refetch()}>Check for updates</Button>
              )}
              {update && (
                <Button size="sm" variant="primary" disabled={busy} onClick={() => void install()}>
                  {state.phase === "downloading" ? (pct != null ? `Downloading ${pct}%` : "Downloading…") : state.phase === "restarting" ? "Restarting…" : `Install ${update.version} and restart`}
                </Button>
              )}
            </div>
            {state.phase === "downloading" && pct != null && <Meter value={pct} max={100} />}
            {check.error && <p className="settings-error">{String((check.error as { message?: string })?.message ?? check.error)}</p>}
            {state.phase === "failed" && <p className="settings-error">Couldn't install: {state.error}</p>}
          </div>
        </FormRow>
        {update?.notes && (
          <FormRow label={`What's new in ${update.version}`}>
            <div className="settings-notes"><Markdown text={blocks(update.notes)} /></div>
          </FormRow>
        )}
      </Card>
    </Section>
  );
}

/** Release notes put headings right above their lists; give each its own block. */
const blocks = (notes: string) => notes.replace(/\r\n/g, "\n").replace(/^(#{1,4} .*)$/gm, "\n$1\n").trim();
