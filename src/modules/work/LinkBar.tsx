/**
 * The bar above a task's composer for the links in its Context a connector
 * reports on (a PR): state icon, reference and title, then autofix and
 * dismiss on the right. Everything shown comes from the connector's
 * `ContextStatus`; autofix is offered only where it says it can report
 * problems (`autofix`).
 */

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import { openLink } from "../../lib/links";
import { keys } from "../../lib/query";
import type { AutofixWatch, ContextStatus } from "../../lib/types";
import { useContextStatus } from "./taskStatus";

export function LinkBar({ itemId }: { itemId: string }) {
  const statuses = useContextStatus([itemId]).get(itemId) ?? [];
  const qc = useQueryClient();
  const { data: watches = [] } = useQuery({
    queryKey: keys.autofix(itemId),
    queryFn: () => api.autofixList(itemId),
    refetchInterval: 30_000,
  });
  const shown = statuses.filter((s) => s.url && s.reference && !watches.some((w) => w.url === s.url && w.dismissed));
  if (!shown.length) return null;

  const update = (p: Promise<AutofixWatch[]>) =>
    p.then((w) => qc.setQueryData(keys.autofix(itemId), w)).catch(() => qc.invalidateQueries({ queryKey: keys.autofix(itemId) }));

  return (
    <div className="link-bars">
      {shown.map((s) => (
        <Bar
          key={s.url}
          status={s}
          watch={watches.find((w) => w.url === s.url)}
          onAutofix={(on) => update(api.autofixSet(itemId, s.url!, on))}
          onDismiss={() => update(api.autofixDismiss(itemId, s.url!, true))}
        />
      ))}
    </div>
  );
}

function Bar({ status: s, watch, onAutofix, onDismiss }: { status: ContextStatus; watch?: AutofixWatch; onAutofix: (on: boolean) => void; onDismiss: () => void }) {
  const on = !!watch?.enabled;
  const autofixTitle = on
    ? `Autofix is on: checks every minute, less often when nothing changes.${watch?.last ? ` ${watch.last}` : ""}${watch?.checked_at ? ` (${timeAgo(watch.checked_at)})` : ""}. Click to turn off.`
    : "Autofix: watch for failing checks and review comments and have the agent fix or answer them";
  return (
    <div className="link-bar">
      <button className="link-bar-main" title={s.detail ?? undefined} onClick={(e) => openLink(s.url!, e)}>
        <span className={cx("link-bar-icon", `tone-${s.tone}`)}><Icon name={s.icon ?? "link"} size={14} /></span>
        <span className="link-bar-ref">{s.reference}</span>
        <span className="link-bar-title">{s.title ?? s.detail}</span>
        <span className={cx("link-bar-state", `tone-${s.tone}`)}>{s.label}</span>
      </button>
      {s.autofix && (
        <button className={cx("link-bar-btn", "link-bar-autofix", on && "on")} aria-pressed={on} aria-label="Autofix" title={autofixTitle} onClick={() => onAutofix(!on)}>
          <Icon name="check" size={14} />
        </button>
      )}
      <button className="link-bar-btn" title="Hide this bar (turns autofix off)" aria-label="Dismiss" onClick={onDismiss}>
        <Icon name="x" size={14} />
      </button>
    </div>
  );
}
