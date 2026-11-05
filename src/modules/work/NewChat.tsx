import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { TabMark } from "../../design/brands";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { keys } from "../../lib/query";
import type { Chat } from "../../lib/types";
import { Composer } from "./Chat";
import { cx } from "../../design/components";
import { ProjectPicker, useProjectTint, useRememberedProject } from "./Projects";

/**
 * A fresh chat. The first message becomes a tab task (so it is queued and
 * ranked like everything else) and starts the agent session on that task.
 * Asking about something already in the queue ("work on PAY-12") lets the
 * agent find it and attach it as context.
 */
export function NewChat({ onStarted, project: filtered }: { onStarted: (itemId: string) => void; project?: string | null }) {
  const qc = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The last project picked here is where the next chat goes too.
  const [project, setProject] = useRememberedProject("tab.newChatProject");
  const tint = useProjectTint(project);
  // Looking at one project's queue means new chats go there too.
  useEffect(() => {
    if (filtered) setProject(filtered);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filtered]);

  const send = async (text: string, attachments: string[] = []) => {
    setBusy(true);
    setError(null);
    try {
      const { item, session } = await api.chatSend(null, text, project, attachments);
      qc.setQueryData(keys.item(item.id), item);
      qc.setQueryData<Chat>(keys.chat(item.id), { session, messages: [], approvals: [], running: true });
      qc.invalidateQueries({ queryKey: keys.allItems });
      onStarted(item.id);
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  return (
    <div className={cx("new-chat", tint.className)} style={tint.style}>
      <div className="new-chat-hero">
        <TabMark size={40} />
        <h2>What should tab work on?</h2>
      </div>
      <Composer chat={busy ? { session: null, messages: [], approvals: [], running: true } : undefined} onSend={send} placeholder="Describe a task, paste a link, or ask about your queue…"
        autoFocus
        extra={<span className="composer-project"><ProjectPicker value={project} onChange={setProject} compact /></span>}
      />
      {error && <div className="chat-error"><Icon name="x-circle" size={14} /> {error}</div>}
    </div>
  );
}
