import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button, TextArea, TextInput, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import { useProjectNotes, useProjects } from "../../lib/hooks";
import { keys } from "../../lib/query";
import type { Project, ProjectNote } from "../../lib/types";

export const projectColors = ["#7c5cff", "#3b82f6", "#06b6d4", "#10b981", "#84cc16", "#f59e0b", "#f97316", "#e5484d", "#ec4899", "#8b8d98"];
export const projectIcons = [
  "folder", "zap", "code", "terminal", "git-branch", "shield", "lock", "sparkles", "bot", "graph",
  "activity", "gauge", "mail", "message", "ticket", "store", "cloud", "user", "files", "eye",
];

/** A project's icon on a tint of its color. */
export function ProjectMark({ project, size = 18 }: { project: Pick<Project, "color" | "icon">; size?: number }) {
  return (
    <span className="pj-mark" style={{ width: size, height: size, color: project.color, background: `color-mix(in srgb, ${project.color} 18%, transparent)` }}>
      <Icon name={project.icon} size={Math.round(size * 0.64)} />
    </span>
  );
}

/**
 * Props that tint a chat with its project's color (see `.has-project` in
 * app.css): the user's messages, the send button, the composer and a strip
 * along the top. Nothing when there's no project.
 */
export function useProjectTint(id: string | null | undefined): { className?: string; style?: React.CSSProperties } {
  const { data: projects = [] } = useProjects();
  const p = id ? projects.find((x) => x.id === id) : undefined;
  return p ? { className: "has-project", style: { "--project": p.color } as React.CSSProperties } : {};
}

/** A small chip naming an item's project. */
export function ProjectChip({ id }: { id: string | null | undefined }) {
  const { data: projects = [] } = useProjects();
  const p = id ? projects.find((x) => x.id === id) : undefined;
  if (!p) return null;
  return (
    <span className="pj-chip" style={{ color: p.color }} title={`Project: ${p.name}`}>
      <Icon name={p.icon} size={11} />
      <span>{p.name}</span>
    </span>
  );
}

/**
 * A project id remembered in localStorage under `key`. Falls back to null
 * once that project is deleted.
 */
export function useRememberedProject(key: string): [string | null, (id: string | null) => void] {
  const { data: projects } = useProjects();
  const [id, setId] = useState<string | null>(() => {
    try {
      return localStorage.getItem(key) || null;
    } catch {
      return null;
    }
  });
  const set = (v: string | null) => {
    setId(v);
    try {
      if (v) localStorage.setItem(key, v);
      else localStorage.removeItem(key);
    } catch {
      /* private mode: just not remembered */
    }
  };
  const valid = id && projects && !projects.some((p) => p.id === id) ? null : id;
  return [valid, set];
}

/**
 * Picks a project: a button showing the current one and a menu of all of
 * them, with "New project" and an edit button on each.
 *
 * `none` names the empty choice: "No project" when assigning, "All projects"
 * when filtering.
 */
export function ProjectPicker({ value, onChange, none = "No project", align = "left", compact }: {
  value: string | null;
  onChange: (id: string | null) => void;
  none?: string;
  align?: "left" | "right";
  compact?: boolean;
}) {
  const { data: projects = [] } = useProjects();
  const [open, setOpen] = useState(false);
  const [editing, setEditing] = useState<Project | "new" | null>(null);
  const ref = useRef<HTMLSpanElement>(null);
  const current = projects.find((p) => p.id === value);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => (window.removeEventListener("mousedown", onDown), window.removeEventListener("keydown", onKey));
  }, [open]);

  const pick = (id: string | null) => (onChange(id), setOpen(false));

  return (
    <span className="menu-anchor pj-picker" ref={ref}>
      <button className={cx("pj-trigger", compact && "compact", open && "active", !current && "empty")} onClick={() => setOpen((v) => !v)} aria-haspopup="menu" aria-expanded={open}>
        {current ? <ProjectMark project={current} size={compact ? 16 : 18} /> : <Icon name="folder" size={13} />}
        <span className="pj-trigger-name">{current?.name ?? none}</span>
        <Icon name="chevron-down" size={12} />
      </button>
      {open && (
        <div className={cx("menu pj-menu", align === "right" && "right")} role="menu">
          <button role="menuitemradio" aria-checked={!value} className={cx(!value && "active")} onClick={() => pick(null)}>
            <span className="pj-option"><span className="pj-mark pj-mark-none"><Icon name={none === "All projects" ? "inbox" : "x"} size={11} /></span>{none}</span>
            {!value && <Icon name="check" size={13} />}
          </button>
          {projects.length > 0 && <div className="pj-menu-sep" />}
          {projects.map((p) => (
            <div key={p.id} className={cx("pj-menu-row", p.id === value && "active")}>
              <button role="menuitemradio" aria-checked={p.id === value} onClick={() => pick(p.id)}>
                <span className="pj-option"><ProjectMark project={p} />{p.name}</span>
                {p.id === value && <Icon name="check" size={13} />}
              </button>
              <button className="pj-edit" title={`Edit ${p.name}`} aria-label={`Edit ${p.name}`} onClick={() => (setEditing(p), setOpen(false))}>
                <Icon name="edit" size={12} />
              </button>
            </div>
          ))}
          <div className="pj-menu-sep" />
          <button onClick={() => (setEditing("new"), setOpen(false))}>
            <span className="pj-option"><Icon name="plus" size={13} /> New project</span>
          </button>
        </div>
      )}
      {editing && (
        <ProjectEditor
          project={editing === "new" ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={(p) => editing === "new" && onChange(p.id)}
          onDeleted={(id) => id === value && onChange(null)}
        />
      )}
    </span>
  );
}

/** Create or edit a project: name, color, icon, and the notes agents left on it. */
export function ProjectEditor({ project, onClose, onSaved, onDeleted }: {
  project: Project | null;
  onClose: () => void;
  onSaved?: (p: Project) => void;
  onDeleted?: (id: string) => void;
}) {
  const qc = useQueryClient();
  const { data: projects = [] } = useProjects();
  const [name, setName] = useState(project?.name ?? "");
  const [color, setColor] = useState(project?.color ?? projectColors[projects.length % projectColors.length]);
  const [icon, setIcon] = useState(project?.icon ?? "folder");
  const [error, setError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && (e.stopPropagation(), onClose());
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const save = async () => {
    try {
      const p = await api.projectSave({ id: project?.id, name, color, icon });
      await qc.invalidateQueries({ queryKey: keys.projects });
      onSaved?.(p);
      onClose();
    } catch (e) {
      setError(String(e));
    }
  };
  const remove = async () => {
    if (!project) return;
    await api.projectDelete(project.id);
    await qc.invalidateQueries({ queryKey: keys.projects });
    qc.invalidateQueries({ queryKey: keys.allItems });
    onDeleted?.(project.id);
    onClose();
  };

  return (
    <div className="pj-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="pj-dialog" role="dialog" aria-label={project ? `Edit ${project.name}` : "New project"} onMouseDown={(e) => e.stopPropagation()}>
        <header className="pj-dialog-head">
          <ProjectMark project={{ color, icon }} size={32} />
          <TextInput
            autoFocus
            placeholder="Project name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && name.trim() && save()}
          />
        </header>
        <div className="pj-field">
          <span className="label-sm">Color</span>
          <div className="pj-swatches">
            {projectColors.map((c) => (
              <button key={c} className={cx("pj-swatch", c === color && "active")} style={{ background: c }} onClick={() => setColor(c)} aria-label={c} />
            ))}
          </div>
        </div>
        <div className="pj-field">
          <span className="label-sm">Icon</span>
          <div className="pj-icons">
            {projectIcons.map((i) => (
              <button key={i} className={cx("pj-icon", i === icon && "active")} style={i === icon ? { color, borderColor: color } : undefined} onClick={() => setIcon(i)} aria-label={i}>
                <Icon name={i} size={15} />
              </button>
            ))}
          </div>
        </div>
        {project && <ProjectNotes projectId={project.id} />}
        {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
        <footer className="pj-dialog-foot">
          {project ? (
            confirmDelete ? (
              <span className="pj-confirm">
                Delete {project.name}? Its chats stay, without a project.
                <Button size="sm" variant="danger" onClick={remove}>Delete</Button>
              </span>
            ) : (
              <Button size="sm" variant="ghost" icon="trash" onClick={() => setConfirmDelete(true)}>Delete</Button>
            )
          ) : <span />}
          <span className="pj-dialog-actions">
            <Button size="sm" onClick={onClose}>Cancel</Button>
            <Button size="sm" variant="primary" disabled={!name.trim()} onClick={save}>{project ? "Save" : "Create project"}</Button>
          </span>
        </footer>
      </div>
    </div>
  );
}

/** What agents (and the person) have written down about a project. */
export function ProjectNotes({ projectId }: { projectId: string }) {
  const qc = useQueryClient();
  const { data: notes = [] } = useProjectNotes(projectId);
  const [adding, setAdding] = useState(false);
  const refresh = () => qc.invalidateQueries({ queryKey: keys.notes(projectId) });

  return (
    <div className="pj-field">
      <div className="pj-notes-head">
        <span className="label-sm">Notes <span className="subtle">· what agents start each chat with</span></span>
        {!adding && <Button size="sm" variant="ghost" icon="plus" onClick={() => setAdding(true)}>Add</Button>}
      </div>
      <div className="pj-notes">
        {adding && <NoteEditor onCancel={() => setAdding(false)} onSave={async (topic, text) => (await api.projectNoteSave(projectId, { topic, text }), setAdding(false), refresh())} />}
        {notes.map((n) => <NoteRow key={n.id} note={n} onChanged={refresh} />)}
        {!notes.length && !adding && <p className="subtle pj-notes-empty">Nothing yet. Agents in this project write down what they learn here, like how it deploys or who owns what.</p>}
      </div>
    </div>
  );
}

function NoteRow({ note, onChanged }: { note: ProjectNote; onChanged: () => void }) {
  const [editing, setEditing] = useState(false);
  if (editing)
    return (
      <NoteEditor
        note={note}
        onCancel={() => setEditing(false)}
        onSave={async (_, text) => (await api.projectNoteSave(note.project_id, { id: note.id, topic: note.topic, text }), setEditing(false), onChanged())}
      />
    );
  return (
    <div className="pj-note">
      <div className="pj-note-main">
        <div className="pj-note-top">
          <span className="pj-note-topic">{note.topic}</span>
          <span className="subtle">
            {note.added_by === "agent" ? <><Icon name="bot" size={11} /> agent</> : "you"} · {timeAgo(note.updated_at)}
            {note.sources.length > 0 && ` · ${note.sources.length} source${note.sources.length > 1 ? "s" : ""}`}
          </span>
        </div>
        <div className="pj-note-text">{note.text}</div>
      </div>
      <div className="pj-note-actions">
        <button className="icon-btn" title="Edit" aria-label="Edit note" onClick={() => setEditing(true)}><Icon name="edit" size={13} /></button>
        <button className="icon-btn" title="Delete" aria-label="Delete note" onClick={async () => (await api.projectNoteDelete(note.id), onChanged())}><Icon name="x" size={13} /></button>
      </div>
    </div>
  );
}

function NoteEditor({ note, onSave, onCancel }: { note?: ProjectNote; onSave: (topic: string, text: string) => void; onCancel: () => void }) {
  const [topic, setTopic] = useState(note?.topic ?? "");
  const [text, setText] = useState(note?.text ?? "");
  const ok = topic.trim() && text.trim();
  return (
    <div className="pj-note editing">
      {!note && <TextInput placeholder="Topic, e.g. deploys" value={topic} onChange={(e) => setTopic(e.target.value)} autoFocus />}
      <TextArea rows={2} placeholder="What should agents know?" value={text} onChange={(e) => setText(e.target.value)} autoFocus={!!note} />
      <span className="pj-dialog-actions">
        <Button size="sm" variant="ghost" onClick={onCancel}>Cancel</Button>
        <Button size="sm" variant="primary" disabled={!ok} onClick={() => ok && onSave(topic.trim(), text.trim())}>Save</Button>
      </span>
    </div>
  );
}
