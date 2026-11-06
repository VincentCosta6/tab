import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import type { SessionMessage as ChatMessage } from "../../lib/types";

const authError = /authentication_error|OAuth (access )?token|invalid api key|please run \/login|not logged in|401/i;

const agentName: Record<string, string> = { claude_code: "Claude Code", opencode: "OpenCode" };

function Copy({ text }: { text: string }) {
  const [done, setDone] = useState(false);
  return (
    <button className="agent-notice-cmd" onClick={() => void navigator.clipboard.writeText(text).then(() => (setDone(true), setTimeout(() => setDone(false), 1500)))} title="Copy">
      <code>{text}</code>
      <Icon name={done ? "check" : "copy"} size={12} />
    </button>
  );
}

/**
 * Above the composer: how to sign the agent back in when its last turn failed
 * on authentication. Updates live in the nav (modules/updates/Updater.tsx).
 */
export function AgentNotice({ messages }: { messages?: ChatMessage[] }) {
  const { data: health } = useQuery({ queryKey: ["agent-health"], queryFn: api.agentHealth, staleTime: 30 * 60_000, retry: false });
  const name = agentName[health?.kind ?? "claude_code"] ?? "The agent";
  const talk = messages?.filter((m) => m.kind === "error" || m.kind === "text") ?? [];
  const last = talk[talk.length - 1];

  if (last?.kind === "error" && authError.test(last.content)) {
    const login = health?.kind === "opencode" ? "opencode auth login" : "claude";
    return (
      <div className="agent-notice danger" role="status">
        <Icon name="lock" size={14} />
        <div>
          <strong>{name} needs you to sign in again.</strong>
          <span>
            In a terminal, run <Copy text={login} />
            {health?.kind !== "opencode" && <> and type <code>/login</code></>}. Then send your message again.
          </span>
        </div>
      </div>
    );
  }

  return null;
}
