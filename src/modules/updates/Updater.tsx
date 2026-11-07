import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { Icon } from "../../design/icons";
import { cx } from "../../design/components";
import { api, onAgentUpgrade } from "../../lib/api";

const agentName: Record<string, string> = { claude_code: "Claude Code", opencode: "OpenCode" };
/** What fits in the 84px nav; the full name goes in the tooltip. */
const shortName: Record<string, string> = { claude_code: "Claude", opencode: "OpenCode" };

/** "2.1.91 (Claude Code)" → [2, 1, 91]. */
function parts(v: string | null | undefined): number[] | null {
  const m = v?.match(/\d+(?:\.\d+)+/);
  return m ? m[0].split(".").map(Number) : null;
}

export function isOlder(have: string | null | undefined, latest: string | null | undefined) {
  const a = parts(have), b = parts(latest);
  if (!a || !b) return false;
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    if ((a[i] ?? 0) !== (b[i] ?? 0)) return (a[i] ?? 0) < (b[i] ?? 0);
  }
  return false;
}

type Phase = "idle" | "running" | "done" | "failed";

/**
 * Bottom-left of the nav, above the usage meter: a connector with a newer
 * release, a one-tap update and its progress. It runs the same command the
 * agent was installed with (winget, npm, brew or its own `update`), so the
 * install stays managed by whatever put it there. Claude Code is the first;
 * add more connectors as further entries of `updates`.
 */
export function Updater() {
  const qc = useQueryClient();
  const { data: health } = useQuery({ queryKey: ["agent-health"], queryFn: api.agentHealth, staleTime: 30 * 60_000, retry: false });
  const [phase, setPhase] = useState<Phase>("idle");
  const [lines, setLines] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [skipped, setSkipped] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => { if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false); };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  // Once updated, show the tick for a moment and then let the card go.
  useEffect(() => {
    if (phase !== "done") return;
    const t = setTimeout(() => { setPhase("idle"); setOpen(false); }, 5000);
    return () => clearTimeout(t);
  }, [phase]);

  const name = agentName[health?.kind ?? "claude_code"] ?? "Agent";
  const short = shortName[health?.kind ?? "claude_code"] ?? name;
  const have = parts(health?.version)?.join(".");
  const available = !!health?.found && !!health.latest && isOlder(health.version, health.latest) && skipped !== health.latest;
  if (phase === "idle" && !available) return null;

  const update = async () => {
    setPhase("running"); setLines([]); setError(null);
    const off = await onAgentUpgrade((l) => setLines((p) => [...p.slice(-199), l]));
    try {
      await api.agentUpgrade();
      await qc.refetchQueries({ queryKey: ["agent-health"] });
      setPhase("done");
    } catch (e) {
      setError(String((e as { message?: string })?.message ?? e));
      setPhase("failed");
      setOpen(true);
    } finally {
      off();
    }
  };

  const last = lines[lines.length - 1];
  return (
    <div className="updater" ref={ref}>
      <div className={cx("updater-card", phase)} role="status" title={`${name}: you have ${have ?? "?"}, ${health?.latest ?? "?"} is out`}>
        <div className="updater-head">
          <Icon name={phase === "done" ? "check" : phase === "failed" ? "alert" : "arrow-up"} size={11} />
          <span className="updater-name">{short}</span>
          {phase === "idle" && (
            <button className="updater-x" aria-label="Not now" title="Not now" onClick={() => health?.latest && setSkipped(health.latest)}>
              <Icon name="x" size={11} />
            </button>
          )}
        </div>
        {phase === "idle" && (
          <>
            <span className="updater-sub">{parts(health?.latest)?.join(".") ?? health?.latest}</span>
            <button className="updater-go" onClick={() => void update()} title={health?.upgrade ? `Runs ${health.upgrade}` : "Update"}>Update</button>
          </>
        )}
        {phase === "running" && (
          <>
            <div className="updater-bar"><span /></div>
            <button className="updater-sub linky" onClick={() => setOpen(!open)} title={last ?? "Updating…"}>{last ?? "Starting…"}</button>
          </>
        )}
        {phase === "done" && (
          <>
            <span className="updater-sub">{have}</span>
            <span className="updater-sub">Updated</span>
          </>
        )}
        {phase === "failed" && (
          <>
            <button className="updater-sub linky" onClick={() => setOpen(!open)}>Failed</button>
            <button className="updater-go" onClick={() => void update()}>Retry</button>
          </>
        )}
      </div>

      {open && (
        <div className="updater-pop">
          <strong>{health?.upgrade ? <code>{health.upgrade}</code> : `Updating ${name}`}</strong>
          {error && <p className="updater-error">{error}</p>}
          <pre className="updater-log">{lines.length ? lines.join("\n") : "Waiting for output…"}</pre>
        </div>
      )}
    </div>
  );
}
