// What a run in the Runs panel links to: its item, its agent's chat, the
// chat or run that started it, and the step waiting for your OK.

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { JobChat, RunLine } from "../work/WorkflowStatus";

export function RunExtras({ runId }: { runId: string }) {
  const { data: view } = useQuery({ queryKey: ["workflow-run-view", runId], queryFn: () => api.workflowRunView(runId), staleTime: 0 });
  const [chat, setChat] = useState<{ job: string; title: string } | null>(null);
  if (!view) return null;
  return (
    <>
      <RunLine view={view} inPanel onChat={(job, title) => setChat({ job, title })} />
      {chat && <JobChat job={chat.job} title={chat.title} onClose={() => setChat(null)} />}
    </>
  );
}
