//! The job runner: takes agent jobs workflows hand out, on this runner.
//!
//! Every runner on a hub (each of the person's computers, and a tab cloud
//! runner on paid plans) runs one. It says what it can do on its runner
//! record (harnesses installed, repos checked out), and every few seconds
//! looks for open jobs it may take ([`Job::can_take`]). For each, it asks
//! the hub for the job's lease; only the runner that gets it works the job.
//!
//! Working a job means starting an agent chat with the job's prompt, in the
//! repo's folder, with the job's harness, and renewing the lease while it
//! runs. The chat syncs like any other, so the person can watch or answer it
//! from another device. When the agent finishes, the runner writes what came
//! of it (its last message, and any pull request it mentions), but only if
//! it still holds the lease with the same fencing token: a runner whose
//! lease lapsed and was taken by another can't overwrite that one's work.
//!
//! A cloud runner's agents work on code nobody at the keyboard checked, in
//! a sandbox with no git host token. The runner clones the job's GitHub
//! repo itself, and when the agent is done pushes the branch it committed
//! to, with the hub's git access ([`JobRunner::set_git_token`]) handed to
//! that one git command and nothing else.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
pub use tab_domain::runner::RunnerKind as Kind;
use tab_domain::runner::{
    CloudGap, JOB_LEASE, JOB_RENEW, Job, JobResult, JobState, RunnerCaps, RunnerKind, job_lease,
};
use tab_domain::workflow::Intervention;
pub use tokio_util::sync::CancellationToken;

use crate::agents::AgentHub;
use crate::engine::EngineEvent;
use crate::leases::{self, Leases};
use crate::model::{AgentKind, SessionStatus};
use crate::store::Store;

mod watch;

/// How often the runner looks for jobs.
const LOOK: Duration = Duration::from_secs(15);
/// How often it says it's awake on its runner record.
const ANNOUNCE: Duration = Duration::from_secs(60);
/// Jobs one runner works at once.
const AT_ONCE: usize = 2;

/// What working a job needs from the app.
pub trait Agents: Send + Sync + 'static {
    /// Harnesses installed here.
    fn harnesses(&self) -> futures_util::future::BoxFuture<'_, Vec<AgentKind>>;
    /// The folder to work in when the job names no repo.
    fn default_folder(&self) -> String;
    /// Starts a chat keyed `key` with `kind` in `cwd` and sends it `prompt`.
    /// Returns the chat's id.
    fn start(
        &self,
        key: &str,
        kind: Option<AgentKind>,
        model: &str,
        title: &str,
        cwd: &str,
        prompt: &str,
    ) -> anyhow::Result<String>;
    /// Stops a chat's agent.
    fn stop(&self, session: &str);
    /// What a chat needed in tab cloud and didn't have (taken once).
    fn gaps(&self, _session: &str) -> Vec<CloudGap> {
        Vec::new()
    }
}

impl Agents for Arc<AgentHub> {
    fn harnesses(&self) -> futures_util::future::BoxFuture<'_, Vec<AgentKind>> {
        Box::pin(async move {
            self.detect()
                .await
                .into_iter()
                .filter(|p| p.found)
                .map(|p| p.kind)
                .collect()
        })
    }
    fn default_folder(&self) -> String {
        self.workdir()
    }
    fn start(
        &self,
        key: &str,
        kind: Option<AgentKind>,
        model: &str,
        title: &str,
        cwd: &str,
        prompt: &str,
    ) -> anyhow::Result<String> {
        let store = &self.store;
        let session = match store.session_for_key(key)? {
            Some(s) => s,
            None => {
                let kind = kind
                    .or(self.config().kind)
                    .ok_or_else(|| anyhow::anyhow!("no agent is set up on this runner"))?;
                // The model comes from the harness's setting on this
                // runner; a per-job model needs a per-chat model setting.
                let _ = model;
                store.session_create_keyed(key, kind, title, cwd, Utc::now())?
            }
        };
        self.emit(EngineEvent::SessionChanged {
            session: session.clone(),
        });
        self.send(&session.id, prompt)?;
        Ok(session.id)
    }
    fn stop(&self, session: &str) {
        AgentHub::stop(self, session)
    }
    fn gaps(&self, session: &str) -> Vec<CloudGap> {
        self.take_gaps(session)
    }
}

pub struct JobRunner {
    store: Arc<Store>,
    agents: Arc<dyn Agents>,
    leases: Arc<dyn Leases>,
    kind: RunnerKind,
    name: String,
    /// Jobs this runner is working.
    busy: Mutex<HashSet<String>>,
    harnesses: Mutex<Option<(std::time::Instant, Vec<String>)>>,
    /// A cloud runner's git access, kept in memory only.
    git_token: Mutex<Option<String>>,
}

impl JobRunner {
    pub fn new(
        store: Arc<Store>,
        agents: Arc<dyn Agents>,
        leases: Arc<dyn Leases>,
        kind: RunnerKind,
        name: &str,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            agents,
            leases,
            kind,
            name: name.into(),
            busy: Default::default(),
            harnesses: Default::default(),
            git_token: Default::default(),
        })
    }

    /// The GitHub token a cloud runner clones and pushes with. It goes to
    /// the runner's own git commands, never to an agent.
    pub fn set_git_token(&self, token: Option<String>) {
        *self.git_token.lock().unwrap() = token;
    }

    fn git_token(&self) -> Option<String> {
        self.git_token.lock().unwrap().clone()
    }

    /// Looks for jobs until `cancel`. Call it on the tokio runtime (inside
    /// a task, or `block_on`): it spawns the runner's loop there.
    pub fn start(self: &Arc<Self>, cancel: CancellationToken) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut look = tokio::time::interval(LOOK);
            let mut last_announce: Option<std::time::Instant> = None;
            loop {
                tokio::select! {
                    _ = look.tick() => {}
                    _ = cancel.cancelled() => break,
                }
                if last_announce.is_none_or(|t| t.elapsed() >= ANNOUNCE) {
                    let caps = me.caps(Utc::now()).await;
                    if let Err(e) = me.store.runner_put(&caps) {
                        tracing::warn!(error = %e, "couldn't say this runner is awake");
                    }
                    last_announce = Some(std::time::Instant::now());
                }
                me.take(Utc::now()).await;
            }
        });
    }

    /// What this runner can do now.
    pub async fn caps(&self, now: DateTime<Utc>) -> RunnerCaps {
        let harnesses = {
            let cached = self.harnesses.lock().unwrap().clone();
            match cached {
                Some((at, h)) if at.elapsed() < Duration::from_secs(600) => h,
                _ => {
                    let h: Vec<String> = self
                        .agents
                        .harnesses()
                        .await
                        .into_iter()
                        .map(|k| k.as_str().to_string())
                        .collect();
                    *self.harnesses.lock().unwrap() = Some((std::time::Instant::now(), h.clone()));
                    h
                }
            }
        };
        let repos = match self.kind {
            RunnerKind::Cloud => Vec::new(),
            RunnerKind::Computer => self
                .store
                .repos_checked_out()
                .unwrap_or_default()
                .into_iter()
                .map(|(r, _)| r)
                .collect(),
        };
        RunnerCaps {
            id: self.leases.holder(),
            name: self.name.clone(),
            kind: self.kind,
            harnesses,
            repos,
            connections: Vec::new(),
            seen_at: Some(now),
        }
    }

    /// Claims and starts the jobs this runner may take.
    pub async fn take(self: &Arc<Self>, now: DateTime<Utc>) {
        let open = match self.store.jobs_open() {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, "listing jobs failed");
                return;
            }
        };
        if open.is_empty() {
            return;
        }
        let caps = self.caps(now).await;
        for job in open {
            if self.busy.lock().unwrap().len() >= AT_ONCE {
                return;
            }
            if self.busy.lock().unwrap().contains(&job.id) {
                continue;
            }
            // The hub knows whether a holder's lease is live; ask it.
            if job.can_take(&caps, false, now).is_err() {
                continue;
            }
            let claim = match leases::claim(&self.leases, &job_lease(&job.id), JOB_LEASE).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!(error = %e, "couldn't claim a job");
                    continue;
                }
            };
            if !claim.won {
                continue;
            }
            // Read it again now it's ours: it may have finished meanwhile.
            let Ok(Some(fresh)) = self.store.job_get(&job.id) else {
                let _ = leases::release(&self.leases, &job_lease(&job.id)).await;
                continue;
            };
            if fresh.finished() {
                let _ = leases::release(&self.leases, &job_lease(&job.id)).await;
                continue;
            }
            let job = fresh.claimed(&caps.id, claim.token, Utc::now());
            if let Err(e) = self.store.job_put(&job, Utc::now()) {
                tracing::warn!(error = %e, "couldn't mark a job taken");
                let _ = leases::release(&self.leases, &job_lease(&job.id)).await;
                continue;
            }
            self.busy.lock().unwrap().insert(job.id.clone());
            let me = self.clone();
            tokio::spawn(async move {
                let id = job.id.clone();
                if let Err(e) = me.work(job, claim.token).await {
                    tracing::warn!(job = %id, error = %e, "working a job failed");
                }
                me.busy.lock().unwrap().remove(&id);
            });
        }
    }

    /// Writes `state` to the job if this runner still holds it.
    fn finish(&self, id: &str, me: &str, token: u64, state: JobState) -> anyhow::Result<bool> {
        self.finish_with(id, me, token, state, Vec::new(), Vec::new())
    }

    /// [`JobRunner::finish`], noting what the job needed and didn't have,
    /// and each time the person stepped in.
    fn finish_with(
        &self,
        id: &str,
        me: &str,
        token: u64,
        state: JobState,
        gaps: Vec<CloudGap>,
        stepped_in: Vec<Intervention>,
    ) -> anyhow::Result<bool> {
        let Some(mut job) = self.store.job_get(id)? else {
            return Ok(false);
        };
        if !job.accepts_result_from(me, token) {
            return Ok(false);
        }
        for g in gaps {
            CloudGap::add(&mut job.missing, g);
        }
        job.interventions.extend(stepped_in);
        job.state = state;
        self.store.job_put(&job, Utc::now())?;
        Ok(true)
    }

    async fn work(self: &Arc<Self>, job: Job, token: u64) -> anyhow::Result<()> {
        let me = self.leases.holder();
        let key = job_lease(&job.id);
        let cwd = if job.spec.repo.is_empty() {
            self.agents.default_folder()
        } else {
            match self.kind {
                RunnerKind::Computer => self
                    .store
                    .repos_checked_out()?
                    .into_iter()
                    .find(|(r, _)| r.eq_ignore_ascii_case(&job.spec.repo))
                    .map(|(_, p)| p)
                    // A repo's path can come from an agent or another
                    // device; one wider than a code folder isn't used.
                    .filter(|p| !crate::sandbox::current().too_wide(Path::new(p)))
                    .unwrap_or_else(|| self.agents.default_folder()),
                // A cloud runner clones into its own folder first.
                RunnerKind::Cloud => {
                    match clone_into(
                        &self.agents.default_folder(),
                        &job.spec.repo,
                        self.git_token().as_deref(),
                    )
                    .await
                    {
                        Ok(dir) => dir,
                        Err(e) => {
                            self.finish(
                                &job.id,
                                &me,
                                token,
                                JobState::Failed {
                                    error: format!("{e:#}"),
                                    finished_at: Utc::now(),
                                    usage: None,
                                },
                            )?;
                            let _ = leases::release(&self.leases, &key).await;
                            return Ok(());
                        }
                    }
                }
            }
        };
        let kind = (!job.spec.harness.is_empty()).then(|| AgentKind::parse(&job.spec.harness));
        let title = format!("Workflow job: {}", first_line(&job.spec.prompt));
        let prompt = match self.kind {
            RunnerKind::Computer => job.spec.prompt.clone(),
            RunnerKind::Cloud => format!("{}{CLOUD_NOTE}", job.spec.prompt),
        };
        let plan = self.store.plan_windows().unwrap_or_default();
        let started = Utc::now();
        let session = match self.agents.start(
            &format!("job:{}", job.id),
            kind,
            &job.spec.model,
            &title,
            &cwd,
            &prompt,
        ) {
            Ok(s) => s,
            Err(e) => {
                self.finish(
                    &job.id,
                    &me,
                    token,
                    JobState::Failed {
                        error: format!("couldn't start the agent: {e:#}"),
                        finished_at: Utc::now(),
                        usage: None,
                    },
                )?;
                let _ = leases::release(&self.leases, &key).await;
                return Ok(());
            }
        };
        // Note the chat on the job, so the person can open it.
        if let Some(mut j) = self.store.job_get(&job.id)?
            && j.accepts_result_from(&me, token)
        {
            if let JobState::Running { session: s, .. } = &mut j.state {
                *s = Some(session.clone());
            }
            self.store.job_put(&j, Utc::now())?;
        }

        let mut watch = watch::Watch::new(&self.store, &session, plan, started);

        // Give the turn a moment to start before reading its status.
        tokio::time::sleep(watch::POLL).await;
        let poll = watch::POLL;
        let mut since_renew = Duration::ZERO;
        loop {
            if Utc::now() >= job.deadline {
                self.agents.stop(&session);
                let (usage, stepped_in) = watch.report(&self.store, Utc::now(), true);
                self.finish_with(
                    &job.id,
                    &me,
                    token,
                    JobState::Failed {
                        error: "The agent didn't finish before the job's deadline.".into(),
                        finished_at: Utc::now(),
                        usage: Some(usage),
                    },
                    self.agents.gaps(&session),
                    stepped_in,
                )?;
                break;
            }
            let s = self.store.session_get(&session)?;
            watch.saw(s.as_ref().map(|s| s.status), Utc::now());
            match s.as_ref().map(|s| s.status) {
                Some(SessionStatus::Idle) => {
                    let mut result = self.result_of(&session)?;
                    let (usage, stepped_in) = watch.report(&self.store, Utc::now(), false);
                    result.usage = Some(usage);
                    if self.kind == RunnerKind::Cloud && !job.spec.repo.is_empty() {
                        match push_branch(
                            Path::new(&cwd),
                            &job.spec.repo,
                            self.git_token().as_deref(),
                        )
                        .await
                        {
                            Ok(Some(b)) => result.branch = Some(b),
                            Ok(None) => {}
                            Err(e) => {
                                tracing::warn!(job = %job.id, error = %e, "pushing the job's branch failed");
                                result.summary = format!(
                                    "{}\n\n(tab couldn't push the branch: {e:#})",
                                    result.summary
                                );
                            }
                        }
                    }
                    self.finish_with(
                        &job.id,
                        &me,
                        token,
                        JobState::Done {
                            runner: me.clone(),
                            result,
                            finished_at: Utc::now(),
                        },
                        self.agents.gaps(&session),
                        stepped_in,
                    )?;
                    break;
                }
                Some(SessionStatus::Failed) | None => {
                    let why = self
                        .store
                        .messages(&session)?
                        .into_iter()
                        .rev()
                        .find(|m| m.kind == "error")
                        .map(|m| m.content)
                        .unwrap_or_else(|| "The agent stopped with an error.".into());
                    let (usage, stepped_in) = watch.report(&self.store, Utc::now(), false);
                    self.finish_with(
                        &job.id,
                        &me,
                        token,
                        JobState::Failed {
                            error: why,
                            finished_at: Utc::now(),
                            usage: Some(usage),
                        },
                        self.agents.gaps(&session),
                        stepped_in,
                    )?;
                    break;
                }
                // Working, monitoring, or waiting on the person: keep it.
                _ => {}
            }
            tokio::time::sleep(poll).await;
            since_renew += poll;
            if since_renew >= JOB_RENEW.to_std().unwrap_or(Duration::from_secs(30)) {
                since_renew = Duration::ZERO;
                match leases::claim(&self.leases, &key, JOB_LEASE).await {
                    Ok(c) if c.won && c.token == token => {}
                    Ok(_) => {
                        // Someone else has it now: their run counts.
                        tracing::warn!(job = %job.id, "lost a job's lease; stopping");
                        self.agents.stop(&session);
                        return Ok(());
                    }
                    // Offline for a moment: the lease outlasts a few misses.
                    Err(e) => tracing::debug!(error = %e, "couldn't renew a job lease"),
                }
            }
        }
        let _ = leases::release(&self.leases, &key).await;
        Ok(())
    }

    /// What came of a chat: its last reply and any pull request it names.
    fn result_of(&self, session: &str) -> anyhow::Result<JobResult> {
        let msgs = self.store.messages(session)?;
        let summary = msgs
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && m.kind == "text")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let mut pr_url = None;
        let mut branch = None;
        for m in &msgs {
            for u in tab_domain::links::urls(&m.content) {
                if u.contains("/pull/") {
                    pr_url = Some(u);
                }
            }
            if let Some(b) = m.meta.get("branch").and_then(|b| b.as_str()) {
                branch = Some(b.to_string());
            }
        }
        Ok(JobResult {
            summary,
            branch,
            pr_url,
            usage: None,
        })
    }
}

/// This computer's name, for its runner record.
pub fn computer_name() -> String {
    sysinfo::System::host_name()
        .ok()
        .map(|h| h.trim_end_matches(".local").to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "This computer".into())
}

fn first_line(s: &str) -> String {
    let l = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    if l.chars().count() > 60 {
        format!("{}…", l.chars().take(60).collect::<String>())
    } else {
        l.to_string()
    }
}

/// What a cloud job's agent is told about where it works.
const CLOUD_NOTE: &str = "\n\n---\nYou're running in tab cloud, in a sandbox without git or GitHub credentials. \
When you're done, commit your work on a new branch (not the default branch); tab pushes that branch \
for you. Don't try to push or open a pull request yourself.\n\
Connections (Linear, Sentry, ...) the person allowed in tab cloud are your MCP tools; ones that \
change something may be off here. If one you need is off, or a connection only offers \
`request_access`, call that once, finish what you can, and say what's left.";

/// `owner/name` from what a job names: `owner/name` or a
/// `https://github.com/owner/name` URL. Anything else is refused, so a
/// templated value can't point the runner, and its git access, elsewhere.
pub fn github_repo(repo: &str) -> anyhow::Result<(String, String)> {
    let r = repo.trim();
    let path = r
        .strip_prefix("https://github.com/")
        .unwrap_or(r)
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.len() <= 100
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match path.split('/').collect::<Vec<_>>().as_slice() {
        [owner, name] if ok(owner) && ok(name) && !owner.starts_with('-') => {
            Ok(((*owner).to_string(), (*name).to_string()))
        }
        _ => anyhow::bail!(
            "a cloud job's repo must be a GitHub repo, owner/name; {repo:?} isn't one"
        ),
    }
}

/// A git command for the runner itself, with `token` (if any) answering
/// only GitHub's credential prompts, for this one command. Nothing is
/// written to disk and no other credential store is asked.
///
/// The runner runs git outside the sandbox, in checkouts a job's agent
/// wrote to (`.git/config` and hooks included), so whatever the repository
/// configures to run (hooks, `core.fsmonitor`, `core.sshCommand`, filters,
/// proxies) is switched off first ([`crate::sandbox::broker::git_hardening`]),
/// as for tab's other git commands outside the sandbox. The token is only
/// given to git fetching from GitHub's own URL, or pushing from a
/// repository tab made ([`push_branch`]).
async fn git(args: &[&str], cwd: &Path, token: Option<&str>) -> tokio::process::Command {
    let exe = which::which("git").unwrap_or_else(|_| "git".into());
    let mut cfg: Vec<(String, String)> = vec![
        // Empty first: forget any helper configured elsewhere.
        ("credential.helper".into(), String::new()),
        (
            "credential.https://github.com.helper".into(),
            if token.is_some() {
                "!f() { test \"$1\" = get && printf 'username=x-access-token\\npassword=%s\\n' \"$TAB_GIT_TOKEN\"; }; f".into()
            } else {
                String::new()
            },
        ),
        ("http.sslVerify".into(), "true".into()),
    ];
    if cwd.join(".git").exists() || cwd.join("HEAD").exists() {
        cfg.extend(crate::sandbox::broker::git_hardening(&exe, cwd).await);
    }
    let mut c = tokio::process::Command::new(&exe);
    c.args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", "https")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .env("GIT_CONFIG_COUNT", cfg.len().to_string())
        .kill_on_drop(true);
    for (i, (k, v)) in cfg.iter().enumerate() {
        c.env(format!("GIT_CONFIG_KEY_{i}"), k);
        c.env(format!("GIT_CONFIG_VALUE_{i}"), v);
    }
    match token {
        Some(t) => c.env("TAB_GIT_TOKEN", t),
        None => c.env_remove("TAB_GIT_TOKEN"),
    };
    c
}

/// Runs `git args` in `cwd`, failing with its stderr if git does.
async fn git_ok(args: &[&str], cwd: &Path, token: Option<&str>, what: &str) -> anyhow::Result<()> {
    let out = git(args, cwd, token).await.output().await?;
    anyhow::ensure!(
        out.status.success(),
        "git {what} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

/// Clones a GitHub repo under `root/repos/` for a cloud runner, or fetches
/// it if it's there already.
async fn clone_into(root: &str, repo: &str, token: Option<&str>) -> anyhow::Result<String> {
    let (owner, name) = github_repo(repo)?;
    let url = format!("https://github.com/{owner}/{name}.git");
    let dir = Path::new(root)
        .join("repos")
        .join(format!("{owner}__{name}"));
    if dir.join(".git").exists() {
        // An earlier job's agent may have pointed `origin` elsewhere.
        git_ok(
            &["remote", "set-url", "--", "origin", &url],
            &dir,
            None,
            "remote set-url",
        )
        .await?;
        git_ok(&["fetch", "--prune", "origin"], &dir, token, "fetch").await?;
    } else {
        std::fs::create_dir_all(dir.parent().unwrap_or(Path::new(root)))?;
        git_ok(
            &["clone", "--", &url, &dir.to_string_lossy()],
            Path::new(root),
            token,
            "clone",
        )
        .await?;
    }
    Ok(dir.to_string_lossy().into_owned())
}

/// Pushes the branch the agent left checked out, if it isn't the default
/// branch and has commits the remote doesn't. Returns the branch pushed.
///
/// The push doesn't run in the agent's checkout: the branch is fetched
/// (without the token) into an empty repository tab makes, and pushed from
/// there to `repo` on GitHub, so nothing the agent wrote in `.git` runs
/// with the token or decides where it goes.
async fn push_branch(
    dir: &Path,
    repo: &str,
    token: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let out = |args: &'static [&'static str]| {
        let dir = dir.to_path_buf();
        async move {
            let o = git(args, &dir, None).await.output().await?;
            anyhow::Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
        }
    };
    let branch = out(&["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let default = out(&["rev-parse", "--abbrev-ref", "origin/HEAD"]).await?;
    let default = default.strip_prefix("origin/").unwrap_or(&default);
    if branch.is_empty() || branch == "HEAD" || branch == default {
        return Ok(None);
    }
    let ahead = out(&["rev-list", "--count", "origin/HEAD..HEAD"]).await?;
    if ahead.parse::<u64>().unwrap_or(0) == 0 {
        return Ok(None);
    }
    anyhow::ensure!(
        token.is_some(),
        "tab cloud has no git access to push with; add it in Settings → tab cloud"
    );
    let head = out(&["rev-parse", "--verify", "HEAD^{commit}"]).await?;
    let (owner, name) = github_repo(repo)?;
    let url = format!("https://github.com/{owner}/{name}.git");
    let scratch = std::env::temp_dir().join(format!("tab-push-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&scratch)?;
    let pushed = async {
        git_ok(&["init", "--bare", "-q", "."], &scratch, None, "init").await?;
        // From the agent's checkout, without the token.
        let from = dir.to_string_lossy().into_owned();
        let src = format!("+refs/heads/{branch}:refs/heads/{branch}");
        let mut fetch = git(&["fetch", "--no-tags", "--", &from, &src], &scratch, None).await;
        fetch.env("GIT_ALLOW_PROTOCOL", "file");
        let o = fetch.output().await?;
        anyhow::ensure!(
            o.status.success(),
            "git fetch from the job's checkout failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let at = format!("refs/heads/{branch}^{{commit}}");
        let got = git(&["rev-parse", "--verify", &at], &scratch, None)
            .await
            .output()
            .await?;
        anyhow::ensure!(
            String::from_utf8_lossy(&got.stdout).trim() == head,
            "the job's branch changed while tab was pushing it"
        );
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        git_ok(&["push", "--", &url, &refspec], &scratch, token, "push").await
    }
    .await;
    let _ = std::fs::remove_dir_all(&scratch);
    pushed.map(|()| Some(branch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leases::InMemory;
    use futures_util::future::BoxFuture;
    use tab_domain::runner::{JobSpec, Placement};

    /// Agents that "finish" a chat as soon as it starts.
    struct Instant(Arc<Store>, Mutex<Vec<String>>);

    impl Agents for Instant {
        fn harnesses(&self) -> BoxFuture<'_, Vec<AgentKind>> {
            Box::pin(async { vec![AgentKind::ClaudeCode] })
        }
        fn default_folder(&self) -> String {
            std::env::temp_dir().to_string_lossy().into_owned()
        }
        fn start(
            &self,
            key: &str,
            kind: Option<AgentKind>,
            _model: &str,
            title: &str,
            cwd: &str,
            prompt: &str,
        ) -> anyhow::Result<String> {
            self.1.lock().unwrap().push(prompt.to_string());
            let s = self.0.session_create_keyed(
                key,
                kind.unwrap_or(AgentKind::ClaudeCode),
                title,
                cwd,
                Utc::now(),
            )?;
            self.0.message_add(
                &s.id,
                "assistant",
                "text",
                "Opened https://github.com/acme/api/pull/7",
                &serde_json::json!({}),
                Utc::now(),
            )?;
            self.0.usage_add(
                &crate::store::NewUsage {
                    purpose: "agent",
                    session_id: Some(s.id.clone()),
                    agent: "claude_code".into(),
                    model: Some("claude-opus-5-5".into()),
                    billing: "api",
                    cost_usd: 0.3,
                    input_tokens: 1200,
                    output_tokens: 80,
                },
                Utc::now(),
            )?;
            self.0
                .session_update(&s.id, Some(SessionStatus::Idle), None, Utc::now())?;
            Ok(s.id)
        }
        fn stop(&self, _session: &str) {}
        fn gaps(&self, _session: &str) -> Vec<CloudGap> {
            vec![linear_gap()]
        }
    }

    fn linear_gap() -> CloudGap {
        CloudGap {
            connection: "linear".into(),
            name: "Linear".into(),
            tool: None,
            why: tab_domain::runner::GapKind::NotInCloud,
        }
    }

    fn job(placement: Placement) -> Job {
        let now = Utc::now();
        Job {
            id: "j1".into(),
            run_id: "r1".into(),
            step_id: "fix".into(),
            workflow_id: "w1".into(),
            spec: JobSpec {
                prompt: "Fix the flaky test".into(),
                placement,
                ..Default::default()
            },
            state: JobState::Waiting,
            attempts: 0,
            max_attempts: 2,
            created_at: now,
            deadline: now + chrono::Duration::hours(1),
            missing: Vec::new(),
            claimed_at: None,
            interventions: Vec::new(),
        }
    }

    async fn until(mut f: impl FnMut() -> bool) {
        for _ in 0..200 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out");
    }

    #[tokio::test]
    async fn a_computer_takes_a_job_runs_it_and_writes_what_came_of_it() {
        let store = Arc::new(Store::in_memory().unwrap());
        store
            .job_put(&job(Placement::Computers), Utc::now())
            .unwrap();
        let agents = Arc::new(Instant(store.clone(), Mutex::new(vec![])));
        let leases = InMemory::new("laptop");
        let runner = JobRunner::new(
            store.clone(),
            agents.clone(),
            leases.clone(),
            RunnerKind::Computer,
            "Laptop",
        );
        runner.take(Utc::now()).await;
        let s = store.clone();
        until(move || s.job_get("j1").unwrap().unwrap().finished()).await;
        let j = store.job_get("j1").unwrap().unwrap();
        let JobState::Done {
            runner: by, result, ..
        } = &j.state
        else {
            panic!("{:?}", j.state)
        };
        assert_eq!(by, "laptop");
        assert_eq!(
            result.pr_url.as_deref(),
            Some("https://github.com/acme/api/pull/7")
        );
        assert_eq!(j.attempts, 1);
        assert_eq!(*agents.1.lock().unwrap(), ["Fix the flaky test"]);
        // What the chat needed and didn't have is on the job.
        assert_eq!(j.missing, [linear_gap()]);
        // And what it spent, which never leaves this computer otherwise.
        let u = result.usage.clone().unwrap();
        assert_eq!(u.billing, tab_domain::workflow::Billing::Api);
        assert!((u.cost_usd - 0.3).abs() < 1e-9);
        assert_eq!((u.input_tokens, u.output_tokens), (1200, 80));
        assert_eq!(
            (u.harness.as_str(), u.model.as_str()),
            ("claude_code", "claude-opus-5-5")
        );
        assert!(j.claimed_at.is_some());
        assert!(j.interventions.is_empty(), "{:?}", j.interventions);
    }

    /// Agents whose chat asks the person something, gets an answer, is
    /// then told something unasked, and finishes; on a Claude plan.
    struct Asks(Arc<Store>);

    impl Agents for Asks {
        fn harnesses(&self) -> BoxFuture<'_, Vec<AgentKind>> {
            Box::pin(async { vec![AgentKind::ClaudeCode] })
        }
        fn default_folder(&self) -> String {
            std::env::temp_dir().to_string_lossy().into_owned()
        }
        fn start(
            &self,
            key: &str,
            kind: Option<AgentKind>,
            _model: &str,
            title: &str,
            cwd: &str,
            prompt: &str,
        ) -> anyhow::Result<String> {
            let store = self.0.clone();
            let s = store.session_create_keyed(
                key,
                kind.unwrap_or(AgentKind::ClaudeCode),
                title,
                cwd,
                Utc::now(),
            )?;
            store.message_add(
                &s.id,
                "user",
                "text",
                prompt,
                &serde_json::json!({}),
                Utc::now(),
            )?;
            store.session_update(&s.id, Some(SessionStatus::Working), None, Utc::now())?;
            let id = s.id.clone();
            // The runner looks every 3s: at 3 (working), 6 (waiting), 9
            // (working again) and 12 (done).
            tokio::spawn(async move {
                let at = |secs: u64| tokio::time::sleep(Duration::from_secs(secs));
                let say = |role: &str, text: &str| {
                    store
                        .message_add(&id, role, "text", text, &serde_json::json!({}), Utc::now())
                        .unwrap();
                };
                let set = |st| {
                    store
                        .session_update(&id, Some(st), None, Utc::now())
                        .unwrap();
                };
                at(4).await;
                say("assistant", "Which test?");
                set(SessionStatus::NeedsInput);
                at(3).await;
                say("user", "The clock one");
                set(SessionStatus::Working);
                at(3).await;
                say("user", "Also, leave CI alone");
                at(1).await;
                store
                    .usage_add(
                        &crate::store::NewUsage {
                            purpose: "agent",
                            session_id: Some(id.clone()),
                            agent: "claude_code".into(),
                            model: Some("claude-sonnet-5".into()),
                            billing: "subscription",
                            cost_usd: 0.8,
                            input_tokens: 900,
                            output_tokens: 60,
                        },
                        Utc::now(),
                    )
                    .unwrap();
                say("assistant", "Pinned the clock.");
                set(SessionStatus::Idle);
            });
            Ok(s.id)
        }
        fn stop(&self, _session: &str) {}
    }

    #[tokio::test]
    async fn a_job_notes_each_time_the_person_stepped_in_and_how_long_it_waited() {
        let store = Arc::new(Store::in_memory().unwrap());
        store
            .job_put(&job(Placement::Computers), Utc::now())
            .unwrap();
        let runner = JobRunner::new(
            store.clone(),
            Arc::new(Asks(store.clone())),
            InMemory::new("laptop"),
            RunnerKind::Computer,
            "Laptop",
        );
        runner.take(Utc::now()).await;
        for _ in 0..400 {
            if store.job_get("j1").unwrap().unwrap().finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let j = store.job_get("j1").unwrap().unwrap();
        let JobState::Done { result, .. } = &j.state else {
            panic!("{:?}", j.state)
        };
        let kinds: Vec<_> = j.interventions.iter().map(|i| i.kind).collect();
        use tab_domain::workflow::InterventionKind as K;
        assert_eq!(kinds, [K::Question, K::Message]);
        let u = result.usage.clone().unwrap();
        // About one look apart: seen waiting at 6s, answered by 9s.
        assert!(
            (1_000..=6_000).contains(&u.human_wait_ms),
            "{}",
            u.human_wait_ms
        );
        // On the plan: tokens, not dollars.
        assert_eq!(u.billing, tab_domain::workflow::Billing::Subscription);
        assert_eq!(u.cost_usd, 0.0);
        assert_eq!(u.input_tokens, 900);
        assert_eq!(u.model, "claude-sonnet-5");
        // No plan window was reported, so no share is claimed.
        assert_eq!(u.plan_share, 0.0);
    }

    #[tokio::test]
    async fn a_job_held_by_another_runner_is_left_to_it() {
        let store = Arc::new(Store::in_memory().unwrap());
        store
            .job_put(&job(Placement::Anywhere), Utc::now())
            .unwrap();
        let agents = Arc::new(Instant(store.clone(), Mutex::new(vec![])));
        let leases = InMemory::new("laptop");
        // The cloud runner got there first.
        assert!(leases.claim_as("cloud", "job:j1", JOB_LEASE).won);
        let runner = JobRunner::new(
            store.clone(),
            agents.clone(),
            leases,
            RunnerKind::Computer,
            "Laptop",
        );
        runner.take(Utc::now()).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(agents.1.lock().unwrap().is_empty());
        assert_eq!(
            store.job_get("j1").unwrap().unwrap().state,
            JobState::Waiting
        );
    }

    #[tokio::test]
    async fn a_cloud_job_is_not_for_a_computer() {
        let store = Arc::new(Store::in_memory().unwrap());
        store.job_put(&job(Placement::Cloud), Utc::now()).unwrap();
        let agents = Arc::new(Instant(store.clone(), Mutex::new(vec![])));
        let runner = JobRunner::new(
            store.clone(),
            agents.clone(),
            InMemory::new("laptop"),
            RunnerKind::Computer,
            "Laptop",
        );
        runner.take(Utc::now()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(agents.1.lock().unwrap().is_empty());
    }

    #[test]
    fn cloud_jobs_only_name_github_repos() {
        assert_eq!(
            github_repo("acme/api").unwrap(),
            ("acme".to_string(), "api".to_string())
        );
        assert_eq!(
            github_repo("https://github.com/acme/api.git").unwrap(),
            ("acme".to_string(), "api".to_string())
        );
        for bad in [
            "",
            "acme",
            "acme/api/extra",
            "git@evil.com:acme/api",
            "https://evil.com/acme/api",
            "ext::sh -c touch% /tmp/x",
            "file:///etc",
            "-uacme/api",
            "acme/..",
            "acme/api; rm -rf /",
            "acme/{{item.body}}",
        ] {
            assert!(github_repo(bad).is_err(), "{bad:?} was taken");
        }
    }

    #[tokio::test]
    async fn the_runners_git_token_reaches_only_its_own_git_commands() {
        let tmp = std::env::temp_dir();
        let out = git(
            &["config", "--get", "credential.https://github.com.helper"],
            &tmp,
            Some("ghp_secret"),
        )
        .await
        .output()
        .await
        .unwrap();
        let helper = String::from_utf8_lossy(&out.stdout);
        assert!(helper.contains("TAB_GIT_TOKEN"), "{helper}");
        assert!(!helper.contains("ghp_secret"), "the token is in the config");
        let none = git(
            &["config", "--get", "credential.https://github.com.helper"],
            &tmp,
            None,
        )
        .await
        .output()
        .await
        .unwrap();
        assert!(String::from_utf8_lossy(&none.stdout).trim().is_empty());
    }

    #[tokio::test]
    async fn a_checkouts_own_config_cant_run_programs_for_the_runner() {
        let dir =
            std::env::temp_dir().join(format!("tab-job-git-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(run(&dir, &["init", "-q", "."]).await.status.success());
        // What an agent could leave in the checkout's .git/config.
        for (k, v) in [
            ("core.fsmonitor", "touch pwned"),
            ("core.hooksPath", ".evil-hooks"),
            ("core.sshCommand", "touch pwned"),
            ("http.sslVerify", "false"),
        ] {
            assert!(run_cfg(&dir, k, v).await);
        }
        for k in ["core.fsmonitor", "core.sshCommand"] {
            let o = run(&dir, &["config", "--get", k]).await;
            assert!(
                String::from_utf8_lossy(&o.stdout).trim().is_empty(),
                "{k} survived"
            );
        }
        let o = run(&dir, &["config", "--get", "core.hooksPath"]).await;
        assert!(!String::from_utf8_lossy(&o.stdout).contains(".evil-hooks"));
        let o = run(&dir, &["config", "--get", "http.sslVerify"]).await;
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "true");
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn run(dir: &Path, args: &[&str]) -> std::process::Output {
        git(args, dir, None).await.output().await.unwrap()
    }

    /// Writes the checkout's own config, as an agent in it could.
    async fn run_cfg(dir: &Path, k: &str, v: &str) -> bool {
        tokio::process::Command::new("git")
            .args(["config", "--local", k, v])
            .current_dir(dir)
            .status()
            .await
            .is_ok_and(|s| s.success())
    }

    #[test]
    fn titles_are_the_prompts_first_line() {
        assert_eq!(first_line("\n  Fix it\nmore"), "Fix it");
        assert!(first_line(&"x".repeat(80)).ends_with('…'));
    }
}
