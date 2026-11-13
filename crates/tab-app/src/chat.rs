//! A task's chat with its agent: open, send, steer, stop, answer
//! approvals. What the Tauri app's `chat_*` commands did, for any UI.

use serde::Serialize;
use tab_core::EngineEvent;
use tab_core::agents::Decision;
use tab_core::live::SteerCommand;
use tab_core::model::{Approval, NewTask, QueueItem, Session, SessionMessage};

use crate::{Backend, err};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Serialize)]
pub struct Chat {
    pub session: Option<Session>,
    pub messages: Vec<SessionMessage>,
    pub approvals: Vec<Approval>,
    pub running: bool,
}

/// One of a task's chats, for `/resume`.
#[derive(Debug, Clone, Serialize)]
pub struct ChatSummary {
    pub session: Session,
    /// The first thing the person said in it.
    pub first: String,
    /// How many messages it has, the person's and the agent's.
    pub messages: usize,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatStarted {
    pub item: QueueItem,
    pub session: Session,
}

impl Backend {
    /// The task's current chat.
    pub fn chat_open(&self, item_id: &str) -> Result<Chat> {
        let session = self.store.session_for_item(item_id).map_err(err)?;
        self.chat_of(session)
    }

    pub(crate) fn chat_of(&self, session: Option<Session>) -> Result<Chat> {
        let (messages, approvals, running) = match &session {
            // Since it was last compacted, with the last few lines before.
            Some(s) => (
                self.store.messages_current(&s.id, 3).map_err(err)?,
                self.agents.pending_approvals(&s.id),
                self.agents.is_running(&s.id),
            ),
            None => (Vec::new(), Vec::new(), false),
        };
        Ok(Chat {
            session,
            messages,
            approvals,
            running,
        })
    }

    /// `/clear`: a fresh chat on the same task. The old one stays for `/resume`.
    pub fn chat_clear(&self, item_id: &str) -> Result<Chat> {
        if self.agents.config().kind.is_none() {
            return Err("Set up an agent first (Settings → Agent).".into());
        }
        let session = self.agents.session_new_for_item(item_id).map_err(err)?;
        self.chat_of(Some(session))
    }

    /// `/resume`: every chat a task has had, the current one first.
    pub fn chat_list(&self, item_id: &str) -> Result<Vec<ChatSummary>> {
        let sessions = self.store.sessions_for_item(item_id).map_err(err)?;
        let ids: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();
        let mut firsts = self.store.sessions_first_prompt(&ids).map_err(err)?;
        Ok(sessions
            .into_iter()
            .map(|s| {
                let (first, messages) = firsts.remove(&s.id).unwrap_or_default();
                let running = self.agents.is_running(&s.id);
                ChatSummary {
                    session: s,
                    first,
                    messages,
                    running,
                }
            })
            .collect())
    }

    /// `/resume`: makes one of a task's earlier chats its current one again.
    pub fn chat_resume(&self, session_id: &str) -> Result<Chat> {
        let session = self
            .store
            .session_open(session_id, chrono::Utc::now())
            .map_err(err)?
            .ok_or("that chat no longer exists")?;
        let _ = self.events.send(EngineEvent::SessionChanged {
            session: session.clone(),
        });
        self.chat_of(Some(session))
    }

    /// Sends a chat message. Without an item (the "New" chat), the message
    /// first becomes a tab task, and the session hangs off that task.
    pub async fn chat_send(
        &self,
        item_id: Option<String>,
        text: &str,
        project_id: Option<String>,
        attachments: Vec<String>,
    ) -> Result<ChatStarted> {
        let mut text = text.trim().to_string();
        if text.is_empty() && attachments.is_empty() {
            return Err("message is empty".into());
        }
        if text.is_empty() {
            text = "Have a look at this.".into();
        }
        if self.agents.config().kind.is_none() {
            return Err("Set up an agent first (Settings → Agent).".into());
        }
        let item = match item_id {
            Some(id) => self
                .store
                .get(&id)
                .map_err(err)?
                .ok_or("that item no longer exists")?,
            None => {
                let first = text.lines().next().unwrap_or_default();
                let title = task_title(first);
                let task = NewTask {
                    title,
                    body: (text.len() > first.len()).then(|| text.clone()),
                    created_by: "user".into(),
                    project_id,
                    auto_title: true,
                    ..Default::default()
                };
                self.with_engine(async |e| e.create_task(task)).await?
            }
        };
        let session = self.agents.session_for_item(&item.id).map_err(err)?;
        // Another device runs this chat: the message goes there through the hub.
        if !self.agents.runs_here(&session) {
            self.steer_remote(&session.id, &text, &attachments).await?;
            return Ok(ChatStarted { item, session });
        }
        // A turn in flight takes the message (and its files) as steering;
        // otherwise it starts one.
        if self
            .agents
            .steer_with(&session.id, &text, &attachments)
            .map_err(err)?
        {
            return Ok(ChatStarted { item, session });
        }
        self.agents
            .send_with(&session.id, &text, &attachments)
            .map_err(err)?;
        Ok(ChatStarted { item, session })
    }

    /// Sends the person's message to a session another device runs.
    pub async fn steer_remote(
        &self,
        session: &str,
        text: &str,
        attachments: &[String],
    ) -> Result<()> {
        let hub = self
            .work_hub
            .as_ref()
            .ok_or("this chat runs on another device, and the hub isn't running here")?;
        let files = tab_core::live::file_refs(attachments);
        if files.len() != attachments.len() {
            return Err("an attachment isn't in the hub, so the other device can't see it".into());
        }
        hub.steer_remote(
            session,
            &SteerCommand::Message {
                text: text.into(),
                files,
            },
        )
        .await
        .map_err(err)?;
        Ok(())
    }

    fn runs_elsewhere(&self, session_id: &str) -> Result<bool> {
        Ok(self
            .store
            .session_get(session_id)
            .map_err(err)?
            .is_some_and(|s| !self.agents.runs_here(&s)))
    }

    pub async fn chat_stop(&self, session_id: &str) -> Result<()> {
        match (&self.work_hub, self.runs_elsewhere(session_id)?) {
            (Some(h), true) => {
                h.steer_remote(session_id, &SteerCommand::Stop)
                    .await
                    .map_err(err)?;
            }
            _ => self.agents.stop(session_id),
        }
        Ok(())
    }

    /// "Send now": stop the tool a turn is waiting on, so the messages the
    /// person already sent are read right away.
    pub async fn chat_interrupt(&self, session_id: &str) -> Result<()> {
        match (&self.work_hub, self.runs_elsewhere(session_id)?) {
            (Some(h), true) => {
                h.steer_remote(session_id, &SteerCommand::Interrupt)
                    .await
                    .map_err(err)?;
            }
            _ => {
                self.agents.interrupt(session_id);
            }
        }
        Ok(())
    }

    /// Answers an agent's approval prompt or question, here or on the device
    /// that runs the chat.
    pub async fn approval_resolve(
        &self,
        id: String,
        decision: String,
        message: Option<String>,
        answers: Option<serde_json::Value>,
    ) -> Result<bool> {
        let d = Decision::from_parts(&decision, message.clone(), answers.clone());
        if self.agents.resolve_approval(&id, d) {
            return Ok(true);
        }
        let Some(hub) = &self.work_hub else {
            return Ok(false);
        };
        let Some(session) = hub.remote_approval(&id) else {
            return Ok(false);
        };
        hub.steer_remote(
            &session,
            &SteerCommand::Approval {
                id,
                decision,
                message,
                answers,
            },
        )
        .await
        .map_err(err)?;
        Ok(true)
    }
}

/// A new task's title from the first line of the message that starts it.
pub fn task_title(first: &str) -> String {
    if first.chars().count() > 80 {
        first.chars().take(77).collect::<String>() + "…"
    } else {
        first.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::task_title;

    #[test]
    fn long_first_lines_are_cut() {
        assert_eq!(task_title("Fix the build"), "Fix the build");
        let long = "x".repeat(100);
        let t = task_title(&long);
        assert_eq!(t.chars().count(), 78);
        assert!(t.ends_with('…'));
        // Counted in characters, not bytes.
        assert_eq!(task_title(&"é".repeat(80)), "é".repeat(80));
    }
}
