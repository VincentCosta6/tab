//! Suggested replies: after a turn, a small model guesses what the person
//! will most likely say next. It lands in the transcript as a `suggestion`
//! message; the composer offers it on Tab and Enter sends it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::AgentHub;
use super::small::{self, Ask};
const SYSTEM: &str = "You predict the next message a person will send to their AI coding agent, \
the way an autocomplete would. Write it as a command or instruction TO the agent, in the person's voice. \
Think: what would they ask the agent to do next? Under 12 words. Good: \"run the tests\", \"commit this\", \
\"yes, go ahead\", \"now do the same for the settings page\", \"show me the diff\". \
Bad (never): anything about what the person themselves will do (\"I'll reload and check\", \"Looks good\"), \
anything that praises, thanks or reports back, and anything the agent would say (\"I can\", \"Would you like\"). \
Never write a question, and never repeat or rephrase the agent's question. If the agent asked the person \
to choose, pick the most sensible option and tell the agent to do it (e.g. \"go with the first one\"). \
If no obvious next instruction exists, reply with exactly NONE. Output only the message.";

const BAD_START: &[&str] = &[
    "i'll",
    "i will",
    "i'm",
    "i am",
    "i can",
    "let me",
    "looks good",
    "thanks",
    "thank",
    "great",
    "nice",
    "perfect",
    "awesome",
    "would you",
    "do you",
    "sounds good",
];

pub(super) fn spawn(hub: Arc<AgentHub>, session_id: String) {
    tokio::spawn(async move {
        if let Err(e) = run(&hub, &session_id).await {
            tracing::debug!(session = %session_id, error = %e, "suggesting a reply failed");
        }
    });
}

async fn run(hub: &Arc<AgentHub>, session_id: &str) -> anyhow::Result<()> {
    let cfg = hub.config();
    if cfg.kind.is_none() || !cfg.suggest_replies {
        return Ok(());
    }
    // Only when the turn ended on the agent's own text.
    let messages = hub.store.messages(session_id)?;
    let Some(last) = messages
        .iter()
        .rev()
        .find(|m| !matches!(m.kind.as_str(), "result" | "status" | "suggestion"))
    else {
        return Ok(());
    };
    if last.role != "assistant" || last.kind != "text" {
        return Ok(());
    }
    let text: String = last.content.chars().take(3000).collect();
    let asked: String = messages
        .iter()
        .rev()
        .find(|m| m.role == "user" && m.kind == "text")
        .map(|m| m.content.chars().take(600).collect())
        .unwrap_or_default();
    let prompt = format!(
        "The person last asked the agent:\n{asked}\n\nThe agent replied:\n{text}\n\nWrite the person's next instruction to the agent."
    );
    let reply = small::complete(
        hub,
        &cfg,
        Ask {
            agent: None,
            model: None,
            system: SYSTEM,
            prompt: &prompt,
            schema: None,
            purpose: "suggestion",
            session_id: Some(session_id),
            timeout: Duration::from_secs(45),
        },
    )
    .await?;
    let reply = reply
        .as_str()
        .unwrap_or_default()
        .trim()
        .trim_matches('"')
        .trim()
        .to_string();
    let reply = reply.as_str();
    let lower = reply.to_lowercase();
    if reply.is_empty()
        || lower == "none"
        || reply.contains('?')
        || reply.chars().count() > 120
        || BAD_START.iter().any(|b| lower.starts_with(b))
    {
        return Ok(());
    }
    // The person may have moved on while this ran.
    if hub.is_running(session_id)
        || hub.store.messages(session_id)?.last().map(|m| m.id)
            != Some(messages.last().map_or(0, |m| m.id))
    {
        return Ok(());
    }
    hub.post(session_id, "system", "suggestion", reply, Value::Null);
    Ok(())
}
