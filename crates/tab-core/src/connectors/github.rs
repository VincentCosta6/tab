//! GitHub notifications → queue items, plus cached PR / issue previews.
//!
//! Signs in through the GitHub CLI (`gh auth token`) by default; a personal
//! access token in settings (classic with `notifications` + `repo`) overrides it. All calls go through the
//! gateway: the notifications poll is revalidated with ETags (a `304` does not
//! count against GitHub's limit), and previews are cached for five minutes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Method;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::config::ConnectorConfig;
use crate::connector::{Connector, ConnectorContext, ConnectorFactory};
use crate::gateway::{Call, Gateway, GatewayError};
use crate::model::{
    Block, ConnectorDescriptor, ConnectorStatus, EntityRef, Field, IncomingEvent, KindInfo, Link,
    QueueItem, SettingField, SettingKind, Tone,
};
use crate::ratelimit::RateLimitSpec;

const API: &str = "https://api.github.com";
const MAX_BODY: usize = 4000;
const PREVIEW_TTL: Duration = Duration::from_secs(300);

pub struct GithubFactory;

impl ConnectorFactory for GithubFactory {
    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor {
            id: "github".into(),
            name: "GitHub".into(),
            icon: "github".into(),
            description: "Review requests, CI failures, mentions and comments from your GitHub notifications.".into(),
            kinds: vec![
                KindInfo::new("github.review_requested", "Review requested", "git-pull-request", Tone::Accent),
                KindInfo::new("github.ci_failed", "CI failed", "x-circle", Tone::Danger),
                KindInfo::new("github.mention", "Mention", "at-sign", Tone::Info),
                KindInfo::new("github.comment", "Comment", "message", Tone::Neutral),
                KindInfo::new("github.assign", "Assigned", "user", Tone::Info),
                KindInfo::new("github.security", "Security alert", "shield", Tone::Danger),
                KindInfo::new("github.activity", "Activity", "github", Tone::Neutral),
            ],
            settings: vec![
                SettingField {
                    key: "token".into(),
                    label: "Personal access token".into(),
                    kind: SettingKind::Secret,
                    help: Some("Optional. Leave empty to use your GitHub CLI login (gh auth login).".into()),
                    required: false,
                },
                SettingField {
                    key: "poll_seconds".into(),
                    label: "Poll interval (seconds)".into(),
                    kind: SettingKind::Number,
                    help: Some("GitHub's requested interval wins if it is longer. Minimum 30.".into()),
                    required: false,
                },
            ],
            available: true,
            rate_limits: vec![
                RateLimitSpec::new("github", "core", 5000, 3600, 40)
                    .note("REST: 5,000 requests per hour per token"),
                RateLimitSpec::new("github", "search", 30, 60, 10)
                    .note("Search: 30 requests per minute"),
            ],
            category: "code".into(),
            features: vec![
                "Review pull requests in a built-in VS Code editor with side-by-side diffs".into(),
                "Comment on lines without leaving tab".into(),
                "Marking an item done marks the notification read".into(),
            ],
            builtin: false,
        }
    }

    fn build(
        &self,
        cfg: &ConnectorConfig,
        gateway: Arc<Gateway>,
    ) -> Option<anyhow::Result<Arc<dyn Connector>>> {
        let token = cfg
            .str("token")
            .map(str::to_string)
            .or_else(crate::gh::token);
        Some(match token {
            Some(token) => Ok(Arc::new(GithubConnector::new(
                gateway,
                &token,
                cfg.u64("poll_seconds"),
            ))),
            None => Err(anyhow::anyhow!(
                "Sign in with the GitHub CLI (gh auth login) or add a personal access token."
            )),
        })
    }
}

pub struct GithubConnector {
    gateway: Arc<Gateway>,
    min_poll: Duration,
    /// thread id → updated_at we last emitted, so comment bodies are only fetched for new activity.
    seen: Mutex<HashMap<String, DateTime<Utc>>>,
}

#[derive(Deserialize)]
struct Notification {
    id: String,
    reason: String,
    updated_at: DateTime<Utc>,
    subject: Subject,
    repository: Repository,
}

#[derive(Deserialize)]
struct Subject {
    title: String,
    url: Option<String>,
    latest_comment_url: Option<String>,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct Repository {
    full_name: String,
    html_url: String,
}

#[derive(Deserialize)]
struct Comment {
    body: Option<String>,
    user: Option<User>,
}

#[derive(Deserialize)]
struct User {
    login: String,
}

#[derive(Deserialize)]
struct PullOrIssue {
    title: String,
    body: Option<String>,
    state: String,
    user: Option<User>,
    html_url: String,
    #[serde(default)]
    draft: Option<bool>,
    #[serde(default)]
    merged: Option<bool>,
    #[serde(default)]
    additions: Option<u64>,
    #[serde(default)]
    deletions: Option<u64>,
    #[serde(default)]
    changed_files: Option<u64>,
    #[serde(default)]
    commits: Option<u64>,
    #[serde(default)]
    comments: Option<u64>,
    #[serde(default)]
    head: Option<Ref>,
    #[serde(default)]
    base: Option<Ref>,
    #[serde(default)]
    labels: Vec<Label>,
}

#[derive(Deserialize)]
struct Ref {
    #[serde(rename = "ref")]
    name: String,
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

/// Maps GitHub's notification `reason` to a tab kind.
pub fn kind_for(reason: &str, subject_type: &str) -> &'static str {
    match (reason, subject_type) {
        ("review_requested", _) => "github.review_requested",
        ("mention" | "team_mention", _) => "github.mention",
        ("ci_activity", _) | (_, "CheckSuite") => "github.ci_failed",
        ("assign", _) => "github.assign",
        ("security_alert", _) => "github.security",
        ("comment" | "author" | "state_change" | "subscribed" | "manual", _) => "github.comment",
        _ => "github.activity",
    }
}

/// `https://api.github.com/repos/o/r/pulls/12` → `https://github.com/o/r/pull/12`.
pub fn html_url(api_url: Option<&str>, repo_html: &str) -> String {
    match api_url {
        Some(u) if u.starts_with("https://api.github.com/repos/") => u
            .replacen("https://api.github.com/repos/", "https://github.com/", 1)
            .replacen("/pulls/", "/pull/", 1)
            .replacen("/commits/", "/commit/", 1),
        _ => format!("{repo_html}/actions"),
    }
}

fn number_from(api_url: Option<&str>) -> Option<u64> {
    api_url?.rsplit('/').next()?.parse().ok()
}

fn truncate(s: String, n: usize) -> String {
    if s.chars().count() > n {
        s.chars().take(n).collect::<String>() + "…"
    } else {
        s
    }
}

impl GithubConnector {
    pub fn new(gateway: Arc<Gateway>, token: &str, poll_seconds: Option<u64>) -> Self {
        let mut h = HeaderMap::new();
        h.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        h.insert(
            HeaderName::from_static("x-github-api-version"),
            HeaderValue::from_static("2022-11-28"),
        );
        if let Ok(mut v) = HeaderValue::from_str(&format!("Bearer {}", token.trim())) {
            v.set_sensitive(true);
            h.insert(AUTHORIZATION, v);
        }
        gateway.set_provider_headers("github", h);
        Self {
            gateway,
            min_poll: Duration::from_secs(poll_seconds.unwrap_or(60).max(30)),
            seen: Mutex::new(HashMap::new()),
        }
    }

    async fn latest_comment(&self, n: &Notification) -> Option<Comment> {
        let url = n
            .subject
            .latest_comment_url
            .as_deref()
            .filter(|u| Some(*u) != n.subject.url.as_deref())?;
        // A comment URL's content rarely changes; cache it for a day.
        let call =
            Call::get("github", "github", "latest comment", url).cache(Duration::from_secs(86_400));
        self.gateway.send(call).await.ok()?.json().ok()
    }

    async fn to_event(&self, n: &Notification) -> IncomingEvent {
        let comment = self.latest_comment(n).await;
        let actor = comment
            .as_ref()
            .and_then(|c| c.user.as_ref())
            .map(|u| u.login.clone());
        let body = comment.and_then(|c| c.body).map(|b| truncate(b, MAX_BODY));
        let repo = &n.repository.full_name;
        let number = number_from(n.subject.url.as_deref());

        let mut blocks = vec![Block::Badges {
            items: vec![
                Field::new("reason", n.reason.replace('_', " ")),
                Field::new("type", n.subject.kind.clone()),
            ],
        }];
        if let Some(b) = &body {
            blocks.push(Block::Quote {
                text: b.clone(),
                author: actor.clone(),
            });
        }

        let mut entities = vec![EntityRef::new(
            "repo",
            format!("github:{repo}"),
            repo.clone(),
            "in",
        )];
        if let Some(num) = number {
            let kind = if n.subject.kind == "PullRequest" {
                "pull_request"
            } else {
                "issue"
            };
            entities.push(EntityRef::new(
                kind,
                format!("github:{repo}#{num}"),
                format!("{repo}#{num}"),
                "about",
            ));
        }
        if let Some(login) = &actor {
            entities.push(EntityRef::new(
                "person",
                format!("github:{login}"),
                login.clone(),
                "from",
            ));
        }

        IncomingEvent {
            source: "github".into(),
            external_id: n.id.clone(),
            kind: kind_for(&n.reason, &n.subject.kind).into(),
            title: n.subject.title.clone(),
            body,
            url: Some(html_url(n.subject.url.as_deref(), &n.repository.html_url)),
            actor,
            context: Some(repo.clone()),
            occurred_at: n.updated_at,
            blocks,
            entities,
            metadata: json!({
                "repo": repo,
                "number": number,
                "subject_type": n.subject.kind,
                "reason": n.reason,
                "api_url": n.subject.url,
            }),
        }
    }

    /// One poll. Returns how long to wait before the next one.
    async fn poll(&self, ctx: &ConnectorContext) -> Result<Duration, GatewayError> {
        // TTL zero: always revalidate, but with the ETag, so unchanged polls are free.
        let call = Call::get(
            "github",
            "github",
            "poll notifications",
            format!("{API}/notifications"),
        )
        .query("per_page", "50")
        .cache(Duration::ZERO);
        let resp = self.gateway.send(call).await?;
        let wait = resp
            .headers
            .get("x-poll-interval")
            .and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(self.min_poll)
            .max(self.min_poll);
        if resp.unchanged() {
            return Ok(wait);
        }
        let notifications: Vec<Notification> = resp.json()?;
        for n in &notifications {
            let is_new = {
                let mut seen = self.seen.lock().await;
                let changed = seen.get(&n.id) != Some(&n.updated_at);
                seen.insert(n.id.clone(), n.updated_at);
                changed
            };
            if is_new {
                ctx.emit(self.to_event(n).await).await?;
            }
        }
        Ok(wait)
    }
}

#[async_trait]
impl Connector for GithubConnector {
    fn id(&self) -> &str {
        "github"
    }

    async fn run(&self, ctx: ConnectorContext) -> anyhow::Result<()> {
        let mut backoff = Duration::from_secs(15);
        while !ctx.is_cancelled() {
            let wait = match self.poll(&ctx).await {
                Ok(wait) => {
                    ctx.synced().await;
                    backoff = Duration::from_secs(15);
                    wait
                }
                Err(GatewayError::Http { status: 401, .. }) => {
                    anyhow::bail!("GitHub rejected the token (401). Check it and reconnect.")
                }
                Err(GatewayError::RateLimited { until, .. }) => {
                    ctx.set_status(ConnectorStatus::RateLimited { until }).await;
                    (until - Utc::now())
                        .to_std()
                        .unwrap_or(self.min_poll)
                        .max(self.min_poll)
                }
                Err(e) => {
                    ctx.set_status(ConnectorStatus::Error {
                        message: e.to_string(),
                    })
                    .await;
                    backoff = (backoff * 2).min(Duration::from_secs(600));
                    backoff
                }
            };
            if !ctx.sleep(wait).await {
                break;
            }
        }
        Ok(())
    }

    async fn on_done(&self, item: &QueueItem) -> anyhow::Result<()> {
        let url = format!("{API}/notifications/threads/{}", item.event.external_id);
        self.gateway
            .send(Call::new(
                Method::PATCH,
                "github",
                "github",
                "mark notification read",
                url,
            ))
            .await?;
        Ok(())
    }

    async fn preview(&self, item: &QueueItem) -> anyhow::Result<Vec<Block>> {
        let Some(api_url) = item.event.metadata["api_url"].as_str() else {
            return Ok(Vec::new());
        };
        let call = Call::get("github", "github", "item preview", api_url).cache(PREVIEW_TTL);
        let p: PullOrIssue = self.gateway.send(call).await?.json()?;

        let state = match (p.merged, p.draft, p.state.as_str()) {
            (Some(true), _, _) => Field::new("State", "Merged").tone(Tone::Accent),
            (_, Some(true), _) => Field::new("State", "Draft"),
            (_, _, "open") => Field::new("State", "Open").tone(Tone::Success),
            (_, _, s) => Field::new("State", s.to_string()).tone(Tone::Danger),
        };
        let mut blocks = Vec::new();
        if let (Some(a), Some(d)) = (p.additions, p.deletions) {
            let mut stats = vec![
                Field::new("added", format!("+{a}")).tone(Tone::Success),
                Field::new("removed", format!("−{d}")).tone(Tone::Danger),
            ];
            stats.extend(p.changed_files.map(|f| Field::new("files", f.to_string())));
            stats.extend(p.commits.map(|c| Field::new("commits", c.to_string())));
            blocks.push(Block::Stats { items: stats });
        }
        let mut fields = vec![state];
        fields.extend(p.user.map(|u| Field::new("Author", u.login)));
        if let (Some(h), Some(b)) = (&p.head, &p.base) {
            fields.push(Field::new("Branch", format!("{} → {}", h.name, b.name)));
        }
        fields.extend(p.comments.map(|c| Field::new("Comments", c.to_string())));
        blocks.push(Block::Fields { items: fields });
        if !p.labels.is_empty() {
            blocks.push(Block::Badges {
                items: p
                    .labels
                    .into_iter()
                    .map(|l| Field::new("label", l.name))
                    .collect(),
            });
        }
        if let Some(body) = p.body.filter(|b| !b.trim().is_empty()) {
            blocks.push(Block::Text {
                text: truncate(body, 3000),
                markdown: true,
            });
        }
        blocks.push(Block::Links {
            items: vec![Link {
                label: format!("Open “{}” on GitHub", p.title),
                url: p.html_url,
            }],
        });
        Ok(blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_reasons_and_urls() {
        assert_eq!(
            kind_for("review_requested", "PullRequest"),
            "github.review_requested"
        );
        assert_eq!(kind_for("ci_activity", "CheckSuite"), "github.ci_failed");
        assert_eq!(kind_for("team_mention", "Issue"), "github.mention");
        assert_eq!(
            html_url(
                Some("https://api.github.com/repos/acme/api/pulls/12"),
                "https://github.com/acme/api"
            ),
            "https://github.com/acme/api/pull/12"
        );
        assert_eq!(
            html_url(None, "https://github.com/acme/api"),
            "https://github.com/acme/api/actions"
        );
        assert_eq!(
            number_from(Some("https://api.github.com/repos/acme/api/issues/7")),
            Some(7)
        );
    }
}
