// A project's end-to-end encrypted vault (EIN, tax returns, bank details,
// IDs): set up, unlock with Touch ID / Windows Hello, entries with masked
// values, the devices that can open it, and who read what.

import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, TextArea, TextInput, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import type { Project, VaultAudit, VaultDevice, VaultEntry, VaultStatus } from "../../lib/types";

const statusKey = ["vault-status"] as const;
const entriesKey = (projectId: string) => ["vault-entries", projectId] as const;
const auditKey = (projectId: string) => ["vault-audit", projectId] as const;

export function useVaultStatus() {
  return useQuery({ queryKey: statusKey, queryFn: api.vaultStatus, refetchInterval: 5000 });
}

/** Opens the vault if it isn't (Touch ID / Windows Hello). Throws when that's not possible here. */
export async function ensureVaultOpen(): Promise<void> {
  const st = await api.vaultStatus();
  if (st.state === "open") return;
  if (st.state === "locked") return api.vaultUnlock();
  throw new Error(st.state === "none" ? "Set up the vault first (Knowledge → Vault)." : "This device can't open the vault yet (Knowledge → Vault).");
}

function minutesLeft(until: string | null): string {
  if (!until) return "";
  const m = Math.max(0, Math.ceil((new Date(until).getTime() - Date.now()) / 60_000));
  return m <= 1 ? "locks in a minute" : `locks in ${m} min`;
}

/** The Vault section of a project's Knowledge page. */
export function VaultSection({ project }: { project: Project }) {
  const qc = useQueryClient();
  const { data: st } = useVaultStatus();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [words, setWords] = useState<string | null>(null);
  const refresh = () => qc.invalidateQueries({ queryKey: ["vault-status"] }).then(() => qc.invalidateQueries({ queryKey: ["vault-entries"] })).then(() => qc.invalidateQueries({ queryKey: ["vault-audit"] }));
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  if (!st) return null;
  const pending = st.devices.filter((d) => d.state === "pending" && !d.this);

  return (
    <section className="vault">
      <header className="vault-head">
        <span className="vault-glyph"><Icon name="lock" size={15} /></span>
        <span className="vault-title">
          <h3>Vault</h3>
          <span className="subtle">End-to-end encrypted. Your devices hold the key; the hub and tab cloud only store ciphertext.</span>
        </span>
        <span className="vault-head-end">
          {st.state === "open" && (
            <>
              <Badge tone="success" icon="check">Open · {minutesLeft(st.open_until)}</Badge>
              <Button size="sm" variant="ghost" icon="lock" onClick={() => run(api.vaultLock)}>Lock</Button>
            </>
          )}
          {st.state === "locked" && <Button size="sm" variant="primary" icon="lock" disabled={busy} onClick={() => run(api.vaultUnlock)}>{st.presence ? `Unlock with ${st.presence}` : "Unlock"}</Button>}
        </span>
      </header>
      {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
      {st.state === "none" && <SetUp busy={busy} onCreate={() => run(async () => setWords(await api.vaultCreate()))} />}
      {(st.state === "no_access" || st.state === "asked") && <Join st={st} busy={busy} run={run} />}
      {st.state === "locked" && (
        <p className="subtle vault-empty">Locked. Unlocking opens it on this device for 10 minutes; only this project's coordinator can search it, and other threads ask you for each field.</p>
      )}
      {pending.length > 0 && <Asking devices={pending} open={st.state === "open"} busy={busy} run={run} />}
      {st.state === "open" && <Entries project={project} />}
      {(st.state === "open" || st.state === "locked") && (
        <div className="vault-cols">
          <Devices st={st} busy={busy} run={run} />
          <Activity project={project} open={st.state === "open"} />
        </div>
      )}
      {words && <RecoveryKey words={words} onDone={() => setWords(null)} />}
    </section>
  );
}

function SetUp({ busy, onCreate }: { busy: boolean; onCreate: () => void }) {
  return (
    <div className="vault-setup">
      <ul>
        <li>For what agents shouldn't read freely: your EIN, tax returns, bank and ID numbers.</li>
        <li>Encrypted on this device before it syncs. Other devices you add can open it; nobody else can, including tab.</li>
        <li>Only the project's coordinator searches it, and values stay masked until you release them.</li>
      </ul>
      <Button variant="primary" icon="lock" disabled={busy} onClick={onCreate}>Set up the vault</Button>
    </div>
  );
}

/** This device has no key yet: ask an open device, or use the recovery key. */
function Join({ st, busy, run }: { st: VaultStatus; busy: boolean; run: (f: () => Promise<unknown>) => void }) {
  const [recovering, setRecovering] = useState(false);
  const [words, setWords] = useState("");
  if (st.state === "asked") {
    return (
      <div className="vault-join">
        <p>Waiting for another device to let <strong>{st.device.name}</strong> in. On a device that has the vault, open a project's Knowledge page and approve it there.</p>
        <div className="vault-code" title="Check the other device shows the same code">{st.device.code}</div>
        <p className="subtle">Approve only if the other device shows this same code.</p>
      </div>
    );
  }
  return (
    <div className="vault-join">
      <p>There's a vault on your other devices, but this one can't open it yet.</p>
      {recovering ? (
        <>
          <TextArea rows={3} autoFocus placeholder="Your 24-word recovery key" value={words} onChange={(e) => setWords(e.target.value)} />
          <span className="toolbar">
            <Button size="sm" variant="primary" disabled={busy || words.trim().split(/\s+/).length < 24} onClick={() => run(() => api.vaultRecover(words))}>Open with recovery key</Button>
            <Button size="sm" variant="ghost" onClick={() => setRecovering(false)}>Cancel</Button>
          </span>
        </>
      ) : (
        <span className="toolbar">
          <Button size="sm" variant="primary" disabled={busy} onClick={() => run(api.vaultAsk)}>Ask another device</Button>
          <Button size="sm" variant="ghost" onClick={() => setRecovering(true)}>Use recovery key</Button>
        </span>
      )}
    </div>
  );
}

function Asking({ devices, open, busy, run }: { devices: VaultDevice[]; open: boolean; busy: boolean; run: (f: () => Promise<unknown>) => void }) {
  return (
    <>
      {devices.map((d) => (
        <div key={d.id} className="vault-ask">
          <Icon name="monitor" size={15} />
          <span className="vault-ask-main">
            <span><strong>{d.name}</strong> wants to open the vault <span className="subtle">· {timeAgo(d.created_at)}</span></span>
            <span className="subtle">Let it in only if it shows <strong className="vault-code-inline">{d.code}</strong>.</span>
          </span>
          <span className="toolbar">
            <Button size="sm" variant="primary" disabled={busy} title={open ? undefined : "Unlocks the vault first"} onClick={() => run(async () => (open || (await api.vaultUnlock()), api.vaultApprove(d.id)))}>It shows {d.code}</Button>
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => run(() => api.vaultDeny(d.id))}>Deny</Button>
          </span>
        </div>
      ))}
    </>
  );
}

function Entries({ project }: { project: Project }) {
  const qc = useQueryClient();
  const { data: entries = [], error } = useQuery({ queryKey: entriesKey(project.id), queryFn: () => api.vaultEntries(project.id) });
  const [editing, setEditing] = useState<VaultEntry | "new" | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const picker = useRef<HTMLInputElement>(null);
  const refresh = () => (qc.invalidateQueries({ queryKey: entriesKey(project.id) }), qc.invalidateQueries({ queryKey: auditKey(project.id) }));
  const addFiles = async (files: File[]) => {
    for (const f of files) await api.vaultFileAdd(project.id, f.name, new Uint8Array(await f.arrayBuffer()));
    refresh();
  };
  const remove = async (e: VaultEntry) => {
    if (!confirm(`Delete ${e.title} from the vault? This can't be undone.`)) return;
    await api.vaultEntryDelete(project.id, e.id);
    refresh();
  };
  const save = async (e: VaultEntry) => setNote(`Saved to ${await api.vaultFileExport(project.id, e.id)}`);

  return (
    <div className="vault-entries">
      <div className="vault-tools">
        <span className="subtle">{entries.length ? `${entries.length} ${entries.length === 1 ? "entry" : "entries"}` : ""}</span>
        <input ref={picker} type="file" multiple hidden onChange={(e) => (addFiles(Array.from(e.target.files ?? [])), (e.target.value = ""))} />
        <span className="toolbar">
          <Button size="sm" icon="upload" onClick={() => picker.current?.click()}>Add file</Button>
          <Button size="sm" variant="primary" icon="plus" onClick={() => setEditing("new")}>Add entry</Button>
        </span>
      </div>
      {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {String(error)}</div>}
      {note && <p className="subtle">{note}</p>}
      {!entries.length && !error && (
        <button className="pjk-drop" onClick={() => setEditing("new")}>
          <Icon name="lock" size={16} />
          Add your EIN, state tax ID, bank details, or drop in a tax return.
        </button>
      )}
      {entries.map((e) => (
        <div key={e.id} className="vault-entry">
          <span className="vault-entry-head">
            <Icon name={e.kind === "file" ? "file" : "lock"} size={13} />
            <strong>{e.title}</strong>
            <span className="subtle">{e.file ? `${(e.file.size / 1024).toFixed(0)} KB · ` : ""}{timeAgo(e.updated_at)}</span>
            <span className="pjk-actions">
              {e.file && <button className="icon-btn" title="Save a decrypted copy to Downloads" onClick={() => save(e)}><Icon name="download" size={13} /></button>}
              {e.kind === "note" && <button className="icon-btn" title="Edit" onClick={() => setEditing(e)}><Icon name="edit" size={13} /></button>}
              <button className="icon-btn" title="Delete" onClick={() => remove(e)}><Icon name="trash" size={13} /></button>
            </span>
          </span>
          {e.fields.map((f) => <FieldRow key={f.label} projectId={project.id} entryId={e.id} label={f.label} masked={f.masked} />)}
          {e.note && <p className="vault-entry-note">{e.note}</p>}
        </div>
      ))}
      {editing && <EntryDialog project={project} entry={editing === "new" ? null : editing} onClose={() => (setEditing(null), refresh())} />}
    </div>
  );
}

/** One field, masked; the eye shows it for 20 seconds (and is logged). */
function FieldRow({ projectId, entryId, label, masked }: { projectId: string; entryId: string; label: string; masked: string }) {
  const [value, setValue] = useState<string | null>(null);
  useEffect(() => {
    if (value === null) return;
    const t = setTimeout(() => setValue(null), 20_000);
    return () => clearTimeout(t);
  }, [value]);
  const get = () => api.vaultReveal(projectId, entryId, label);
  return (
    <div className="vault-field">
      <span className="vault-field-label">{label}</span>
      <code className={cx("vault-field-value", value !== null && "shown")}>{value ?? masked}</code>
      <button className="icon-btn" title={value === null ? "Show" : "Hide"} onClick={async () => setValue(value === null ? await get() : null)}><Icon name={value === null ? "eye" : "eye-off"} size={13} /></button>
      <button className="icon-btn" title="Copy" onClick={async () => navigator.clipboard.writeText(value ?? (await get()))}><Icon name="copy" size={13} /></button>
    </div>
  );
}

function EntryDialog({ project, entry, onClose }: { project: Project; entry: VaultEntry | null; onClose: () => void }) {
  const [title, setTitle] = useState(entry?.title ?? "");
  const [note, setNote] = useState(entry?.note ?? "");
  // A value left empty on an existing field keeps the one it has.
  const [fields, setFields] = useState<{ label: string; value: string; masked?: string }[]>(
    entry?.fields.map((f) => ({ label: f.label, value: "", masked: f.masked })) ?? [{ label: "", value: "" }],
  );
  const [error, setError] = useState<string | null>(null);
  const set = (i: number, k: "label" | "value", v: string) => setFields(fields.map((f, j) => (j === i ? { ...f, [k]: v } : f)));
  const save = async () => {
    try {
      await api.vaultEntrySave(project.id, {
        id: entry?.id,
        title,
        note,
        fields: fields.filter((f) => f.label.trim()).map((f) => ({ label: f.label, value: f.masked && !f.value ? null : f.value })),
      });
      onClose();
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <div className="pj-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="pj-dialog pjk-dialog" role="dialog" aria-label={entry ? `Edit ${entry.title}` : "Add to the vault"} onMouseDown={(e) => e.stopPropagation()}>
        <header className="pjk-dialog-head">
          <h3><Icon name="lock" size={14} /> {entry ? "Edit vault entry" : "Add to the vault"}</h3>
          <button className="icon-btn" onClick={onClose} aria-label="Close"><Icon name="x" size={14} /></button>
        </header>
        <div className="pj-field">
          <span className="label-sm">Name</span>
          <TextInput autoFocus placeholder="Company IDs" value={title} onChange={(e) => setTitle(e.target.value)} />
        </div>
        <div className="pj-field">
          <span className="label-sm">Fields</span>
          {fields.map((f, i) => (
            <div key={i} className="vault-field-edit">
              <TextInput placeholder="EIN" value={f.label} onChange={(e) => set(i, "label", e.target.value)} />
              <TextInput type="password" autoComplete="off" placeholder={f.masked ? `Keep ${f.masked}` : "12-3456789"} value={f.value} onChange={(e) => set(i, "value", e.target.value)} />
              <button className="icon-btn" title="Remove" onClick={() => setFields(fields.filter((_, j) => j !== i))}><Icon name="x" size={13} /></button>
            </div>
          ))}
          <button className="link-btn" onClick={() => setFields([...fields, { label: "", value: "" }])}>+ Add a field</button>
        </div>
        <div className="pj-field">
          <span className="label-sm">Note (optional)</span>
          <TextArea rows={2} placeholder="Where it's from, when it renews" value={note} onChange={(e) => setNote(e.target.value)} />
        </div>
        {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
        <footer className="pj-dialog-foot">
          <span className="subtle pjk-foot-note">Encrypted on this device before it's saved.</span>
          <span className="pj-dialog-actions">
            <Button size="sm" variant="ghost" onClick={onClose}>Cancel</Button>
            <Button size="sm" variant="primary" disabled={!title.trim()} onClick={save}>{entry ? "Save" : "Add"}</Button>
          </span>
        </footer>
      </div>
    </div>
  );
}

function Devices({ st, busy, run }: { st: VaultStatus; busy: boolean; run: (f: () => Promise<unknown>) => void }) {
  const active = st.devices.filter((d) => d.state === "active");
  const remove = (d: VaultDevice) => {
    if (!confirm(`Remove ${d.name}? It won't be able to open the vault again, and the vault's key changes for your other devices.`)) return;
    run(() => api.vaultRemoveDevice(d.id));
  };
  return (
    <div className="vault-box">
      <h4>Devices</h4>
      {active.map((d) => (
        <div key={d.id} className="vault-device">
          <Icon name="monitor" size={13} />
          <span className="vault-device-main">
            <span>{d.name}{d.this && <span className="subtle"> · this device</span>}</span>
            <span className="subtle">{d.approved_by ? `let in by ${d.approved_by}` : "set it up"} · {timeAgo(d.created_at)}</span>
          </span>
          <button className="icon-btn" disabled={busy || st.state !== "open"} title={st.state === "open" ? "Remove" : "Unlock to remove a device"} onClick={() => remove(d)}><Icon name="trash" size={13} /></button>
        </div>
      ))}
      <p className="subtle vault-hint">New device: open tab there and choose "Ask another device". Lost them all: the recovery key.</p>
    </div>
  );
}

const actionLook: Record<string, string> = {
  create: "set up the vault", unlock: "unlocked", search: "searched", get: "read", reveal: "saw", request: "asked for",
  approve: "released", deny: "declined", add: "added", edit: "edited", delete: "deleted", device_ask: "asked to join",
  device_approve: "let in a device", device_remove: "removed a device", rotate: "changed the vault key", recover: "used the recovery key", export: "saved a copy of",
};
const actorLook: Record<VaultAudit["actor"], string> = { person: "You", coordinator: "Coordinator", thread: "A thread" };

function Activity({ project, open }: { project: Project; open: boolean }) {
  const { data: log = [] } = useQuery({ queryKey: auditKey(project.id), queryFn: () => api.vaultAudit(project.id), refetchInterval: 10_000 });
  return (
    <div className="vault-box">
      <h4>Activity <span className="subtle">every read is logged</span></h4>
      {!log.length && <p className="subtle">Nothing yet.</p>}
      <div className="vault-log">
        {log.slice(0, 30).map((a) => {
          const d = a.detail;
          const what = d?.fields?.length ? d.fields.join(", ") : d?.title ?? d?.device ?? (d?.query ? `"${d.query}"` : "");
          return (
            <div key={a.id} className="vault-log-row" title={d?.why ? `Why: ${d.why}` : undefined}>
              <span className={cx("vault-actor", a.actor)}>{actorLook[a.actor] ?? a.actor}</span>
              <span className="vault-log-main">
                {actionLook[a.action] ?? a.action} {what && <strong>{what}</strong>}
                {d?.why && <span className="subtle"> · {d.why}</span>}
              </span>
              <span className="subtle">{timeAgo(a.at)}</span>
            </div>
          );
        })}
      </div>
      {!open && log.length > 0 && <p className="subtle vault-hint">Unlock to see what each entry read.</p>}
    </div>
  );
}

/** The recovery key, shown once. */
function RecoveryKey({ words, onDone }: { words: string; onDone: () => void }) {
  const [saved, setSaved] = useState(false);
  const list = words.split(" ");
  return (
    <div className="pj-backdrop">
      <div className="pj-dialog pjk-dialog" role="dialog" aria-label="Your recovery key">
        <header className="pjk-dialog-head"><h3><Icon name="lock" size={14} /> Your recovery key</h3></header>
        <p>If you lose every device, these 24 words are the only way back into the vault. tab doesn't keep them and can't recover them. Write them down or put them in your password manager.</p>
        <ol className="vault-words">{list.map((w, i) => <li key={i}><span className="subtle">{i + 1}</span> {w}</li>)}</ol>
        <label className="pjk-check"><input type="checkbox" checked={saved} onChange={(e) => setSaved(e.target.checked)} /> I've saved them somewhere safe</label>
        <footer className="pj-dialog-foot">
          <Button size="sm" icon="copy" onClick={() => navigator.clipboard.writeText(words)}>Copy</Button>
          <span className="pj-dialog-actions"><Button size="sm" variant="primary" disabled={!saved} onClick={onDone}>Done</Button></span>
        </footer>
      </div>
    </div>
  );
}
