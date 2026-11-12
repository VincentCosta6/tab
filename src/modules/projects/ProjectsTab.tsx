// Projects: where you run a piece of work, or a whole business. Each has a
// coordinator (one long-running chat that watches the project and answers
// about it), its queue of threads, and what it knows: resources (sites,
// services, email, documents) with who may read each, repos and notes.
// Agents read the same things with tab_project. See docs/projects-hub.md.

import { useEffect, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, EmptyState, TextArea, TextInput, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import { useAgentActivity, useProjectItems, useProjectRepos, useProjectResources, useProjects } from "../../lib/hooks";
import { keys } from "../../lib/query";
import type { Chat, Project, ProjectRepo, QueueItem } from "../../lib/types";
import { AgentStatus, Composer, Transcript } from "../work/Chat";
import { ItemPane } from "../work/ItemPane";
import { NewChat } from "../work/NewChat";
import { actOnItem, type ItemAction } from "../work/optimistic";
import { ProjectEditor, ProjectMark, ProjectNotes, useProjectTint, useRememberedProject } from "../work/Projects";
import { taskStatus, unread, useContextStatus, useSeen, type TaskStatus } from "../work/taskStatus";
import { QueueRow } from "../work/WorkTab";
import { AddResource, KnowledgeRail, KnowledgeView } from "./Knowledge";
import "./projects.css";

export function ProjectsTab() {
  const { data: projects = [] } = useProjects();
  const [selected, setSelected] = useRememberedProject("tab.projects.selected");
  const [creating, setCreating] = useState(false);
  const project = projects.find((p) => p.id === selected) ?? projects[0];

  return (
    <div className="pjt">
      <aside className="pjt-list">
        <header className="pjt-list-head">
          <h2>Projects</h2>
          <Button size="sm" icon="plus" onClick={() => setCreating(true)}>New</Button>
        </header>
        <div className="pjt-list-items">
          {projects.map((p) => <ProjectListItem key={p.id} project={p} active={p.id === project?.id} onSelect={() => setSelected(p.id)} />)}
          {!projects.length && <p className="subtle pjt-empty-note">No projects yet.</p>}
        </div>
        <p className="subtle pjt-list-foot">Work outside a project stays in Work.</p>
      </aside>
      <main className="pjt-main">
        {project ? (
          <ProjectPage key={project.id} project={project} />
        ) : (
          <EmptyState icon="folder" title="No projects yet">
            Run a piece of work, or a whole business, as a project: a coordinator that watches it, its threads, and everything it knows.
          </EmptyState>
        )}
      </main>
      {creating && <ProjectEditor project={null} onClose={() => setCreating(false)} onSaved={(p) => setSelected(p.id)} />}
    </div>
  );
}

function ProjectListItem({ project: p, active, onSelect }: { project: Project; active: boolean; onSelect: () => void }) {
  const { data: items = [] } = useProjectItems(p.id);
  const activity = useAgentActivity();
  const open = items.filter((i) => i.state !== "done");
  const busy = open.filter((i) => activity.get(i.id)?.status === "needs_input").length;
  return (
    <button className={cx("pjt-list-item", active && "active")} onClick={onSelect}>
      <ProjectMark project={p} size={26} />
      <span className="pjt-list-text">
        <span className="pjt-list-name">{p.name}</span>
        <span className="subtle">{open.length ? `${open.length} open` : p.about || "Nothing open"}</span>
      </span>
      {busy > 0 && <span className="pjt-badge" title={`${busy} waiting on you`}>{busy}</span>}
    </button>
  );
}

type View = "home" | "queue" | "knowledge" | "notes" | "settings";

function ProjectPage({ project }: { project: Project }) {
  const [view, setView] = useState<View>("home");
  const [editing, setEditing] = useState(false);
  const [adding, setAdding] = useState(false);
  const [focus, setFocus] = useState<Focus>({ kind: "coordinator" });
  const { data: items = [] } = useProjectItems(project.id);
  const { data: resources = [] } = useProjectResources(project.id);
  const { data: repos = [] } = useProjectRepos(project.id);
  const tint = useProjectTint(project.id);
  const open = items.filter((i) => i.state !== "done").length;
  const openThread = (id: string) => (setFocus({ kind: "item", id }), setView("home"));

  const tabs: { id: View; label: string; icon: string; count?: number }[] = [
    { id: "home", label: "Home", icon: "sparkles" },
    { id: "queue", label: "Queue", icon: "inbox", count: open },
    { id: "knowledge", label: "Knowledge", icon: "files", count: resources.length + repos.length },
    { id: "notes", label: "Notes", icon: "pin" },
    { id: "settings", label: "Settings", icon: "settings" },
  ];

  return (
    <div className={cx("pjt-page", tint.className)} style={{ ...tint.style, "--pc": project.color } as React.CSSProperties}>
      <header className="pjt-head">
        <ProjectMark project={project} size={34} />
        <div className="pjt-head-text">
          <h1>{project.name}</h1>
          {project.about && <p className="subtle" title={project.about}>{project.about}</p>}
        </div>
        <span className="pjt-head-actions">
          <Button size="sm" icon="plus" onClick={() => setAdding(true)}>Add resource</Button>
          <Button size="sm" variant="primary" icon="message" onClick={() => (setFocus({ kind: "new" }), setView("home"))}>New thread</Button>
        </span>
        <nav className="pjt-tabs" role="tablist">
          {tabs.map((t) => (
            <button key={t.id} role="tab" aria-selected={view === t.id} className={cx("pjt-tab", view === t.id && "on")} onClick={() => setView(t.id)}>
              <Icon name={t.icon} size={13} /> {t.label}{t.count ? <span className="pjt-tab-count">{t.count}</span> : null}
            </button>
          ))}
        </nav>
      </header>
      <div className="pjt-body">
        {view === "home" && (
          <div className="pjh">
            <ProjectQueue project={project} items={items} selected={focus.kind === "item" ? focus.id : null} onOpen={openThread} />
            <Center project={project} focus={focus} onFocus={setFocus} />
            <KnowledgeRail project={project} onAdd={() => setAdding(true)} onSeeAll={() => setView("knowledge")} onEditRules={() => setView("settings")} />
          </div>
        )}
        {view === "queue" && (
          <div className="pjt-scroll"><div className="pjt-wide"><ProjectQueue project={project} items={items} selected={null} onOpen={openThread} full /></div></div>
        )}
        {view === "knowledge" && (
          <div className="pjt-scroll">
            <KnowledgeView project={project} onAdd={() => setAdding(true)} repos={<Repos projectId={project.id} />} />
          </div>
        )}
        {view === "notes" && (
          <div className="pjt-scroll"><div className="pjt-narrow pjt-section"><ProjectNotes projectId={project.id} /></div></div>
        )}
        {view === "settings" && (
          <div className="pjt-scroll">
            <div className="pjt-narrow">
              <div className="pjt-section-head">
                <span className="subtle">Every chat in this project reads these before it starts, and agents keep them up to date.</span>
                <Button size="sm" variant="ghost" icon="edit" onClick={() => setEditing(true)}>Name, color, icon</Button>
              </div>
              <ProfileField project={project} field="about" label="About" placeholder="What is this project, and what does it do?" empty="Nothing yet. An agent fills this in once it learns what the project is, or write it yourself." />
              <ProfileField project={project} field="rules" label="Instructions" placeholder={"How should agents work here? One per line, e.g.\n- Reply to customers short and technical\n- Refunds over $100 need me"} empty="No instructions yet. Add how agents should work here: tone, approvals, what not to touch." />
              <Repos projectId={project.id} />
            </div>
          </div>
        )}
      </div>
      {editing && <ProjectEditor project={project} onClose={() => setEditing(false)} />}
      {adding && <AddResource project={project} onClose={() => setAdding(false)} />}
    </div>
  );
}

type Focus = { kind: "coordinator" } | { kind: "item"; id: string } | { kind: "new" };

/** The middle of Home: the coordinator, or a thread opened from the queue, or a new one. */
function Center({ project, focus, onFocus }: { project: Project; focus: Focus; onFocus: (f: Focus) => void }) {
  const qc = useQueryClient();
  const act = async (item: QueueItem, action: ItemAction) => {
    if (await actOnItem(qc, item, action).catch(() => false)) {
      qc.invalidateQueries({ queryKey: keys.projectItems(project.id) });
      onFocus({ kind: "coordinator" });
    }
  };
  if (focus.kind === "coordinator") return <Coordinator project={project} />;
  return (
    <div className="pjh-center">
      <button className="pjh-back" onClick={() => onFocus({ kind: "coordinator" })}><Icon name="arrow-left" size={13} /> Coordinator</button>
      {focus.kind === "item" ? (
        <ItemPane key={focus.id} id={focus.id} onAct={act} onSelect={(id) => onFocus({ kind: "item", id })} />
      ) : (
        <NewChat project={project.id} onStarted={(id) => (qc.invalidateQueries({ queryKey: keys.projectItems(project.id) }), onFocus({ kind: "item", id }))} />
      )}
    </div>
  );
}

/** The project's coordinator: one ongoing chat that knows the whole project. */
function Coordinator({ project }: { project: Project }) {
  const qc = useQueryClient();
  const key = keys.coordinator(project.id);
  const { data: chat } = useQuery<Chat>({ queryKey: key, queryFn: () => api.coordinatorChatOpen(project.id), staleTime: 0, refetchOnMount: "always" });
  const [error, setError] = useState<string | null>(null);

  const send = async (text: string, attachments: string[] = []) => {
    setError(null);
    qc.setQueryData<Chat>(key, (c) => ({
      session: c?.session ?? null,
      approvals: c?.approvals ?? [],
      running: true,
      messages: [...(c?.messages ?? []), { id: -Date.now(), session_id: c?.session?.id ?? "", role: "user", kind: "text", content: text, meta: attachments.length ? { attachments } : null, created_at: new Date().toISOString() }],
    }));
    try {
      const session = await api.coordinatorChatSend(project.id, text, attachments);
      qc.setQueryData<Chat>(key, (c) => ({ session, approvals: c?.approvals ?? [], running: true, messages: (c?.messages ?? []).filter((m) => m.id > 0) }));
    } catch (e) {
      setError(String(e));
    } finally {
      qc.invalidateQueries({ queryKey: key });
    }
  };

  const suggestions = ["What needs me in this project?", "What changed this week?", `Summarize where ${project.name} stands`];
  const empty = !chat?.messages.length;
  return (
    <div className="pjh-center pjh-coord">
      <header className="pjh-coord-head">
        <span className="pjh-coord-mark"><Icon name="sparkles" size={13} /></span>
        <strong>Coordinator</strong>
        {chat?.session && <AgentStatus chat={chat} />}
      </header>
      {empty ? (
        <div className="pjh-coord-intro">
          <span className="pjh-coord-mark big"><Icon name="sparkles" size={20} /></span>
          <h2>Ask anything about {project.name}</h2>
          <p className="subtle">The coordinator reads this project's queue, threads, notes and resources, including the ones only it may read. Ask it where things stand, or tell it what to get done and it starts threads for it.</p>
          <div className="suggestions">
            {suggestions.map((s) => <button key={s} className="suggestion" onClick={() => send(s)}><Icon name="zap" size={13} /> {s}</button>)}
          </div>
        </div>
      ) : (
        <div className="pjh-transcript"><Transcript chat={chat} onSend={send} /></div>
      )}
      {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
      <Composer chat={chat} onSend={send} placeholder="Ask about the project, or tell the coordinator what to do…" />
    </div>
  );
}

type Group = { id: string; label: string; items: { item: QueueItem; status: TaskStatus | null }[] };

/** The project's threads, by what they're waiting on. */
function ProjectQueue({ project, items, selected, onOpen, full }: { project: Project; items: QueueItem[]; selected: string | null; onOpen: (id: string) => void; full?: boolean }) {
  const activity = useAgentActivity();
  const seen = useSeen();
  const contextStatus = useContextStatus(items.filter((i) => activity.has(i.id)).map((i) => i.id));
  const [showDone, setShowDone] = useState(false);

  const groups = useMemo(() => {
    const g: Record<string, Group> = {
      you: { id: "you", label: "Needs you", items: [] },
      working: { id: "working", label: "Agents working", items: [] },
      others: { id: "others", label: "Waiting on others", items: [] },
      open: { id: "open", label: "Open", items: [] },
      done: { id: "done", label: "Done", items: [] },
    };
    for (const item of items) {
      const a = activity.get(item.id);
      const ctx = contextStatus.get(item.id);
      const status = taskStatus(a, ctx, unread(a, seen));
      const to =
        item.state === "done" ? "done"
        : status?.kind === "working" || status?.kind === "monitoring" ? "working"
        : status && (status.tone === "warning" || status.tone === "danger" || status.kind === "ready") ? "you"
        : ctx?.some((c) => c.waiting_on === "others") ? "others"
        : "open";
      g[to].items.push({ item, status });
    }
    return Object.values(g).filter((x) => x.items.length);
  }, [items, activity, contextStatus, seen]);

  return (
    <section className={cx("pjq", full && "full")}>
      {!items.length && (
        <p className="subtle pjq-empty">Nothing in {project.name} yet. Start a thread here, pick this project when starting a chat in Work, or ask an agent to file things here.</p>
      )}
      {groups.map((g) => (
        <div key={g.id} className="pjq-group">
          <button className="pjq-group-head" onClick={() => g.id === "done" && setShowDone((v) => !v)} disabled={g.id !== "done"}>
            {g.label} · {g.items.length}
            {g.id === "done" && <Icon name={showDone ? "chevron-down" : "chevron-right"} size={11} />}
          </button>
          {(g.id !== "done" || showDone) &&
            g.items.map(({ item, status }) => (
              <QueueRow key={item.id} item={item} agent={activity.get(item.id)} status={status} selected={item.id === selected} onSelect={() => onOpen(item.id)} compact={!full} />
            ))}
        </div>
      ))}
    </section>
  );
}

/** About or rules: shown as text, edited in place. */
function ProfileField({ project, field, label, placeholder, empty }: { project: Project; field: "about" | "rules"; label: string; placeholder: string; empty: string }) {
  const qc = useQueryClient();
  const value = project[field];
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => setDraft(null), [value]);

  const save = async () => {
    if (draft === null) return;
    try {
      await api.projectSetProfile(project.id, { [field]: draft });
      await qc.invalidateQueries({ queryKey: keys.projects });
      setDraft(null);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <section className="pjt-section">
      <div className="pjt-section-head">
        <span className="label-sm">{label}</span>
        {draft === null && <Button size="sm" variant="ghost" icon="edit" onClick={() => setDraft(value)}>{value ? "Edit" : "Add"}</Button>}
      </div>
      {draft !== null ? (
        <div className="pjt-edit">
          <TextArea autoFocus rows={field === "rules" ? 5 : 3} placeholder={placeholder} value={draft} onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => (e.key === "Enter" && (e.metaKey || e.ctrlKey) ? save() : e.key === "Escape" && setDraft(null))} />
          {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
          <span className="pjt-actions">
            <Button size="sm" variant="ghost" onClick={() => setDraft(null)}>Cancel</Button>
            <Button size="sm" variant="primary" onClick={save}>Save</Button>
          </span>
        </div>
      ) : value ? (
        <div className="pjt-text">{value}</div>
      ) : (
        <p className="subtle pjt-placeholder">{empty}</p>
      )}
    </section>
  );
}

function Repos({ projectId }: { projectId: string }) {
  const qc = useQueryClient();
  const { data: repos = [] } = useProjectRepos(projectId);
  const [adding, setAdding] = useState(false);
  const refresh = () => qc.invalidateQueries({ queryKey: keys.repos(projectId) });

  return (
    <section className="pjt-section">
      <div className="pjt-section-head">
        <span className="label-sm">Repos <span className="subtle">· where agents work</span></span>
        {!adding && <Button size="sm" variant="ghost" icon="plus" onClick={() => setAdding(true)}>Add</Button>}
      </div>
      <div className="pjt-repos">
        {adding && <RepoEditor onCancel={() => setAdding(false)} onSave={async (r) => (await api.projectRepoSave(projectId, r), setAdding(false), refresh())} />}
        {repos.map((r) => <RepoRow key={r.id} repo={r} onChanged={refresh} />)}
        {!repos.length && !adding && <p className="subtle pjt-placeholder">No repos yet. Agents add the ones they work in for this project.</p>}
      </div>
    </section>
  );
}

function RepoRow({ repo, onChanged }: { repo: ProjectRepo; onChanged: () => void }) {
  const [editing, setEditing] = useState(false);
  if (editing)
    return <RepoEditor repo={repo} onCancel={() => setEditing(false)} onSave={async (r) => (await api.projectRepoSave(repo.project_id, r), setEditing(false), onChanged())} />;
  const github = /^[\w.-]+\/[\w.-]+$/.test(repo.repo);
  return (
    <div className="pjt-repo">
      <Icon name="git-branch" size={14} className="subtle" />
      <div className="pjt-repo-main">
        <div className="pjt-repo-top">
          {github ? <a href={`https://github.com/${repo.repo}`} target="_blank" rel="noreferrer">{repo.repo}</a> : <span>{repo.repo}</span>}
          <span className="subtle">{repo.added_by === "agent" ? <><Icon name="bot" size={11} /> agent</> : "you"} · {timeAgo(repo.updated_at)}</span>
        </div>
        {repo.note && <div className="pjt-repo-note">{repo.note}</div>}
        {repo.path && <code className="pjt-repo-path">{repo.path}</code>}
      </div>
      <div className="pj-note-actions">
        <button className="icon-btn" title="Edit" aria-label="Edit repo" onClick={() => setEditing(true)}><Icon name="edit" size={13} /></button>
        <button className="icon-btn" title="Remove" aria-label="Remove repo" onClick={async () => (await api.projectRepoDelete(repo.project_id, repo.id), onChanged())}><Icon name="x" size={13} /></button>
      </div>
    </div>
  );
}

function RepoEditor({ repo, onSave, onCancel }: { repo?: ProjectRepo; onSave: (r: { repo: string; path: string | null; note: string }) => void; onCancel: () => void }) {
  const [name, setName] = useState(repo?.repo ?? "");
  const [path, setPath] = useState(repo?.path ?? "");
  const [note, setNote] = useState(repo?.note ?? "");
  const ok = name.trim();
  const save = () => ok && onSave({ repo: name.trim(), path: path.trim() || null, note: note.trim() });
  return (
    <div className="pj-note editing">
      {!repo && <TextInput autoFocus placeholder="owner/name or URL" value={name} onChange={(e) => setName(e.target.value)} />}
      <TextInput placeholder="What it's for, e.g. the API" value={note} onChange={(e) => setNote(e.target.value)} autoFocus={!!repo} onKeyDown={(e) => e.key === "Enter" && save()} />
      <TextInput placeholder="Local checkout (optional), e.g. ~/src/api" value={path} onChange={(e) => setPath(e.target.value)} onKeyDown={(e) => e.key === "Enter" && save()} />
      <span className="pjt-actions">
        <Button size="sm" variant="ghost" onClick={onCancel}>Cancel</Button>
        <Button size="sm" variant="primary" disabled={!ok} onClick={save}>Save</Button>
      </span>
    </div>
  );
}
