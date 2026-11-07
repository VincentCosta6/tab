import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, Card, FormRow, Section, TextInput } from "../../../design/components";
import { api } from "../../../lib/api";
import type { TabConfig } from "../../../lib/types";
import { bytes, timeAgo } from "../../../lib/format";
import { useDraft } from "../draft";

export function StoragePage() {
  const { draft, set } = useDraft();
  return <StorageSection config={draft.storage} onChange={(v) => set((c) => void (c.storage = v))} />;
}

/** What tab keeps on disk, and clearing it out. */
function StorageSection({ config, onChange }: { config: TabConfig["storage"]; onChange: (v: TabConfig["storage"]) => void }) {
  const qc = useQueryClient();
  const usage = useQuery({ queryKey: ["storage"], queryFn: api.storageUsage });
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const run = async (key: string, fn: () => Promise<{ removed: number; bytes: number }>) => {
    setBusy(key);
    setNote(null);
    try {
      const s = await fn();
      setNote(s.removed ? `Freed ${bytes(s.bytes)}.` : "Nothing to clear.");
    } catch (e) {
      setNote(`Couldn't clear: ${e}`);
    } finally {
      setBusy(null);
      qc.invalidateQueries({ queryKey: ["storage"] });
    }
  };
  const u = usage.data;
  const size = (s?: { bytes: number; files: number }) => (s ? `${bytes(s.bytes)} · ${s.files.toLocaleString()} files` : "…");

  return (
    <Section
      title="Storage"
      description="Cleared automatically every few hours. Review checkouts also go as soon as their PR closes or merges."
      actions={<Button size="sm" disabled={busy !== null} onClick={() => run("clean", api.storageClean)}>{busy === "clean" ? "Cleaning…" : "Clean up now"}</Button>}
    >
      <Card>
        <FormRow label="Keep for" help="Attachments of chats idle this long, and review checkouts not opened this long, are removed.">
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <TextInput type="number" min={1} step={1} value={config.keep_days} style={{ maxWidth: 100 }}
              onChange={(e) => onChange({ ...config, keep_days: Math.max(1, Math.round(Number(e.target.value) || 1)) })} />
            <span>days</span>
          </div>
        </FormRow>
        <FormRow label="Size limit" help="Past this, the least recently used attachments and checkouts are removed until it fits. Anything used in the last hour stays.">
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <TextInput type="number" min={0} step={0.5} placeholder="No limit" value={config.max_gb ?? ""} style={{ maxWidth: 100 }}
              onChange={(e) => onChange({ ...config, max_gb: e.target.value === "" || Number(e.target.value) <= 0 ? null : Number(e.target.value) })} />
            <span>GB</span>
          </div>
        </FormRow>
        <FormRow label="Chat attachments" help="Files you attach to chats.">
          <span className="mono">{size(u?.attachments)}</span>
        </FormRow>
        <FormRow label="Chat folders" help="Each chat's prompt and settings for its agent.">
          <span className="mono">{size(u?.sessions)}</span>
        </FormRow>
        <FormRow label="Review checkouts" help="Local copies of repositories for PR reviews. Opening a PR again brings its copy back.">
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <span className="mono">{u ? bytes(u.workspaces.bytes) : "…"}</span>
            {!!u?.checkouts.length && (
              <Button size="sm" disabled={busy !== null} onClick={() => run("all", () => api.storageRemoveCheckouts())}>
                {busy === "all" ? "Removing…" : "Remove all"}
              </Button>
            )}
          </div>
        </FormRow>
        {u?.checkouts.map((c) => (
          <FormRow key={`${c.repo}#${c.number}`} label={`${c.repo} #${c.number}`} help={opened(c.last_used)}>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span className="mono">{bytes(c.bytes)}</span>
              <Button size="sm" variant="ghost" icon="trash" aria-label={`Remove ${c.repo} #${c.number}`} disabled={busy !== null}
                onClick={() => run(`${c.repo}#${c.number}`, () => api.storageRemoveCheckouts(c.repo, c.number))} />
            </div>
          </FormRow>
        ))}
        {note && <FormRow label=""><span>{note}</span></FormRow>}
      </Card>
    </Section>
  );
}

/** "Opened 3h ago", "Opened just now", or "Opened 10/28/2026" once it's old. */
function opened(iso: string) {
  const ago = timeAgo(iso);
  return /^\d+[mhd]$/.test(ago) ? `Opened ${ago} ago` : `Opened ${ago}`;
}
