import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import type { SessionMessage as ChatMessage } from "../../lib/types";

const authError = /authentication_error|OAuth (access )?token|invalid api key|please run \/login|not logged in|401/i;

const agentName: Record<string, string> = { claude_code: "Claude Code", opencode: "OpenCode", codex: "Codex", pi: "Pi" };
/** How each harness signs in again: a command, then what to type in it, if anything. */
const signIn: Record<string, [string, string | null]> = {
  claude_code: ["claude", "/login"],
  opencode: ["opencode auth login", null],
  codex: ["codex login", null],
  pi: ["pi", "/login"],
};

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
    const [login, then] = signIn[health?.kind ?? "claude_code"] ?? signIn.claude_code;
    return (
      <div className="agent-notice danger" role="status">
        <Icon name="lock" size={14} />
        <div>
          <strong>{name} needs you to sign in again.</strong>
          <span>
            In a terminal, run <Copy text={login} />
            {then && <> and type <code>{then}</code></>}. Then send your message again.
          </span>
        </div>
      </div>
    );
  }

  return null;
}
