//! Chats on the phone: listing them, starting one on a runner, steering.
//!
//! A phone runs no agents. A chat started here is a job ([`Job::chat`])
//! that a runner takes: tab cloud first, or a computer that's online
//! ([`tab_core::delegate`]), unless the person picked. Once a runner takes
//! it, the chat is an ordinary one on that runner, and what the person
//! types here steers it through the hub.

use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tab_core::delegate::{self, Cloud, Expect, Plan, RunOn};
use tab_core::live::SteerCommand;
use tab_core::model::{Session, SessionMessage};
use tab_core::runner::{ChatStart, Job, JobSpec, JobState, RunnerCaps};
use tauri::State;

use crate::link::{Conn, Phone};

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn conn(phone: &Phone) -> Res<Conn> {
    phone
        .conn()
        .ok_or_else(|| "Not connected to your hub yet.".to_string())
}

/// A runner as the picker shows it.
#[derive(Serialize)]
pub struct RunnerView {
    #[serde(flatten)]
    pub caps: RunnerCaps,
    pub awake: bool,
}

/// A chat started here that no runner has started yet.
#[derive(Serialize)]
pub struct Pending {
    pub job: String,
    pub title: String,
    pub item: String,
    pub created_at: chrono::DateTime<Utc>,
    /// Why it's not running, when no runner will take it any more.
    pub given_up: Option<String>,
}

#[derive(Serialize)]
pub struct Overview {
    pub sessions: Vec<Session>,
    pub pending: Vec<Pending>,
    pub runners: Vec<RunnerView>,
    pub cloud: Cloud,
    /// This phone's device id: chats it started name it.
    pub device: String,
}

#[tauri::command]
pub async fn overview(phone: State<'_, Arc<Phone>>) -> Res<Overview> {
    let c = conn(&phone)?;
    let now = Utc::now();
    let cloud = phone.cloud_runner(&c.hub_id).await;
    let device = c.store.device_id().map_err(err)?;
    let pending =
        c.store
            .jobs_recent(100)
            .map_err(err)?
            .into_iter()
            .filter_map(|j| {
                let chat = j.spec.chat.clone()?;
                let started = matches!(
                    &j.state,
                    JobState::Running {
                        session: Some(_),
                        ..
                    }
                ) || matches!(&j.state, JobState::Done { .. });
                (chat.from == device && !started && !matches!(j.state, JobState::Failed { .. }))
                    .then(|| Pending {
                        given_up: j.given_up(false, now),
                        job: j.id,
                        title: chat.title,
                        item: chat.item,
                        created_at: j.created_at,
                    })
            })
            .collect();
    Ok(Overview {
        sessions: c.store.sessions_recent(100).map_err(err)?,
        pending,
        runners: c
            .store
            .runners()
            .map_err(err)?
            .into_iter()
            .map(|r| RunnerView {
                awake: r.awake(now),
                caps: r,
            })
            .collect(),
        cloud,
        device,
    })
}

#[tauri::command]
pub fn messages(phone: State<'_, Arc<Phone>>, session: String) -> Res<Vec<SessionMessage>> {
    conn(&phone)?.store.messages(&session).map_err(err)
}

#[tauri::command]
pub fn session(phone: State<'_, Arc<Phone>>, session: String) -> Res<Option<Session>> {
    conn(&phone)?.store.session_get(&session).map_err(err)
}

/// What the person asks for in the composer.
#[derive(Deserialize)]
pub struct NewChat {
    pub prompt: String,
    #[serde(default)]
    pub title: String,
    /// The task it's for, if any.
    #[serde(default)]
    pub item: String,
    /// `owner/name`, when it's about a repo.
    #[serde(default)]
    pub repo: String,
    /// Empty: the runner's default.
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub on: RunOn,
}

impl NewChat {
    fn spec(&self, from: &str) -> JobSpec {
        JobSpec {
            prompt: self.prompt.trim().to_string(),
            repo: self.repo.trim().to_string(),
            harness: self.harness.trim().to_string(),
            chat: Some(ChatStart {
                item: self.item.clone(),
                title: if self.title.trim().is_empty() {
                    first_line(&self.prompt)
                } else {
                    self.title.trim().to_string()
                },
                from: from.to_string(),
            }),
            ..Default::default()
        }
    }
}

fn first_line(s: &str) -> String {
    let line = s.trim().lines().next().unwrap_or_default();
    match line.char_indices().nth(60) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    }
}

async fn plan_for(phone: &Phone, c: &Conn, chat: &NewChat) -> Res<(JobSpec, Plan)> {
    let device = c.store.device_id().map_err(err)?;
    let spec = chat.spec(&device);
    let runners = c.store.runners().map_err(err)?;
    let cloud = phone.cloud_runner(&c.hub_id).await;
    let plan = delegate::plan(&chat.on, &spec, &runners, cloud, Utc::now());
    Ok((spec, plan))
}

/// Where a chat would run, before it's sent.
#[tauri::command]
pub async fn plan_chat(phone: State<'_, Arc<Phone>>, chat: NewChat) -> Res<Plan> {
    let c = conn(&phone)?;
    Ok(plan_for(&phone, &c, &chat).await?.1)
}

#[derive(Serialize)]
pub struct Started {
    pub job: String,
    pub plan: Plan,
}

/// Hands a new chat to a runner. Refused when, as things stand, none that
/// may run it ever could, rather than leaving a job nobody takes.
#[tauri::command]
pub async fn start_chat(phone: State<'_, Arc<Phone>>, chat: NewChat) -> Res<Started> {
    if chat.prompt.trim().is_empty() {
        return Err("Say what the agent should do.".into());
    }
    let c = conn(&phone)?;
    let (mut spec, plan) = plan_for(&phone, &c, &chat).await?;
    if let Expect::Never { why } = &plan.expect {
        return Err(why.clone());
    }
    plan.apply(&mut spec);
    let job = Job::chat(&uuid::Uuid::new_v4().to_string(), spec, Utc::now());
    c.store.job_put(&job, Utc::now()).map_err(err)?;
    if plan.wakes_cloud()
        && let Err(e) = phone.wake_cloud(&c.hub_id).await
    {
        // tab cloud also wakes its runner when work waits; say so but go on.
        tracing::warn!(error = %e, "couldn't wake the cloud runner");
    }
    Ok(Started { job: job.id, plan })
}

/// The chat a job started, once a runner has started it.
#[tauri::command]
pub fn job_session(phone: State<'_, Arc<Phone>>, job: String) -> Res<Option<String>> {
    let c = conn(&phone)?;
    let Some(j) = c.store.job_get(&job).map_err(err)? else {
        return Ok(None);
    };
    Ok(match j.state {
        JobState::Running { session, .. } => session,
        // Done: the chat lives on under its key.
        JobState::Done { .. } => c
            .store
            .session_for_key(&format!("chat:{job}"))
            .map_err(err)?
            .map(|s| s.id),
        _ => None,
    })
}

/// Sends a message to a chat, on whichever runner runs it.
#[tauri::command]
pub async fn steer(phone: State<'_, Arc<Phone>>, session: String, text: String) -> Res<()> {
    let c = conn(&phone)?;
    c.hub
        .steer_remote(
            &session,
            &SteerCommand::Message {
                text,
                files: Vec::new(),
            },
        )
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(())
}

/// Stops a chat's turn.
#[tauri::command]
pub async fn stop(phone: State<'_, Arc<Phone>>, session: String) -> Res<()> {
    let c = conn(&phone)?;
    c.hub
        .steer_remote(&session, &SteerCommand::Stop)
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_chat_is_titled_by_its_first_line_unless_named() {
        let mut c = NewChat {
            prompt: "  Fix the flaky test\nIt fails on CI".into(),
            title: String::new(),
            item: "t1".into(),
            repo: " acme/api ".into(),
            harness: String::new(),
            on: RunOn::Auto,
        };
        let s = c.spec("phone-1");
        let chat = s.chat.unwrap();
        assert_eq!(chat.title, "Fix the flaky test");
        assert_eq!(chat.from, "phone-1");
        assert_eq!(chat.item, "t1");
        assert_eq!(s.repo, "acme/api");
        c.title = "Flaky".into();
        assert_eq!(c.spec("p").chat.unwrap().title, "Flaky");
        assert_eq!(first_line(&"x".repeat(80)).chars().count(), 61);
    }

    #[test]
    fn the_composer_defaults_to_auto() {
        let c: NewChat = serde_json::from_str(r#"{"prompt":"hi"}"#).unwrap();
        assert_eq!(c.on, RunOn::Auto);
    }
}
