import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { useProjectAnchors, useProjects } from "../../lib/hooks";
import { keys } from "../../lib/query";
import type { EntityRef, ProjectAnchor, QueueItem } from "../../lib/types";

/** How a container reads: "Sentry project tab-desktop", "repo acme/web". */
export function containerName(c: Pick<EntityRef, "kind" | "label">): string {
  const [service, what] = c.kind.includes(".") ? c.kind.split(".", 2) : ["", c.kind];
  const head = service ? `${service[0].toUpperCase()}${service.slice(1)} ${what.replace(/_/g, " ")}` : what.replace(/_/g, " ");
  return `${head} ${c.label}`;
}

/**
 * After you file an item in a project by hand: "Always send <container>
 * here?". Always makes an anchor, so the rest of that container follows;
 * Just this leaves it at the one item.
 */
export function RouteOffer({ projectId, container, onDone }: { projectId: string; container: EntityRef; onDone: () => void }) {
  const qc = useQueryClient();
  const { data: projects = [] } = useProjects();
  const project = projects.find((p) => p.id === projectId);
  const [busy, setBusy] = useState(false);
  if (!project) return null;
  const always = async () => {
    setBusy(true);
    try {
      await api.anchorSave(projectId, container, "always", "learned");
      qc.invalidateQueries({ queryKey: keys.allItems });
      qc.invalidateQueries({ queryKey: keys.anchors(projectId) });
    } finally {
      onDone();
    }
  };
  return (
    <div className="route-offer" role="status">
      <Icon name="git-branch" size={13} />
      <span className="route-offer-text">
        Always send <b>{containerName(container)}</b> to {project.name}?
      </span>
      <Button size="sm" variant="primary" disabled={busy} onClick={always}>Always</Button>
      <Button size="sm" variant="ghost" disabled={busy} onClick={onDone}>Just this</Button>
    </div>
  );
}

/** On a queue row: the project tab thinks it belongs in. One click files it there. */
export function SuggestionChip({ item }: { item: QueueItem }) {
  const qc = useQueryClient();
  const { data: projects = [] } = useProjects();
  const s = item.suggestion;
  const p = s && !item.project_id ? projects.find((x) => x.id === s.project_id) : undefined;
  if (!s || !p) return null;
  const accept = async (e: React.MouseEvent) => {
    e.stopPropagation();
    const next = await api.itemSetProject(item.id, p.id);
    if (next) qc.setQueryData(keys.item(item.id), next);
    qc.invalidateQueries({ queryKey: keys.allItems });
  };
  return (
    <button className="route-chip" title={`Move to ${p.name}: ${s.reason}`} onClick={accept}>
      <i style={{ background: p.color }} />→ {p.name}?
    </button>
  );
}

const SOURCE: Record<string, string> = {
  person: "you",
  learned: "you, from a move",
  scan: "found in the checkout",
  match: "named alike",
  undo: "you undid a move",
};

/**
 * What routes items to a project: the containers it always gets (its repos
 * count too), the ones tab proposed from the project's checkouts and
 * names, and the ones it never gets.
 */
export function ProjectRouting({ projectId }: { projectId: string }) {
  const qc = useQueryClient();
  const { data: anchors = [] } = useProjectAnchors(projectId);
  const refresh = () => {
    qc.invalidateQueries({ queryKey: keys.anchors(projectId) });
    qc.invalidateQueries({ queryKey: keys.allItems });
    qc.invalidateQueries({ queryKey: keys.projectItems(projectId) });
  };
  const set = async (a: ProjectAnchor, state: ProjectAnchor["state"]) => {
    await api.anchorSave(projectId, { kind: a.kind, key: a.key, label: a.label, rel: "container" }, state);
    refresh();
  };
  const remove = async (a: ProjectAnchor) => (await api.anchorDelete(a.id), refresh());
  const proposed = anchors.filter((a) => a.state === "proposed");
  const confirmAll = async () => {
    for (const a of proposed) await api.anchorSave(projectId, { kind: a.kind, key: a.key, label: a.label, rel: "container" }, "always");
    refresh();
  };

  return (
    <section className="pjt-section">
      <div className="pjt-section-head">
        <span className="label-sm">Routing <span className="subtle">· where its items come from</span></span>
        {proposed.length > 1 && <Button size="sm" variant="ghost" icon="check" onClick={confirmAll}>Confirm all {proposed.length}</Button>}
      </div>
      <div className="pjt-repos">
        {anchors.map((a) => (
          <div key={a.id} className={cx("pjt-repo route-anchor", a.state)}>
            <Icon name={a.state === "never" ? "x" : a.state === "proposed" ? "sparkles" : "git-branch"} size={14} className="subtle" />
            <div className="pjt-repo-main">
              <div className="pjt-repo-top">
                <span>{containerName(a)}</span>
                <span className="subtle">{a.state === "always" ? "always here" : a.state === "never" ? "never here" : "proposed"} · {SOURCE[a.source] ?? a.source}</span>
              </div>
              {a.reason && <div className="pjt-repo-note">{a.reason}</div>}
            </div>
            <div className="pj-note-actions">
              {a.state === "proposed" ? (
                <>
                  <Button size="sm" variant="primary" onClick={() => set(a, "always")}>Confirm</Button>
                  <Button size="sm" variant="ghost" onClick={() => set(a, "never")}>Dismiss</Button>
                </>
              ) : (
                <button className="icon-btn" title="Remove" aria-label={`Stop routing ${a.label}`} onClick={() => remove(a)}><Icon name="x" size={13} /></button>
              )}
            </div>
          </div>
        ))}
        {!anchors.length && (
          <p className="subtle pjt-placeholder">
            Items from this project's repos come here on their own. Move an item from Sentry, Linear or Notion here and tab offers to send the rest of its project or team too.
          </p>
        )}
      </div>
    </section>
  );
}
