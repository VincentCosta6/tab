import { useState } from "react";
import { Button } from "../../design/components";
import { Icon } from "../../design/icons";
import type { SessionMessage as ChatMessage } from "../../lib/types";
import { useAgentUpdate } from "../updates/agentUpdate";

const authError = /authentication_error|OAuth (access )?token|invalid api key|please run \/login|not logged in|401/i;

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

/** Releases the person said "Not now" to while this app runs. */
const skipped = new Set<string>();

/**
 * Above the composer: how to sign the agent back in when its last turn failed
 * on authentication, and, when starting a chat, an offer to update the agent
 * first if a newer release is out. The nav's Settings badge says so too.
 */
export function AgentNotice({ messages, starting }: { messages?: ChatMessage[]; starting?: boolean }) {
  const { health, behind, name, have, latest, upgrade, update } = useAgentUpdate();
  const [, redraw] = useState(0);
  const kind = health.data?.kind ?? "claude_code";
  const talk = messages?.filter((m) => m.kind === "error" || m.kind === "text") ?? [];
  const last = talk[talk.length - 1];

  if (last?.kind === "error" && authError.test(last.content)) {
    const [login, then] = signIn[kind] ?? signIn.claude_code;
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

  const offer = starting && ((behind && latest && !skipped.has(latest)) || upgrade.phase !== "idle");
  if (!offer) return null;
  const dismiss = () => (latest && skipped.add(latest), redraw((n) => n + 1));
  const lines = upgrade.phase === "running" || upgrade.phase === "failed" ? upgrade.lines : [];
  return (
    <div className={upgrade.phase === "failed" ? "agent-notice danger" : "agent-notice"} role="status">
      <Icon name={upgrade.phase === "done" ? "check-circle" : upgrade.phase === "failed" ? "alert" : "info"} size={14} />
      <div>
        {upgrade.phase === "idle" && (
          <>
            <strong>{name} {latest} is out.</strong>
            <span>You have {have}. Update it before this chat starts?</span>
            <span className="agent-notice-actions">
              <Button size="sm" variant="primary" onClick={() => void update()}>Update {name}</Button>
              <Button size="sm" variant="ghost" onClick={dismiss}>Not now</Button>
            </span>
          </>
        )}
        {upgrade.phase === "running" && (
          <>
            <strong>Updating {name}…</strong>
            <span title={lines.join("\n")}>{lines[lines.length - 1] ?? "Starting…"}</span>
          </>
        )}
        {upgrade.phase === "done" && <strong>{name} is up to date{have ? ` (${have})` : ""}.</strong>}
        {upgrade.phase === "failed" && (
          <>
            <strong>Couldn't update {name}.</strong>
            <span>{upgrade.error}</span>
            <span className="agent-notice-actions">
              <Button size="sm" onClick={() => void update()}>Retry</Button>
              <Button size="sm" variant="ghost" onClick={dismiss}>Not now</Button>
            </span>
          </>
        )}
      </div>
    </div>
  );
}
