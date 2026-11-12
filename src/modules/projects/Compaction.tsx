// The coordinator's long-running chat: how full its context is, where it was
// compacted (a divider and the summary it carried forward, which the person
// can edit), the History of compacted stretches, and its settings. The
// backend side is crates/tab-core/src/agents/coordinator.rs.

import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, TextArea, Toggle, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { keys } from "../../lib/query";
import type { Chat, Project, Segment, SessionMessage } from "../../lib/types";
import { Markdown } from "../../design/markdown";
import { k } from "../work/ContextMeter";

const when = (iso: string) => new Date(iso).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
const day = (iso: string) => new Date(iso).toLocaleDateString(undefined, { month: "short", day: "numeric" });
const span = (from: string, to: string) => (day(from) === day(to) ? day(to) : `${day(from)} – ${day(to)}`);
const days = (from: string, to: string) => {
  const n = Math.round((new Date(to).getTime() - new Date(from).getTime()) / 86_400_000);
  return n >= 1 ? `${n} day${n === 1 ? "" : "s"}` : null;
};

/** Context used at the end of the latest turn since the last compaction. */
function usedSince(messages: SessionMessage[]): number | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.kind === "compacted") return null;
    if (m.kind === "result" && m.meta?.context?.used) return Number(m.meta.context.used);
  }
  return null;
}

/** "31k / 200k" and a bar toward where the chat compacts. */
export function CompactMeter({ chat, project }: { chat: Chat | undefined; project: Project }) {
  const used = usedSince(chat?.messages ?? []);
  if (used == null) return null;
  const at = project.compact_at;
  const pct = at > 0 ? Math.min(100, (used / at) * 100) : 0;
  return (
    <span className="pjc-meter" title={at > 0 ? `Compacts automatically at ${k(at)} tokens` : "Auto-compact is off"}>
      <span>{k(used)}{at > 0 && ` / ${k(at)}`}</span>
      {at > 0 && <span className="pjc-bar"><span className={cx(pct >= 90 && "hot")} style={{ width: `${pct}%` }} /></span>}
    </span>
  );
}

/** Where the chat was compacted: a divider and what it carried forward. */
export function CompactedDivider({ m }: { m: SessionMessage }) {
  const id: string | undefined = m.meta?.segment_id;
  const { data: segment } = useQuery({ queryKey: keys.segment(id ?? ""), queryFn: () => api.coordinatorSegment(id!), enabled: !!id });
  const tokens = Number(m.meta?.tokens) || 0;
  const how = m.meta?.reason === "manual" ? "Compacted" : `Compacted automatically${tokens ? ` at ${k(tokens)}` : ""}`;
  const length = m.meta?.started_at ? days(m.meta.started_at, m.created_at) : null;
  const count = Number(m.meta?.messages) || 0;
  return (
    <>
      <div className="pjc-divider"><hr /><span>{[how, when(m.created_at), length, count ? `${count.toLocaleString()} messages` : null].filter(Boolean).join(" · ")}</span><hr /></div>
      {segment && <SummaryCard segment={segment} />}
    </>
  );
}

/** What the coordinator carried forward; the person can correct it before the next turn reads it. */
function SummaryCard({ segment }: { segment: Segment }) {
  const qc = useQueryClient();
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => setDraft(null), [segment.summary]);
  const save = async () => {
    if (draft === null) return;
    try {
      const s = await api.coordinatorSummarySave(segment.id, draft);
      qc.setQueryData(keys.segment(segment.id), s);
      if (segment.project_id) qc.invalidateQueries({ queryKey: keys.coordinatorHistory(segment.project_id) });
      setDraft(null);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <div className="pjc-summary">
      <div className="pjc-summary-head">
        <Icon name="layers" size={13} />
        <strong>What the coordinator carried forward</strong>
        {draft === null && <Button size="sm" variant="ghost" onClick={() => setDraft(segment.summary)}>Edit</Button>}
      </div>
      {draft !== null ? (
        <div className="pjt-edit">
          <TextArea autoFocus rows={6} value={draft} onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => (e.key === "Enter" && (e.metaKey || e.ctrlKey) ? save() : e.key === "Escape" && setDraft(null))} />
          {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
          <span className="pjt-actions">
            <Button size="sm" variant="ghost" onClick={() => setDraft(null)}>Cancel</Button>
            <Button size="sm" variant="primary" onClick={save}>Save</Button>
          </span>
        </div>
      ) : (
        <>
          <Markdown text={segment.summary} />
          <span className="subtle pjc-summary-foot">The next turn starts from this. The full transcript is kept in History.</span>
        </>
      )}
    </div>
  );
}

/** The compacted stretches of the coordinator's chat, newest first, and "Compact now". */
export function HistoryDialog({ project, chat, onClose }: { project: Project; chat: Chat | undefined; onClose: () => void }) {
  const qc = useQueryClient();
  const { data: segments = [] } = useQuery({ queryKey: keys.coordinatorHistory(project.id), queryFn: () => api.coordinatorHistory(project.id) });
  const [open, setOpen] = useState<Segment | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const compact = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.coordinatorCompact(project.id);
      qc.invalidateQueries({ queryKey: keys.coordinatorHistory(project.id) });
      qc.invalidateQueries({ queryKey: keys.coordinator(project.id) });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="pj-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="pj-dialog pjk-dialog pjc-history" role="dialog" aria-label="Coordinator history" onMouseDown={(e) => e.stopPropagation()}>
        <header className="pjk-dialog-head">
          {open ? (
            <button className="pjh-back" onClick={() => setOpen(null)}><Icon name="arrow-left" size={13} /> {span(open.started_at, open.ended_at)}</button>
          ) : (
            <h3>History</h3>
          )}
          <button className="icon-btn" onClick={onClose} aria-label="Close"><Icon name="x" size={14} /></button>
        </header>
        {open ? (
          <SegmentTranscript segment={open} />
        ) : (
          <>
            {segments.length ? (
              <ul className="pjc-list">
                {segments.map((g) => (
                  <li key={g.id}>
                    <span className="pjc-glyph"><Icon name="layers" size={13} /></span>
                    <span className="pjc-row">
                      <strong>{span(g.started_at, g.ended_at)}</strong>
                      <span className="subtle">{[g.tokens ? k(g.tokens) : null, `${g.messages.toLocaleString()} messages`, g.reason === "manual" ? "compacted by you" : null].filter(Boolean).join(" · ")}</span>
                    </span>
                    <Button size="sm" variant="ghost" onClick={() => setOpen(g)}>Open</Button>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="subtle">Nothing compacted yet. When a turn ends with more than {project.compact_at > 0 ? k(project.compact_at) : "the threshold"} of context, the coordinator writes down what to carry forward and starts fresh.</p>
            )}
            <p className="subtle pjc-note">Old transcripts are indexed too, so the coordinator can still look things up after compacting. Only it can read them.</p>
            {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
            <span className="pjt-actions">
              <Button size="sm" icon="layers" disabled={busy || !chat?.session || chat.running} onClick={compact}>{busy ? "Compacting…" : "Compact now"}</Button>
            </span>
          </>
        )}
      </div>
    </div>
  );
}

/** A compacted stretch, read-only: what was said, without tool output. */
function SegmentTranscript({ segment }: { segment: Segment }) {
  const { data: messages } = useQuery({ queryKey: ["segment-messages", segment.id], queryFn: () => api.coordinatorSegmentMessages(segment.id) });
  const said = (messages ?? []).filter((m) => m.kind === "text" || m.kind === "wake");
  if (!messages) return <p className="subtle">Loading…</p>;
  return (
    <div className="pjc-transcript">
      {said.map((m) =>
        m.kind === "wake" ? (
          <div key={m.id} className="msg-note">{m.content} · {when(m.created_at)}</div>
        ) : (
          <div key={m.id} className={cx("msg", m.role === "user" ? "msg-user" : "msg-assistant")}>
            {m.role === "user" ? m.content : <Markdown text={m.content} />}
          </div>
        ),
      )}
      {!said.length && <p className="subtle">No messages in this stretch.</p>}
    </div>
  );
}

const thresholds = [100_000, 150_000, 200_000, 400_000, 800_000];
const briefTimes = ["07:00", "08:00", "09:00", "10:00"];

/** How the coordinator watches the project and when its chat compacts. */
export function CoordinatorSettings({ project }: { project: Project }) {
  const qc = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const save = async (p: { watch?: boolean; brief?: string; compactAt?: number }) => {
    try {
      await api.coordinatorSettingsSave(project.id, p);
      await qc.invalidateQueries({ queryKey: keys.projects });
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };
  const brief = project.coordinator_brief;
  return (
    <section className="pjt-section">
      <div className="pjt-section-head"><span className="label-sm">Coordinator</span></div>
      <div className="pjc-settings">
        <span className="subtle">Checks</span>
        <span className="pjc-setting">
          <Toggle checked={project.coordinator_watch} onChange={(v) => save({ watch: v })} label="Wake on queue changes" />
          {project.coordinator_watch ? "On every queue change, and hourly while something waits" : "Only when you ask"}
        </span>
        <span className="subtle">Brief</span>
        <span className="pjc-setting">
          <select className="select" value={brief} onChange={(e) => save({ brief: e.target.value })}>
            <option value="">Off</option>
            {[...new Set([...briefTimes, ...(brief ? [brief] : [])])].sort().map((t) => <option key={t} value={t}>Every weekday {t}</option>)}
          </select>
        </span>
        <span className="subtle">Auto-compact</span>
        <span className="pjc-setting">
          <select className="select" value={project.compact_at} onChange={(e) => save({ compactAt: Number(e.target.value) })}>
            <option value={0}>Off</option>
            {[...new Set([...thresholds, ...(project.compact_at ? [project.compact_at] : [])])].sort((a, b) => a - b).map((n) => <option key={n} value={n}>at {k(n)}</option>)}
          </select>
        </span>
        <span className="subtle">Keeps</span>
        <span>Open threads, decisions, what it's watching and notes it saved. The rest stays searchable in History.</span>
        <span className="subtle">Can</span>
        <span>Start threads, draft replies, file notes</span>
        <span className="subtle">Asks before</span>
        <span>Sending email, merging, deploying, refunds, posting, running commands and fetching pages. When tab wakes it on its own, also before starting an agent.</span>
      </div>
      {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
    </section>
  );
}
