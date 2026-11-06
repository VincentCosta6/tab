import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import type { SessionMessage as ChatMessage } from "../../lib/types";

const authError = /authentication_error|OAuth (access )?token|invalid api key|please run \/login|not logged in|401/i;

const agentName: Record<string, string> = { claude_code: "Claude Code", opencode: "OpenCode" };

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

function Copy({ text }: { text: string }) {
  const [done, setDone] = useState(false);
  return (
    <button className="agent-notice-cmd" onClick={() => void navigator.clipboard.writeText(text).then(() => (setDone(true), setTimeout(() => setDone(false), 1500)))} title="Copy">
      <code>{text}</code>
      <Icon name={done ? "check" : "copy"} size={12} />
    </button>
  );
}

const dismissKey = (v: string) => `tab.agentUpdateDismissed.${v}`;
function dismissed(v: string) {
  try { return localStorage.getItem(dismissKey(v)) === "1"; } catch { return false; }
}

/**
 * Above the composer: how to sign the agent back in when its last turn failed
 * on authentication, or how to update it when a newer release is out.
 */
export function AgentNotice({ messages }: { messages?: ChatMessage[] }) {
  const { data: health } = useQuery({ queryKey: ["agent-health"], queryFn: api.agentHealth, staleTime: 30 * 60_000, retry: false });
  const [hidden, setHidden] = useState(false);
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

  if (!hidden && health?.found && health.latest && isOlder(health.version, health.latest) && !dismissed(health.latest)) {
    return (
      <div className="agent-notice" role="status">
        <Icon name="arrow-up" size={14} />
        <div>
          <strong>{name} {health.latest} is out.</strong>
          <span>You have {parts(health.version)?.join(".")}. To update, run {health.upgrade ? <Copy text={health.upgrade} /> : "its updater"} in a terminal.</span>
        </div>
        <button className="agent-notice-x" aria-label="Dismiss" onClick={() => { try { localStorage.setItem(dismissKey(health.latest!), "1"); } catch { /* private window */ } setHidden(true); }}>
          <Icon name="x" size={12} />
        </button>
      </div>
    );
  }
  return null;
}
