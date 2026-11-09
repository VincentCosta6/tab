import { Icon } from "../../design/icons";
import { useAgentUpdate } from "./agentUpdate";
import { useAppUpdate, useInstall } from "./appUpdate";

/** What has a newer release: tab itself and the agent CLI. */
export function useUpdates() {
  const { check } = useAppUpdate();
  const install = useInstall();
  const agent = useAgentUpdate();
  const out: string[] = [];
  if (check.data || install.state.phase !== "idle") out.push(check.data ? `tab ${check.data.version}` : "tab");
  if (agent.behind || agent.upgrade.phase === "running") out.push(`${agent.name} ${agent.latest ?? ""}`.trim());
  return out;
}

/**
 * On the Settings nav button: an info badge while an update is out. Clicking
 * it opens Settings → Updates.
 */
export function UpdatesBadge({ onOpen }: { onOpen: () => void }) {
  const updates = useUpdates();
  if (!updates.length) return null;
  const title = `${updates.join(" and ")} ${updates.length > 1 ? "are" : "is"} ready to install`;
  return (
    <span
      className="nav-badge update"
      role="button"
      aria-label={title}
      title={title}
      onClick={(e) => (e.stopPropagation(), onOpen())}
    >
      <Icon name="info" size={13} />
    </span>
  );
}
