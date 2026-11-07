// Projects: everything tab knows about each project in one place. What it is
// and the rules to work by, the repos its work happens in, the notes agents
// left, and its recent work. Agents read the same things with tab_project and
// keep them current, so what's here is what the next chat starts from.

import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button, EmptyState, TextArea, TextInput, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import { useProjectItems, useProjectRepos, useProjects } from "../../lib/hooks";
import { openItem } from "../../lib/links";
import { keys } from "../../lib/query";
import type { Project, ProjectRepo } from "../../lib/types";
import { ProjectEditor, ProjectMark, ProjectNotes, useRememberedProject } from "../work/Projects";
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
          {projects.map((p) => (
            <button key={p.id} className={cx("pjt-list-item", p.id === project?.id && "active")} onClick={() => setSelected(p.id)}>
              <ProjectMark project={p} size={22} />
              <span className="pjt-list-text">
                <span className="pjt-list-name">{p.name}</span>
                {p.about && <span className="subtle">{p.about}</span>}
              </span>
            </button>
          ))}
          {!projects.length && <p className="subtle pjt-empty-note">No projects yet.</p>}
        </div>
      </aside>
      <main className="pjt-main">
        {project ? (
          <ProjectPage key={project.id} project={project} />
        ) : (
          <EmptyState icon="folder" title="No projects yet">
            Group work into projects. Agents in a project learn what it is, its rules and its repos, and keep them current here.
          </EmptyState>
        )}
      </main>
      {creating && <ProjectEditor project={null} onClose={() => setCreating(false)} onSaved={(p) => setSelected(p.id)} />}
    </div>
  );
}

function ProjectPage({ project }: { project: Project }) {
  const [editing, setEditing] = useState(false);
  return (
    <div className="pjt-page">
      <header className="pjt-head">
        <ProjectMark project={project} size={36} />
        <h1>{project.name}</h1>
        <Button size="sm" variant="ghost" icon="edit" onClick={() => setEditing(true)}>Edit</Button>
      </header>
      <p className="subtle pjt-lede">Agents in this project read all of this before they start, and keep it up to date as they learn.</p>
      <ProfileField project={project} field="about" label="About" placeholder="What is this project, and what does it do?" empty="Nothing yet. An agent fills this in once it learns what the project is, or write it yourself." />
      <ProfileField project={project} field="rules" label="Rules" placeholder={"How should agents work here? One per line, e.g.\n- Open PRs against main\n- Never touch the billing schema without asking"} empty="No rules yet. Add conventions agents should follow, like branches, reviewers or what not to touch." />
      <Repos projectId={project.id} />
      <div className="pjt-section"><ProjectNotes projectId={project.id} /></div>
      <RecentWork projectId={project.id} />
      {editing && <ProjectEditor project={project} onClose={() => setEditing(false)} />}
    </div>
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

function RecentWork({ projectId }: { projectId: string }) {
  const { data: items = [] } = useProjectItems(projectId);
  return (
    <section className="pjt-section">
      <div className="pjt-section-head"><span className="label-sm">Recent work</span></div>
      <div className="pjt-items">
        {items.slice(0, 20).map((i) => (
          <button key={i.id} className={cx("pjt-item", i.state === "done" && "done")} onClick={() => openItem(i.id)}>
            <Icon name={i.state === "done" ? "check" : "circle-dot"} size={12} className="subtle" />
            <span className="pjt-item-title">{i.title}</span>
            <span className="subtle">{timeAgo(i.occurred_at)}</span>
          </button>
        ))}
        {!items.length && <p className="subtle pjt-placeholder">Nothing in this project yet. Pick it when starting a chat, or ask an agent to file things here.</p>}
      </div>
    </section>
  );
}
