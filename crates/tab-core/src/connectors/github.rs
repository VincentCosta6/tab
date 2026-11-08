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
    ActionField, ActionInfo, Block, ConnectorDescriptor, ConnectorStatus, ContextSection,
    DoneGuard, Effect, EntityRef, Field, FieldType, GuardLevel, IncomingEvent, ItemContext,
    KindInfo, Link, QueueItem, SettingField, SettingKind, Tone,
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
                KindInfo::new("github.ci_passed", "CI passed", "check-circle", Tone::Success),
                KindInfo::new("github.ci", "CI", "circle-dot", Tone::Neutral),
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
            actions: actions(),
            clis: clis(),
        }
    }

    fn forget_pages(&self, pages: &[String]) {
        for p in pages {
            if let Some((repo, number)) = crate::sandbox::broker::parse_github_url(p) {
                super::github_review::forget_links(&repo, number);
            }
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

fn repo_field() -> ActionField {
    ActionField::new("repo", "Repository", FieldType::Text)
        .required()
        .help("owner/name")
        .default("{{item.metadata.repo}}")
}

fn number_field() -> ActionField {
    ActionField::new("number", "PR or issue number", FieldType::Number)
        .required()
        .default("{{item.metadata.number}}")
}

/// The CLIs this connection signs in. tab runs them for agents with the
/// connection's token (see `sandbox::broker`). Reads go straight through,
/// uncached, since an agent asking usually wants what's there now; changes
/// tell the app what to refresh.
pub fn clis() -> Vec<tab_domain::CliInfo> {
    use tab_domain::{CliAuth, CliEffect as E, CliInfo, CliRule as R};
    let read = "list|ls|view|status|diff|checks|watch|show";
    // A push or a repo change can move any pull request in the repo, so
    // their page is the repo's, which refreshes all of them.
    let pages = |extra: &[(&str, &str)]| {
        [
            ("repo", "https://github.com/{repo}"),
            ("branch", "https://github.com/{repo}"),
        ]
        .iter()
        .chain(extra)
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<std::collections::BTreeMap<_, _>>()
    };
    let gh = CliInfo {
        name: "gh".into(),
        description: "The GitHub CLI, signed in as this connection.".into(),
        auth: CliAuth::Env {
            var: "GH_TOKEN".into(),
        },
        value_flags: [
            "-R", "--repo", "--hostname", "-X", "--method", "-H", "--header", "-f", "-F",
            "--field", "--raw-field", "--input", "-q", "--jq", "-t", "--template", "--title",
            "-b", "--body", "--body-file", "-B", "--base", "--head", "-l", "--label", "-a",
            "--assignee", "-m", "--milestone", "-r", "--reviewer", "-L", "--limit", "-s",
            "--state", "--json", "-e", "--env",
        ]
        .map(String::from)
        .to_vec(),
        rules: vec![
            R::new("", E::Read),
            R::new("help|version|completion|status|search|browse", E::Read),
            R::new("auth status", E::Read),
            R::new("auth", E::Blocked)
                .reason("tab signs gh in with your GitHub connection, so `gh auth` isn't available in chats"),
            R::new("extension|extensions|ext", E::Blocked)
                .reason("gh extensions would run with your GitHub token; install and run them yourself"),
            R::new("config|alias get|list|ls", E::Read),
            R::new("config|alias", E::Blocked)
                .reason("gh's settings are yours; change them outside the chat"),
            R::new("api", E::ByMethod),
            R::new(&format!("* {read}"), E::Read),
            R::new("secret|variable|ssh-key|gpg-key", E::Destructive),
            R::new("repo delete|archive|unarchive|rename", E::Destructive).resource("repo"),
            R::new("repo edit", E::Destructive).flags(&["--visibility"]).resource("repo"),
            R::new("repo clone|sync|set-default", E::Read),
            R::new("pr checkout", E::Read),
            R::new("run|release download", E::Read),
            R::new("pr merge", E::Destructive).resource("pull"),
            R::new("* delete|delete-asset|transfer", E::Destructive),
            R::new("pr", E::Write).resource("pull"),
            R::new("issue", E::Write).resource("issue"),
            R::new("release", E::Write).resource("release"),
            R::new("run|workflow", E::Write).resource("run"),
            R::new("repo", E::Write).resource("repo"),
        ],
        default: E::Write,
        pages: pages(&[
            ("pull", "https://github.com/{repo}/pull/{number}"),
            ("issue", "https://github.com/{repo}/issues/{number}"),
        ]),
    };
    let git = CliInfo {
        name: "git".into(),
        description: "git, signed in to github.com as this connection. tab forwards fetches and pushes to GitHub and asks before branch deletes and force pushes, except force pushes to a branch the chat created.".into(),
        auth: CliAuth::GitEndpoint {
            url: "https://github.com/".into(),
            remotes: ["https://github.com/", "git@github.com:", "ssh://git@github.com/"]
                .map(String::from)
                .to_vec(),
        },
        value_flags: Vec::new(),
        rules: Vec::new(),
        // Runs in the chat; only its traffic to GitHub goes through tab.
        default: E::Local,
        pages: pages(&[]),
    };
    vec![gh, git]
}

/// What workflows can do on GitHub. Every one is a single REST call through
/// the gateway, spending from the `core` bucket.
pub fn actions() -> Vec<ActionInfo> {
    let url = || ActionField::new("url", "Link", FieldType::Text);
    vec![
        ActionInfo::new(
            "pr.get",
            "Read a pull request",
            "git-pull-request",
            Effect::Read,
            "Fetches a PR's state, branches, author and size, for conditions in later steps.",
        )
        .inputs(vec![repo_field(), number_field()])
        .outputs(vec![
            ActionField::new("state", "State (open, closed, merged)", FieldType::Text),
            ActionField::new("draft", "Draft", FieldType::Bool),
            ActionField::new("author", "Author", FieldType::Text),
            ActionField::new("base", "Base branch", FieldType::Text),
            ActionField::new("head", "Head branch", FieldType::Text),
            ActionField::new("additions", "Lines added", FieldType::Number),
            ActionField::new("deletions", "Lines removed", FieldType::Number),
            ActionField::new("labels", "Labels", FieldType::List),
            url(),
        ])
        .spends("github/core")
        .for_kinds(&["github.*"]),
        ActionInfo::new(
            "issue.comment",
            "Comment",
            "message",
            Effect::Write,
            "Posts a comment on a pull request or issue, as you.",
        )
        .inputs(vec![
            repo_field(),
            number_field(),
            ActionField::new("body", "Comment", FieldType::LongText)
                .required()
                .help("Markdown."),
        ])
        .outputs(vec![url()])
        .spends("github/core")
        .for_kinds(&["github.*"]),
        ActionInfo::new(
            "issue.add_labels",
            "Add labels",
            "pin",
            Effect::Write,
            "Adds labels to a pull request or issue. Labels that don't exist yet are created.",
        )
        .inputs(vec![
            repo_field(),
            number_field(),
            ActionField::new("labels", "Labels", FieldType::List)
                .required()
                .help("Comma separated."),
        ])
        .spends("github/core")
        .for_kinds(&["github.*"]),
        ActionInfo::new(
            "pr.request_reviewers",
            "Request reviewers",
            "user",
            Effect::Write,
            "Asks people to review a pull request.",
        )
        .inputs(vec![
            repo_field(),
            number_field(),
            ActionField::new("reviewers", "Reviewers", FieldType::List)
                .required()
                .help("GitHub logins, comma separated."),
        ])
        .spends("github/core")
        .for_kinds(&[
            "github.review_requested",
            "github.ci_failed",
            "github.activity",
        ]),
        ActionInfo::new(
            "issue.create",
            "Open an issue",
            "circle-dot",
            Effect::Write,
            "Opens a new issue in a repository.",
        )
        .inputs(vec![
            ActionField::new("repo", "Repository", FieldType::Text)
                .required()
                .help("owner/name"),
            ActionField::new("title", "Title", FieldType::Text).required(),
            ActionField::new("body", "Description", FieldType::LongText),
            ActionField::new("labels", "Labels", FieldType::List),
        ])
        .outputs(vec![
            ActionField::new("number", "Issue number", FieldType::Number),
            url(),
        ])
        .spends("github/core"),
        ActionInfo::new(
            "pr.merge",
            "Merge a pull request",
            "git-merge",
            Effect::Destructive,
            "Merges a pull request. GitHub refuses if checks or reviews block it.",
        )
        .inputs(vec![
            repo_field(),
            number_field(),
            ActionField::new("method", "Merge method", FieldType::Select)
                .options(&["squash", "merge", "rebase"])
                .default("squash"),
        ])
        .outputs(vec![ActionField::new(
            "sha",
            "Merge commit",
            FieldType::Text,
        )])
        .spends("github/core")
        .for_kinds(&["github.review_requested", "github.activity"]),
        ActionInfo::new(
            "issue.close",
            "Close",
            "x-circle",
            Effect::Destructive,
            "Closes a pull request or issue without merging.",
        )
        .inputs(vec![repo_field(), number_field()])
        .spends("github/core")
        .for_kinds(&["github.*"]),
    ]
}

/// `owner/name`, checked so a template can't point the call somewhere else.
fn repo_arg(input: &serde_json::Value) -> anyhow::Result<String> {
    let repo = input["repo"].as_str().unwrap_or_default().trim();
    let ok = repo.split('/').count() == 2
        && repo.split('/').all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        });
    anyhow::ensure!(ok, "repo must look like owner/name, got {repo:?}");
    Ok(repo.to_string())
}

fn number_arg(input: &serde_json::Value) -> anyhow::Result<u64> {
    let n = &input["number"];
    n.as_u64()
        .or_else(|| {
            n.as_str()
                .and_then(|s| s.trim().trim_start_matches('#').parse().ok())
        })
        .ok_or_else(|| anyhow::anyhow!("number must be a PR or issue number, got {n}"))
}

fn list_arg(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::trim).map(str::to_string))
            .filter(|s| !s.is_empty())
            .collect(),
        serde_json::Value::String(s) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
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
    // Some subjects (e.g. discussions, releases) have no title or state.
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    state: String,
    user: Option<User>,
    #[serde(default)]
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

/// Pull requests count only when you wrote them or your review is
/// requested. Other subjects (issues, CI runs, ...) always pass.
fn pr_is_yours(subject_type: &str, reason: &str) -> bool {
    subject_type != "PullRequest" || matches!(reason, "review_requested" | "author")
}

/// Maps GitHub's notification `reason` to a tab kind. CI runs are told
/// apart by the subject title, like "CI workflow run succeeded for main
/// branch", since GitHub notifies about passing runs too.
pub fn kind_for(reason: &str, subject_type: &str, title: &str) -> &'static str {
    match (reason, subject_type) {
        ("review_requested", _) => "github.review_requested",
        ("mention" | "team_mention", _) => "github.mention",
        ("ci_activity", _) | (_, "CheckSuite") => ci_kind(title),
        ("assign", _) => "github.assign",
        ("security_alert", _) => "github.security",
        ("comment" | "author" | "state_change" | "subscribed" | "manual", _) => "github.comment",
        _ => "github.activity",
    }
}

fn ci_kind(title: &str) -> &'static str {
    let t = title.to_ascii_lowercase();
    if t.contains("failed") || t.contains("failure") || t.contains("timed out") {
        "github.ci_failed"
    } else if t.contains("succeeded") || t.contains("success") || t.contains("passed") {
        "github.ci_passed"
    } else {
        "github.ci"
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

    /// `open`, `draft`, `closed` or `merged` for a pull request API URL.
    async fn pr_state(&self, api_url: &str) -> anyhow::Result<&'static str> {
        #[derive(Deserialize)]
        struct Pr {
            state: String,
            #[serde(default)]
            draft: bool,
            #[serde(default)]
            merged: bool,
        }
        let call = Call::get("github", "github", "pull request state", api_url)
            .cache(Duration::from_secs(5 * 60));
        let pr: Pr = self.gateway.send(call).await?.json()?;
        Ok(match (pr.merged, pr.state.as_str(), pr.draft) {
            (true, _, _) => "merged",
            (_, "open", true) => "draft",
            (_, "open", _) => "open",
            _ => "closed",
        })
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
            kind: kind_for(&n.reason, &n.subject.kind, &n.subject.title).into(),
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
            if !is_new {
                continue;
            }
            // Only open and draft pull requests belong in the queue. A PR
            // that closed or merged takes its item with it.
            if !pr_is_yours(&n.subject.kind, &n.reason) {
                ctx.resolve(&n.id).await;
                continue;
            }
            let mut pr_state = None;
            if n.subject.kind == "PullRequest"
                && let Some(url) = n.subject.url.as_deref()
            {
                match self.pr_state(url).await {
                    Ok("closed" | "merged") => {
                        ctx.resolve(&n.id).await;
                        continue;
                    }
                    Ok(s) => pr_state = Some(s),
                    Err(e) => tracing::debug!(error = %e, "couldn't read PR state"),
                }
            }
            let mut e = self.to_event(n).await;
            if let Some(s) = pr_state {
                e.metadata["pr_state"] = s.into();
            }
            ctx.emit(e).await?;
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

    async fn act(
        &self,
        action: &str,
        input: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let send = |method: Method, purpose: &str, url: String, body: Option<serde_json::Value>| {
            let mut call = Call::new(method, "github", "github", purpose, url);
            if let Some(b) = body {
                call = call.json(b);
            }
            self.gateway.send(call)
        };
        match action {
            "pr.get" => {
                let (repo, n) = (repo_arg(input)?, number_arg(input)?);
                // Short cache: a workflow reading the PR it was triggered by
                // shouldn't cost a call per step.
                let call = Call::get(
                    "github",
                    "github",
                    "workflow: read PR",
                    format!("{API}/repos/{repo}/pulls/{n}"),
                )
                .cache(Duration::from_secs(60));
                let p: PullOrIssue = self.gateway.send(call).await?.json()?;
                let state = if p.merged == Some(true) {
                    "merged"
                } else {
                    p.state.as_str()
                };
                Ok(json!({
                    "state": state,
                    "draft": p.draft.unwrap_or(false),
                    "author": p.user.map(|u| u.login),
                    "base": p.base.map(|b| b.name),
                    "head": p.head.map(|h| h.name),
                    "additions": p.additions,
                    "deletions": p.deletions,
                    "labels": p.labels.into_iter().map(|l| l.name).collect::<Vec<_>>(),
                    "url": p.html_url,
                    "title": p.title,
                }))
            }
            "issue.comment" => {
                let (repo, n) = (repo_arg(input)?, number_arg(input)?);
                let body = input["body"].as_str().unwrap_or_default().trim();
                anyhow::ensure!(!body.is_empty(), "the comment is empty");
                let r = send(
                    Method::POST,
                    "workflow: comment",
                    format!("{API}/repos/{repo}/issues/{n}/comments"),
                    Some(json!({ "body": body })),
                )
                .await?;
                let v: serde_json::Value = r.json()?;
                Ok(json!({ "url": v["html_url"] }))
            }
            "issue.add_labels" => {
                let (repo, n) = (repo_arg(input)?, number_arg(input)?);
                let labels = list_arg(&input["labels"]);
                anyhow::ensure!(!labels.is_empty(), "no labels given");
                send(
                    Method::POST,
                    "workflow: add labels",
                    format!("{API}/repos/{repo}/issues/{n}/labels"),
                    Some(json!({ "labels": labels })),
                )
                .await?;
                Ok(json!({ "labels": labels }))
            }
            "pr.request_reviewers" => {
                let (repo, n) = (repo_arg(input)?, number_arg(input)?);
                let reviewers = list_arg(&input["reviewers"]);
                anyhow::ensure!(!reviewers.is_empty(), "no reviewers given");
                send(
                    Method::POST,
                    "workflow: request reviewers",
                    format!("{API}/repos/{repo}/pulls/{n}/requested_reviewers"),
                    Some(json!({ "reviewers": reviewers })),
                )
                .await?;
                Ok(json!({ "reviewers": reviewers }))
            }
            "issue.create" => {
                let repo = repo_arg(input)?;
                let title = input["title"].as_str().unwrap_or_default().trim();
                anyhow::ensure!(!title.is_empty(), "the issue needs a title");
                let mut body = json!({ "title": title });
                if let Some(b) = input["body"].as_str().filter(|b| !b.trim().is_empty()) {
                    body["body"] = b.into();
                }
                let labels = list_arg(&input["labels"]);
                if !labels.is_empty() {
                    body["labels"] = json!(labels);
                }
                let r = send(
                    Method::POST,
                    "workflow: open issue",
                    format!("{API}/repos/{repo}/issues"),
                    Some(body),
                )
                .await?;
                let v: serde_json::Value = r.json()?;
                Ok(json!({ "number": v["number"], "url": v["html_url"] }))
            }
            "pr.merge" => {
                let (repo, n) = (repo_arg(input)?, number_arg(input)?);
                let method = match input["method"].as_str().unwrap_or("squash") {
                    m @ ("merge" | "squash" | "rebase") => m,
                    m => anyhow::bail!("unknown merge method {m}"),
                };
                let r = send(
                    Method::PUT,
                    "workflow: merge PR",
                    format!("{API}/repos/{repo}/pulls/{n}/merge"),
                    Some(json!({ "merge_method": method })),
                )
                .await?;
                let v: serde_json::Value = r.json()?;
                Ok(json!({ "sha": v["sha"] }))
            }
            "issue.close" => {
                let (repo, n) = (repo_arg(input)?, number_arg(input)?);
                send(
                    Method::PATCH,
                    "workflow: close",
                    format!("{API}/repos/{repo}/issues/{n}"),
                    Some(json!({ "state": "closed" })),
                )
                .await?;
                Ok(json!({}))
            }
            other => anyhow::bail!("GitHub has no action {other}"),
        }
    }

    /// An open PR in the item's Context means the work it tracks isn't
    /// finished. The item's own PR doesn't count: finishing it is what Done is for.
    async fn done_guards(&self, ctx: &ItemContext) -> anyhow::Result<Vec<DoneGuard>> {
        let own = ctx.item.event.url.as_deref().and_then(pull_ref);
        let mut pulls: Vec<(String, u64)> = Vec::new();
        for e in &ctx.entries {
            if e.section != ContextSection::Context
                || e.ref_item.as_ref().is_some_and(|r| r.id == ctx.item.id)
            {
                continue;
            }
            let url = e
                .url
                .as_deref()
                .or(e.ref_item.as_ref().and_then(|r| r.event.url.as_deref()));
            if let Some(p) = url.and_then(pull_ref)
                && Some(&p) != own.as_ref()
                && !pulls.contains(&p)
            {
                pulls.push(p);
            }
        }
        let gw = &self.gateway;
        let sums = futures_util::future::join_all(pulls.iter().map(|(repo, n)| async move {
            crate::connectors::github_review::link_summary(gw, repo, "pull", &n.to_string()).await
        }))
        .await;
        let mut out = Vec::new();
        for ((repo, n), s) in pulls.iter().zip(sums) {
            let Ok(s) = s else { continue };
            if s.state != "open" && s.state != "draft" {
                continue;
            }
            let mut left = Vec::new();
            if s.state == "draft" {
                left.push("draft".to_string());
            }
            if let Some(t) = s.open_threads.filter(|t| *t > 0) {
                left.push(format!(
                    "{t} unresolved thread{}",
                    if t == 1 { "" } else { "s" }
                ));
            }
            if s.review_decision
                .as_deref()
                .is_some_and(|d| d.eq_ignore_ascii_case("changes_requested"))
            {
                left.push("changes requested".into());
            }
            if s.checks.as_deref() == Some("failing") {
                left.push("checks failing".into());
            }
            out.push(DoneGuard {
                source: "github".into(),
                level: GuardLevel::Confirm,
                title: format!("{repo}#{n} is still open"),
                detail: (!left.is_empty()).then(|| left.join(", ")),
                url: Some(format!("https://github.com/{repo}/pull/{n}")),
            });
        }
        Ok(out)
    }

    async fn is_resolved(&self, item: &QueueItem) -> anyhow::Result<bool> {
        let m = &item.event.metadata;
        if m["subject_type"].as_str() != Some("PullRequest") {
            return Ok(false);
        }
        if !pr_is_yours("PullRequest", m["reason"].as_str().unwrap_or_default()) {
            return Ok(true);
        }
        let Some(url) = m["api_url"].as_str() else {
            return Ok(false);
        };
        Ok(matches!(self.pr_state(url).await?, "closed" | "merged"))
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

/// `https://github.com/o/r/pull/12[/files…]` → `("o/r", 12)`.
fn pull_ref(url: &str) -> Option<(String, u64)> {
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("https://www.github.com/"))?;
    let mut it = path.split(['/', '?', '#']);
    let (owner, repo, what, n) = (it.next()?, it.next()?, it.next()?, it.next()?);
    (what == "pull" && !owner.is_empty() && !repo.is_empty()).then_some(())?;
    Some((format!("{owner}/{repo}"), n.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_refs() {
        assert_eq!(
            pull_ref("https://github.com/o/r/pull/12/files"),
            Some(("o/r".into(), 12))
        );
        assert_eq!(
            pull_ref("https://github.com/o/r/pull/12#top"),
            Some(("o/r".into(), 12))
        );
        assert_eq!(pull_ref("https://github.com/o/r/issues/12"), None);
        assert_eq!(pull_ref("https://github.com/o/r/pull/new/branch"), None);
    }

    #[test]
    fn keeps_only_your_prs() {
        assert!(pr_is_yours("PullRequest", "review_requested"));
        assert!(pr_is_yours("PullRequest", "author"));
        assert!(!pr_is_yours("PullRequest", "subscribed"));
        assert!(!pr_is_yours("PullRequest", "comment"));
        assert!(pr_is_yours("Issue", "subscribed"));
    }

    #[test]
    fn maps_reasons_and_urls() {
        assert_eq!(
            kind_for("review_requested", "PullRequest", "Fix login"),
            "github.review_requested"
        );
        assert_eq!(
            kind_for(
                "ci_activity",
                "CheckSuite",
                "CI workflow run failed for main branch"
            ),
            "github.ci_failed"
        );
        assert_eq!(
            kind_for(
                "ci_activity",
                "CheckSuite",
                "CI workflow run succeeded for fix branch"
            ),
            "github.ci_passed"
        );
        assert_eq!(
            kind_for(
                "ci_activity",
                "CheckSuite",
                "CI workflow run cancelled for main branch"
            ),
            "github.ci"
        );
        assert_eq!(kind_for("team_mention", "Issue", "Bug"), "github.mention");
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
