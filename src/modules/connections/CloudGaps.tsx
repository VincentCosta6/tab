// "Missing in cloud": what a workflow job in tab cloud needed and didn't
// have (a connection not allowed there, a tool that's off there, a sign-in
// to redo), with a one-click fix for each. This is where people add their
// connections to cloud jobs: at the moment one was needed, not during setup.

import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import { keys } from "../../lib/query";
import type { CloudGap, JobGaps, McpCloudState, McpServerInfo, Workflow } from "../../lib/types";
import { useCloudActions, useMcpCloud, useMcpServers } from "./RemoteMcp";

const DISMISSED = "tab.cloud-gaps.dismissed";

function dismissed(): Set<string> {
  try {
    return new Set(JSON.parse(localStorage.getItem(DISMISSED) ?? "[]") as string[]);
  } catch {
    return new Set();
  }
}

export function useCloudGaps() {
  return useQuery({ queryKey: keys.cloudJobGaps, queryFn: api.cloudJobGaps, staleTime: 30_000, refetchInterval: 60_000 });
}

/** Whether a gap is fixed now, so the card can say so instead of offering the fix again. */
function fixed(g: CloudGap, s: McpServerInfo | undefined, st: McpCloudState | undefined): boolean {
  if (!s) return false;
  switch (g.why) {
    case "not_in_cloud":
    case "sign_in":
      return !!s.cloud_jobs && st?.state === "ready";
    case "tool_off":
      return !!s.cloud_jobs && !!g.tool && s.cloud_tools?.[g.tool] === "allow";
    case "only_on_computer":
      return false;
  }
}

/**
 * The card for one item (`itemId`) or one workflow run (`runId`). Shows
 * nothing when no cloud job there missed anything.
 */
export function CloudGaps({ itemId, runId }: { itemId?: string; runId?: string }) {
  const { data: all = [] } = useCloudGaps();
  const [hidden, setHidden] = useState(dismissed);
  const jobs = all.filter((j) => (itemId ? j.item_id === itemId : j.run_id === runId) && !hidden.has(j.job_id));
  if (!jobs.length) return null;
  const dismiss = (id: string) => {
    const next = new Set(hidden).add(id);
    localStorage.setItem(DISMISSED, JSON.stringify([...next].slice(-200)));
    setHidden(next);
  };
  return <>{jobs.map((j) => <JobCard key={j.job_id} job={j} onDismiss={() => dismiss(j.job_id)} />)}</>;
}

function JobCard({ job, onDismiss }: { job: JobGaps; onDismiss: () => void }) {
  const { data: servers = [] } = useMcpServers();
  const { data: states = [] } = useMcpCloud();
  const cloud = useCloudActions();
  const qc = useQueryClient();
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [onComputers, setOnComputers] = useState(false);

  const run = async (key: string, f: () => Promise<unknown>) => {
    setBusy(key);
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(typeof e === "string" ? e : (e as { message?: string }).message ?? String(e));
    } finally {
      setBusy(null);
    }
  };

  /** The job's step runs on the person's computers from now on. */
  const runOnComputers = () => run("computers", async () => {
    const w = (await api.workflowsList()).find((x: Workflow) => x.id === job.workflow_id);
    if (!w) throw "Its workflow was deleted.";
    const step = w.steps.find((s) => s.id === job.step_id);
    if (!step || step.type !== "agent") throw "That step isn't in the workflow any more.";
    step.placement = { type: "computers" };
    await api.workflowSave(w);
    await qc.invalidateQueries({ queryKey: ["workflows"] });
    setOnComputers(true);
  });

  const what = job.workflow_name ? `“${job.workflow_name}”` : "A workflow";
  return (
    <div className="cloud-gaps" role="region" aria-label="Missing in tab cloud">
      <div className="cloud-gaps-head">
        <Icon name="cloud" size={15} />
        <strong>{what} ran in tab cloud without everything it needed</strong>
        {job.finished_at && <span className="subtle">{timeAgo(job.finished_at)}</span>}
        <button className="icon-btn" title="Dismiss" aria-label="Dismiss" onClick={onDismiss}><Icon name="x" size={13} /></button>
      </div>
      {job.missing.map((g) => {
        const s = servers.find((x) => x.id === g.connection);
        const st = states.find((x) => x.id === g.connection);
        const key = `${g.connection}:${g.tool ?? ""}:${g.why}`;
        const done = fixed(g, s, st) || (g.why === "only_on_computer" && onComputers);
        const name = s?.name ?? g.name ?? g.connection;
        let text: React.ReactNode;
        let fix: React.ReactNode = null;
        switch (g.why) {
          case "not_in_cloud":
            text = <span>It needed <strong>{name}</strong>, which isn't allowed in cloud jobs.</span>;
            fix = s && <Button size="sm" variant="primary" disabled={!!busy} onClick={() => void run(key, () => cloud.allow(s.id))}>Allow in cloud</Button>;
            break;
          case "tool_off":
            text = <span>It tried <code>{g.tool}</code> on <strong>{name}</strong>, which is off in cloud jobs.</span>;
            fix = s && g.tool && <Button size="sm" variant="primary" disabled={!!busy} onClick={() => void run(key, () => cloud.setTool(s.id, g.tool!, "allow"))}>Allow {g.tool} in cloud</Button>;
            break;
          case "sign_in":
            text = <span><strong>{name}</strong>'s sign-in for tab cloud stopped working.</span>;
            fix = s && <Button size="sm" variant="primary" disabled={!!busy} onClick={() => void run(key, () => cloud.signIn(s.id))}>Sign in for tab cloud</Button>;
            break;
          case "only_on_computer":
            text = <span>It needed <strong>{name}</strong>, which only works on your computer.</span>;
            fix = <Button size="sm" disabled={!!busy} onClick={() => void runOnComputers()}>Run this step on my computers</Button>;
            break;
        }
        return (
          <div key={key} className="cloud-gap">
            <div className="cloud-gap-text">
              {text}
              {!s && g.why !== "only_on_computer" && <span className="subtle">It's not connected on this computer any more.</span>}
            </div>
            {done ? <span className="done"><Icon name="check" size={12} /> {g.why === "only_on_computer" ? "Runs on your computers next time" : "Fixed for next time"}</span> : fix}
          </div>
        );
      })}
      {error && <p className="chat-error">{error}</p>}
    </div>
  );
}
