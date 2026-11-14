// What a project knows: its resources (sites, services, email, documents,
// notes) with who may read each, and its repos. The rail sits beside the
// coordinator on Home; the Knowledge tab lists everything.

import { useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Badge, Button, TextArea, TextInput, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api, openUrl } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import { useProjectRepos, useProjectResources } from "../../lib/hooks";
import { keys } from "../../lib/query";
import type { Project, ProjectResource, ResourceAccess, ResourceKind } from "../../lib/types";
import { accessLook, detect, kindLook, kindOrder, statusLook } from "./resources";
import { VaultSection, ensureVaultOpen } from "./Vault";

const isLink = (url: string | null) => !!url && /^https?:\/\//.test(url);

/** Beside the coordinator: the project's rules, then its resources by kind and its repos. */
export function KnowledgeRail({ project, onAdd, onSeeAll, onEditRules }: { project: Project; onAdd: () => void; onSeeAll: () => void; onEditRules: () => void }) {
  const { data: all = [] } = useProjectResources(project.id);
  const { data: repos = [] } = useProjectRepos(project.id);
  // Vault entries say nothing until opened; they're counted, not listed.
  const resources = useMemo(() => all.filter((r) => r.access !== "vault"), [all]);
  const vaulted = all.length - resources.length;
  const groups = useMemo(() => kindOrder.map((k) => [k, resources.filter((r) => r.kind === k)] as const).filter(([, rs]) => rs.length), [resources]);

  return (
    <aside className="pjk-rail">
      <section className="pjk-rail-sec">
        <div className="pjk-rail-head">
          <h3>Instructions</h3>
          <Button size="sm" variant="ghost" onClick={onEditRules}>{project.rules ? "Edit" : "Add"}</Button>
        </div>
        {project.rules ? <div className="pjk-instr">{project.rules}</div> : <p className="subtle pjk-empty">How agents should work here. Every chat in the project reads these.</p>}
      </section>
      <section className="pjk-rail-sec">
        <div className="pjk-rail-head">
          <h3>Resources <span className="subtle">{resources.length + repos.length}</span></h3>
          <Button size="sm" icon="plus" onClick={onAdd}>Add</Button>
        </div>
        {!resources.length && !repos.length && (
          <button className="pjk-drop" onClick={onAdd}>
            <Icon name="link" size={16} />
            Paste a website, a Cloudflare or Stripe link, a Google Doc, an email label…
          </button>
        )}
        {groups.map(([kind, rs]) => (
          <div key={kind} className="pjk-group">
            <div className="pjk-group-head"><span>{kindLook[kind].plural}</span><span>{rs.length}</span></div>
            {rs.map((r) => <RailRow key={r.id} r={r} />)}
          </div>
        ))}
        {vaulted > 0 && (
          <div className="pjk-group">
            <div className="pjk-group-head"><span>Vault</span><span>{vaulted}</span></div>
            <button className="pjk-row" onClick={onSeeAll}>
              <span className="pjk-glyph restricted"><Icon name="lock" size={13} /></span>
              <span className="pjk-row-main">
                <span className="pjk-row-title">{vaulted} encrypted {vaulted === 1 ? "entry" : "entries"}</span>
                <span className="pjk-row-sub">Coordinator only, after you unlock</span>
              </span>
            </button>
          </div>
        )}
        {repos.length > 0 && (
          <div className="pjk-group">
            <div className="pjk-group-head"><span>Repos</span><span>{repos.length}</span></div>
            {repos.map((r) => (
              <div key={r.id} className="pjk-row">
                <span className="pjk-glyph"><Icon name="git-branch" size={13} /></span>
                <span className="pjk-row-main">
                  <span className="pjk-row-title">{r.repo}</span>
                  <span className="pjk-row-sub">{r.path ?? r.note ?? ""}</span>
                </span>
              </div>
            ))}
          </div>
        )}
        {resources.length + repos.length > 0 && <button className="pjk-more" onClick={onSeeAll}>See all in Knowledge <Icon name="arrow-right" size={12} /></button>}
      </section>
    </aside>
  );
}

function RailRow({ r }: { r: ProjectResource }) {
  const st = statusOf(r);
  return (
    <button className="pjk-row" onClick={() => isLink(r.url) && openUrl(r.url!)} title={r.note || r.url || r.title}>
      <span className={cx("pjk-glyph", r.access !== "all" && "restricted")}><Icon name={r.access === "all" ? kindLook[r.kind].icon : "lock"} size={13} /></span>
      <span className="pjk-row-main">
        <span className="pjk-row-title">{r.title}</span>
        <span className="pjk-row-sub">{r.access === "all" ? r.note || r.url : accessLook[r.access].label}</span>
      </span>
      {r.pinned ? <Icon name="pin" size={12} className="subtle" /> : r.status !== "pending" && <Badge tone={st.tone} title={st.help}>{st.label}</Badge>}
    </button>
  );
}

/** The Knowledge tab: every resource with its access and status, then the repos. */
export function KnowledgeView({ project, onAdd, repos }: { project: Project; onAdd: () => void; repos: React.ReactNode }) {
  const qc = useQueryClient();
  const { data: all = [] } = useProjectResources(project.id);
  const resources = useMemo(() => all.filter((r) => r.access !== "vault"), [all]);
  const [kind, setKind] = useState<ResourceKind | null>(null);
  const [query, setQuery] = useState("");
  const [editing, setEditing] = useState<ProjectResource | null>(null);
  const refresh = () => qc.invalidateQueries({ queryKey: keys.resources(project.id) });

  const counts = useMemo(() => kindOrder.map((k) => [k, resources.filter((r) => r.kind === k).length] as const).filter(([, n]) => n), [resources]);
  const q = query.trim().toLowerCase();
  const shown = resources.filter((r) => (!kind || r.kind === kind) && (!q || `${r.title} ${r.url ?? ""} ${r.note}`.toLowerCase().includes(q)));
  const restricted = resources.filter((r) => r.access !== "all").length;

  const pin = async (r: ProjectResource) => (await api.projectResourceSave(project.id, { ...r, pinned: !r.pinned }), refresh());
  const reindex = async (r: ProjectResource) => (await api.projectResourceReindex(r.id), refresh());
  const remove = async (r: ProjectResource) => (await api.projectResourceDelete(project.id, r.id), refresh());

  return (
    <div className="pjk-page">
      <div className="pjk-stats">
        <Stat value={resources.length} label="Resources" />
        <Stat value={resources.filter((r) => r.status === "indexed" || r.status === "live").length} label="Ready for agents" />
        <Stat value={resources.reduce((n, r) => n + r.chunks, 0)} label="Chunks indexed" />
        <Stat value={restricted} label="Coordinator only" />
      </div>
      <div className="pjk-tools">
        <TextInput icon="search" placeholder="Filter resources" value={query} onChange={(e) => setQuery(e.target.value)} />
        <button className={cx("pjk-chip", !kind && "on")} onClick={() => setKind(null)}>All <span>{resources.length}</span></button>
        {counts.map(([k, n]) => (
          <button key={k} className={cx("pjk-chip", kind === k && "on")} onClick={() => setKind(kind === k ? null : k)}>
            <Icon name={kindLook[k].icon} size={12} /> {kindLook[k].plural} <span>{n}</span>
          </button>
        ))}
        <span className="pjk-tools-end"><Button variant="primary" size="sm" icon="plus" onClick={onAdd}>Add resource</Button></span>
      </div>
      {resources.length ? (
        <div className="pjk-table" role="table">
          <div className="pjk-tr pjk-th" role="row">
            <span>Resource</span><span>Kind</span><span>Who can read</span><span>Status</span><span>Last used</span><span />
          </div>
          {shown.map((r) => {
            const st = statusOf(r);
            const ac = accessLook[r.access];
            return (
              <div key={r.id} className="pjk-tr" role="row">
                <span className="pjk-cell-main">
                  <span className={cx("pjk-glyph", r.access !== "all" && "restricted")}><Icon name={kindLook[r.kind].icon} size={14} /></span>
                  <span className="pjk-row-main">
                    <span className="pjk-row-title">
                      {isLink(r.url) ? <a href={r.url!} onClick={(e) => (e.preventDefault(), openUrl(r.url!))}>{r.title}</a> : r.title}
                      {r.pinned && <Icon name="pin" size={11} className="subtle" />}
                    </span>
                    <span className="pjk-row-sub">{[r.note || (r.kind !== "note" ? r.url : null), `${r.added_by === "agent" ? "agent" : "you"} · ${timeAgo(r.updated_at)}`].filter(Boolean).join(" · ")}</span>
                  </span>
                </span>
                <span className="subtle">{kindLook[r.kind].label}</span>
                <span><Badge tone={ac.tone} icon={r.access !== "all" ? "lock" : undefined} title={ac.help}>{ac.label}</Badge></span>
                <span><Badge tone={st.tone} title={statusHelp(r)}>{st.label}{r.chunks ? ` · ${r.chunks}` : ""}</Badge></span>
                <span className="subtle" title={r.last_used_at ? "When an agent's search last used it, on this computer" : "No agent has used it yet"}>{r.last_used_at ? timeAgo(r.last_used_at) : "—"}</span>
                <span className="pjk-actions">
                  {indexable.includes(r.kind) && <button className="icon-btn" title="Read it again now" onClick={() => reindex(r)}><Icon name="refresh" size={13} /></button>}
                  <button className="icon-btn" title={r.pinned ? "Unpin" : "Pin: always in the coordinator's context"} onClick={() => pin(r)}><Icon name="pin" size={13} /></button>
                  <button className="icon-btn" title="Edit" onClick={() => setEditing(r)}><Icon name="edit" size={13} /></button>
                  <button className="icon-btn" title="Remove" onClick={() => remove(r)}><Icon name="trash" size={13} /></button>
                </span>
              </div>
            );
          })}
          {!shown.length && <p className="subtle pjk-empty">Nothing matches.</p>}
        </div>
      ) : (
        <button className="pjk-drop big" onClick={onAdd}>
          <Icon name="link" size={20} />
          <strong>Give this project what it needs to know</strong>
          <span>Websites, a Cloudflare or Stripe link, Google Docs, an email label or sender, notes. Agents read them; the coordinator reads the private ones too.</span>
        </button>
      )}
      <VaultSection project={project} />
      <section className="pjk-repos">{repos}</section>
      {editing && <AddResource project={project} resource={editing} onClose={() => setEditing(null)} />}
    </div>
  );
}

/** Kinds tab reads into search. */
const indexable: ResourceKind[] = ["website", "file", "email", "note"];

/** A resource's status look; one from a newer app reads as not indexed yet. */
const statusOf = (r: ProjectResource) => statusLook[r.status] ?? statusLook.pending;

/** The status badge's hover: why it failed, or when it was last read. */
function statusHelp(r: ProjectResource): string {
  if (r.status === "link_only") return `${r.index_error ? `${r.index_error}. ` : ""}Its name, link and note are searchable.`;
  if (r.index_error) return r.index_error;
  if (r.indexed_at) return `${statusOf(r).help} Read ${timeAgo(r.indexed_at)}.`;
  return statusOf(r).help;
}

function Stat({ value, label }: { value: number; label: string }) {
  return (
    <div className="pjk-stat">
      <span className="pjk-stat-v">{value.toLocaleString()}</span>
      <span className="pjk-stat-k">{label}</span>
    </div>
  );
}

const pickable: ResourceKind[] = ["website", "service", "link", "email", "file", "note"];

const fileTypes = ".pdf,.docx,.xlsx,.md,.mdx,.markdown,.txt,.csv,.tsv,.json,.html,.htm,.rst,.org,.yaml,.yml,.toml,.xml,.log";

/** Paste a link (or a label, or write a note); tab works out what it is. Also edits one. */
export function AddResource({ project, resource, onClose }: { project: Project; resource?: ProjectResource; onClose: () => void }) {
  const qc = useQueryClient();
  const [text, setText] = useState(resource?.url ?? "");
  const found = useMemo(() => (resource ? null : detect(text)), [text, resource]);
  const [kind, setKind] = useState<ResourceKind | null>(resource?.kind ?? null);
  const [title, setTitle] = useState<string | null>(resource?.title ?? null);
  const [note, setNote] = useState(resource?.note ?? "");
  const [access, setAccess] = useState<ResourceAccess>(resource?.access ?? "all");
  const [options, setOptions] = useState<Record<string, unknown>>(resource?.options ?? { depth: 1, schedule: "daily" });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [files, setFiles] = useState<File[]>([]);
  const picker = useRef<HTMLInputElement>(null);

  const k: ResourceKind = kind ?? found?.kind ?? "link";
  const t = title ?? found?.title ?? "";
  const repo = !resource && !kind ? found?.repo : undefined;
  const opt = (key: string, v: unknown) => setOptions((o) => ({ ...o, [key]: v }));
  const uploading = k === "file" && !resource;
  // The vault keeps notes and files; editing an entry happens in the Vault.
  const vaultable = k === "note" || k === "file";
  const canSave = (access !== "vault" || (vaultable && !resource && !repo)) && (uploading ? files.length > 0 : repo || (t.trim() && (k === "note" ? note.trim() : text.trim())));

  const save = async () => {
    if (!canSave || busy) return;
    setBusy(true);
    setError(null);
    try {
      if (access === "vault") {
        // Sealed on this device, so the vault has to be open.
        await ensureVaultOpen();
        if (uploading) for (const f of files) await api.vaultFileAdd(project.id, f.name, new Uint8Array(await f.arrayBuffer()));
        else await api.vaultEntrySave(project.id, { title: t, note, fields: [] });
        await qc.invalidateQueries({ queryKey: ["vault-entries"] });
        await qc.invalidateQueries({ queryKey: keys.resources(project.id) });
      } else if (uploading) {
        for (const f of files) await api.projectFileAdd(project.id, f.name, new Uint8Array(await f.arrayBuffer()), access);
        await qc.invalidateQueries({ queryKey: keys.resources(project.id) });
      } else if (repo) {
        await api.projectRepoSave(project.id, { repo, note: note || null });
        await qc.invalidateQueries({ queryKey: keys.repos(project.id) });
      } else {
        await api.projectResourceSave(project.id, { id: resource?.id, kind: k, title: t, url: k === "note" ? null : (found?.url ?? text.trim()), note, access, options });
        await qc.invalidateQueries({ queryKey: keys.resources(project.id) });
      }
      onClose();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  return (
    <div className="pj-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="pj-dialog pjk-dialog" role="dialog" aria-label={resource ? `Edit ${resource.title}` : `Add to ${project.name}`} onMouseDown={(e) => e.stopPropagation()}>
        <header className="pjk-dialog-head">
          <h3>{resource ? "Edit resource" : `Add to ${project.name}`}</h3>
          <button className="icon-btn" onClick={onClose} aria-label="Close"><Icon name="x" size={14} /></button>
        </header>
        {uploading ? (
          <>
            <input ref={picker} type="file" multiple hidden accept={fileTypes} onChange={(e) => (setFiles([...files, ...Array.from(e.target.files ?? [])]), (e.target.value = ""))} />
            <button className="pjk-drop" onClick={() => picker.current?.click()}
              onDragOver={(e) => e.preventDefault()} onDrop={(e) => (e.preventDefault(), setFiles([...files, ...Array.from(e.dataTransfer.files)]))}>
              <Icon name="upload" size={16} />
              {files.length ? `${files.length} file${files.length > 1 ? "s" : ""}: ${files.map((f) => f.name).join(", ")}` : "Choose or drop files: PDF, Word, Excel, markdown or text"}
            </button>
          </>
        ) : k !== "note" && k !== "file" ? (
          <TextInput autoFocus icon="link" placeholder="Paste a link, or label:billing, or someone@company.com" value={text} onChange={(e) => (setText(e.target.value), setTitle(null))}
            onKeyDown={(e) => e.key === "Enter" && save()} />
        ) : null}
        {found && !uploading && (
          <div className="pjk-found">
            <span className="pjk-glyph"><Icon name={repo ? "git-branch" : kindLook[found.kind].icon} size={14} /></span>
            <span className="pjk-row-main">
              <span className="pjk-row-title">{repo ?? found.title}</span>
              <span className="pjk-row-sub">{found.what}</span>
            </span>
          </div>
        )}
        {!repo && (
          <>
            <div className="pj-field">
              <span className="label-sm">Kind</span>
              <div className="pjk-kinds">
                {pickable.map((x) => (
                  <button key={x} className={cx("pjk-chip", k === x && "on")} onClick={() => setKind(x)}><Icon name={kindLook[x].icon} size={12} /> {kindLook[x].label}</button>
                ))}
              </div>
            </div>
            {!uploading && <div className="pj-field">
              <span className="label-sm">Name</span>
              <TextInput placeholder={k === "note" ? "Refund policy" : "What to call it"} value={t} onChange={(e) => setTitle(e.target.value)} autoFocus={k === "note"} />
            </div>}
            {!uploading && <div className="pj-field">
              <span className="label-sm">{k === "note" ? "Note" : "What it's for (optional)"}</span>
              <TextArea rows={k === "note" ? 5 : 2} placeholder={k === "note" ? "Anything agents should know: a policy, numbers, how something works." : "e.g. where customers pay; deploys of the marketing site"} value={note} onChange={(e) => setNote(e.target.value)} />
            </div>}
            {k === "website" && (
              <div className="pjk-opts">
                <label>Follow links <select className="select" value={String(options.depth ?? 1)} onChange={(e) => opt("depth", Number(e.target.value))}>
                  <option value="0">this page only</option><option value="1">1 level deep</option><option value="2">2 levels deep</option>
                </select></label>
                <label>Refresh <select className="select" value={String(options.schedule ?? "daily")} onChange={(e) => opt("schedule", e.target.value)}>
                  <option value="hourly">hourly</option><option value="daily">daily</option><option value="weekly">weekly</option>
                </select></label>
              </div>
            )}
            {k === "service" && (
              <div className="pjk-opts col">
                {found?.watch && <label className="pjk-check"><input type="checkbox" checked={!!options.watch} onChange={(e) => opt("watch", e.target.checked)} /> {found.watch}</label>}
                <label className="pjk-check"><input type="checkbox" checked={options.agents_change === true} onChange={(e) => opt("agents_change", e.target.checked)} /> Let agents make changes there <span className="subtle">(they still ask first)</span></label>
              </div>
            )}
            <div className="pj-field">
              <span className="label-sm">Who can read it</span>
              <div className="pjk-access">
                {(Object.keys(accessLook) as ResourceAccess[]).map((a) => (
                  <button key={a} className={cx("pjk-access-opt", access === a && "on")} disabled={a === "vault" && (!!resource || !vaultable)} title={a === "vault" && !vaultable ? "The vault keeps notes and files" : undefined} onClick={() => setAccess(a)}>
                    <span className="pjk-access-label">{accessLook[a].icon && <Icon name={accessLook[a].icon!} size={12} />}{accessLook[a].label}</span>
                    <span className="pjk-access-help">{accessLook[a].help}</span>
                  </button>
                ))}
              </div>
            </div>
          </>
        )}
        {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
        <footer className="pj-dialog-foot">
          <span className="subtle pjk-foot-note">{access === "vault" ? "Encrypted on this device; for fields like an EIN, use Add entry in the Vault." : "Coordinator-only resources are never offered to other threads."}</span>
          <span className="pj-dialog-actions">
            <Button size="sm" variant="ghost" onClick={onClose}>Cancel</Button>
            <Button size="sm" variant="primary" disabled={!canSave || busy} onClick={save}>{resource ? "Save" : repo ? "Add repo" : uploading ? (busy ? "Adding…" : files.length > 1 ? `Add ${files.length} files` : "Add file") : "Add resource"}</Button>
          </span>
        </footer>
      </div>
    </div>
  );
}
