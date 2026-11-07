// A chat's live state as a small pill: working, needs you and monitoring
// animate so live sessions stand out; idle and failed stay still.

import { cx } from "../../design/components";
import type { SessionStatus, Tone } from "../../lib/types";

export const sessionLook: Record<SessionStatus, { label: string; tone: Tone }> = {
  working: { label: "Working", tone: "info" },
  needs_input: { label: "Needs you", tone: "warning" },
  monitoring: { label: "Monitoring", tone: "accent" },
  failed: { label: "Failed", tone: "danger" },
  idle: { label: "Idle", tone: "neutral" },
};

export function SessionPill({ status, title, className }: { status: SessionStatus; title?: string; className?: string }) {
  const s = sessionLook[status];
  return (
    <span className={cx("session-pill", `is-${status}`, `tone-${s.tone}`, className)} title={title}>
      <i className="session-pulse" aria-hidden="true" />
      {s.label}
    </span>
  );
}
