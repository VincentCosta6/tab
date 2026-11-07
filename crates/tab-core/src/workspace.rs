//! Local checkouts for the review workspace.
//!
//! Opening a PR clones its repository once (blobless, so only the commits and
//! trees come down up front) and checks the PR's head out into a worktree of
//! its own. That gives the review tool the whole repository: a real file
//! explorer, `git grep` search, and every file's content for language
//! features, without spending API calls.
//!
//! Layout under the app's data directory:
//!
//! ```text
//! workspaces/<owner>/<name>/repo        the shared clone
//! workspaces/<owner>/<name>/pr-<n>      worktree at the PR's head
//! workspaces/<owner>/<name>/pr-<n>.used when the PR was last opened
//! ```
//!
//! Checkouts are removed again once their PR closes or they go unused (see
//! [`Workspaces::checkouts`] and [`Workspaces::remove`]); the shared clone
//! goes with the last of them.
//!
//! The GitHub token is handed to git through `GIT_CONFIG_*` environment
//! variables for each command, so it is never written to `.git/config` or
//! shown in a process list.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

/// Progress for one step of preparing a workspace.
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub repo: String,
    pub number: u64,
    /// `clone`, `fetch`, `checkout`, `index`.
    pub step: String,
    /// `running`, `done`, or `error`.
    pub state: String,
    pub message: String,
    pub percent: Option<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Workspace {
    pub path: String,
    pub head_sha: String,
    /// What the PR's diff is against: the merge base of base and head, like github.com.
    pub merge_base: String,
    /// Every tracked file at the head.
    pub files: Vec<String>,
    /// True when the clone already existed.
    pub reused: bool,
}

/// One PR checkout on disk.
#[derive(Debug, Clone, Serialize)]
pub struct Checkout {
    pub repo: String,
    pub number: u64,
    /// When the PR was last opened for review.
    pub last_used: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Target {
    pub repo: String,
    pub number: u64,
    pub head_sha: String,
    pub base_sha: String,
    pub base_ref: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub whole_word: bool,
    /// Glob limiting which files are searched, like `src/**/*.ts`.
    #[serde(default)]
    pub include: Option<String>,
}

const MAX_HITS: usize = 2000;
/// Files larger than this aren't returned in bulk reads.
const MAX_BULK_FILE: usize = 256 * 1024;
/// One bulk read returns at most this much, so a single IPC reply stays small.
const MAX_BULK_TOTAL: usize = 8 * 1024 * 1024;

pub struct Workspaces {
    root: PathBuf,
}

fn git_exe() -> anyhow::Result<PathBuf> {
    if let Ok(p) = which::which("git") {
        return Ok(p);
    }
    let candidates: &[&str] = if cfg!(windows) {
        &[
            r"C:\Program Files\Git\cmd\git.exe",
            r"C:\Program Files (x86)\Git\cmd\git.exe",
        ]
    } else {
        &[
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
            "/usr/bin/git",
        ]
    };
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow::anyhow!("git isn't installed, so tab can't make a local copy."))
}

fn b64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn valid_repo(repo: &str) -> anyhow::Result<(&str, &str)> {
    let ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    match repo.split_once('/') {
        Some((o, n)) if ok(o) && ok(n) => Ok((o, n)),
        _ => anyhow::bail!("not a repository: {repo}"),
    }
}

fn valid_sha(sha: &str) -> anyhow::Result<&str> {
    if sha.len() >= 7 && sha.len() <= 64 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(sha)
    } else {
        anyhow::bail!("not a commit: {sha}")
    }
}

/// A repo-relative path that can't climb out of the checkout or pass as a flag.
fn valid_path(path: &str) -> anyhow::Result<&str> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.starts_with('-')
        || path.contains('\\')
        || path.contains('\0')
        || path.split('/').any(|seg| seg == ".." || seg.is_empty());
    if bad {
        anyhow::bail!("not a file in the repository: {path}");
    }
    Ok(path)
}

struct Git {
    exe: PathBuf,
    token: Option<String>,
}

impl Git {
    fn cmd(&self, dir: &Path) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.exe);
        #[cfg(windows)]
        {
            cmd.creation_flags(0x0800_0000);
        }
        cmd.current_dir(dir)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never")
            .env("LC_ALL", "C");
        let mut cfg: Vec<(&str, String)> = vec![
            // Long paths on Windows checkouts; harmless elsewhere.
            ("core.longpaths", "true".into()),
        ];
        if let Some(t) = &self.token {
            cfg.push((
                "http.https://github.com/.extraheader",
                format!(
                    "AUTHORIZATION: basic {}",
                    b64(format!("x-access-token:{t}").as_bytes())
                ),
            ));
        }
        cmd.env("GIT_CONFIG_COUNT", cfg.len().to_string());
        for (i, (k, v)) in cfg.iter().enumerate() {
            cmd.env(format!("GIT_CONFIG_KEY_{i}"), k);
            cmd.env(format!("GIT_CONFIG_VALUE_{i}"), v);
        }
        cmd
    }

    async fn run(&self, dir: &Path, args: &[&str]) -> anyhow::Result<String> {
        let out = self
            .cmd(dir)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let last = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("git failed");
            anyhow::bail!("git {}: {}", args.first().unwrap_or(&""), last.trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Runs a long git command, reporting `--progress` output as it goes.
    async fn run_progress(
        &self,
        dir: &Path,
        args: &[&str],
        mut on: impl FnMut(String, Option<u8>),
    ) -> anyhow::Result<()> {
        let mut child = self
            .cmd(dir)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut err = child.stderr.take().expect("piped");
        let mut buf = Vec::new();
        let mut tail = String::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = err.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            for &b in &chunk[..n] {
                if b == b'\r' || b == b'\n' {
                    let line = String::from_utf8_lossy(&buf).trim().to_string();
                    buf.clear();
                    if line.is_empty() {
                        continue;
                    }
                    let line = line.trim_start_matches("remote: ").to_string();
                    on(line.clone(), parse_percent(&line));
                    tail = line;
                } else {
                    buf.push(b);
                }
            }
        }
        let status = child.wait().await?;
        if !status.success() {
            anyhow::bail!(
                "{}",
                if tail.is_empty() {
                    "git failed".to_string()
                } else {
                    tail
                }
            );
        }
        Ok(())
    }
}

/// "Receiving objects:  45% (123/456)" → overall percent across git's phases.
fn parse_percent(line: &str) -> Option<u8> {
    let (phase, rest) = line.split_once(':')?;
    let pct: u8 = rest.trim().split('%').next()?.trim().parse().ok()?;
    // Counting and compressing are quick; receiving is most of a clone.
    let (start, span) = match phase.trim() {
        "Enumerating objects" | "Counting objects" => (0, 5),
        "Compressing objects" => (5, 5),
        "Receiving objects" => (10, 75),
        "Resolving deltas" => (85, 10),
        "Updating files" | "Checking out files" => (95, 5),
        _ => return None,
    };
    Some((start + span * pct as u32 / 100).min(100) as u8)
}

impl Workspaces {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn repo_dir(&self, repo: &str) -> anyhow::Result<PathBuf> {
        let (o, n) = valid_repo(repo)?;
        Ok(self.root.join(o).join(n))
    }

    /// Where a PR's checkout lives, whether or not it exists yet.
    pub fn worktree(&self, repo: &str, number: u64) -> anyhow::Result<PathBuf> {
        Ok(self.repo_dir(repo)?.join(format!("pr-{number}")))
    }

    fn used_marker(&self, repo: &str, number: u64) -> anyhow::Result<PathBuf> {
        Ok(self.repo_dir(repo)?.join(format!("pr-{number}.used")))
    }

    /// Notes that a PR's checkout was just opened, so the sweep leaves it.
    pub fn mark_used(&self, repo: &str, number: u64) {
        if let Ok(p) = self.used_marker(repo, number) {
            let _ = std::fs::create_dir_all(p.parent().unwrap_or(&self.root));
            let _ = std::fs::write(&p, b"");
        }
    }

    /// Every PR checkout on disk, with when it was last opened.
    pub fn checkouts(&self) -> Vec<Checkout> {
        let mut out = Vec::new();
        let dirs = |p: &Path| -> Vec<std::fs::DirEntry> {
            std::fs::read_dir(p)
                .map(|r| r.flatten().filter(|e| e.path().is_dir()).collect())
                .unwrap_or_default()
        };
        let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        for owner in dirs(&self.root) {
            for name in dirs(&owner.path()) {
                for wt in dirs(&name.path()) {
                    let file = wt.file_name();
                    let Some(n) = file
                        .to_str()
                        .and_then(|f| f.strip_prefix("pr-"))
                        .and_then(|n| n.parse::<u64>().ok())
                    else {
                        continue;
                    };
                    let repo = format!(
                        "{}/{}",
                        owner.file_name().to_string_lossy(),
                        name.file_name().to_string_lossy()
                    );
                    if valid_repo(&repo).is_err() {
                        continue;
                    }
                    let used = mtime(&name.path().join(format!("pr-{n}.used")))
                        .or_else(|| mtime(&wt.path()))
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    out.push(Checkout {
                        repo,
                        number: n,
                        last_used: used.into(),
                    });
                }
            }
        }
        out.sort_by(|a, b| (&a.repo, a.number).cmp(&(&b.repo, b.number)));
        out
    }

    /// Removes a PR's checkout, and the repository's clone once no checkout
    /// is left. It comes back the next time the PR is opened.
    pub async fn remove(&self, repo: &str, number: u64) -> anyhow::Result<()> {
        let dir = self.repo_dir(repo)?;
        let clone = dir.join("repo");
        let wt = self.worktree(repo, number)?;
        if let Ok(git) = self.git(None)
            && clone.join(".git").exists()
        {
            let wt_str = wt.to_string_lossy().into_owned();
            let _ = git
                .run(&clone, &["worktree", "remove", "--force", &wt_str])
                .await;
            if wt.exists() {
                tokio::fs::remove_dir_all(&wt).await?;
            }
            let _ = git.run(&clone, &["worktree", "prune"]).await;
        } else if wt.exists() {
            tokio::fs::remove_dir_all(&wt).await?;
        }
        let _ = tokio::fs::remove_file(self.used_marker(repo, number)?).await;

        let others = std::fs::read_dir(&dir)
            .map(|r| {
                r.flatten().any(|e| {
                    e.path().is_dir() && e.file_name().to_string_lossy().starts_with("pr-")
                })
            })
            .unwrap_or(false);
        if !others && dir.exists() {
            tokio::fs::remove_dir_all(&dir).await?;
            if let Some(owner) = dir.parent() {
                // Only goes if empty.
                let _ = tokio::fs::remove_dir(owner).await;
            }
        }
        Ok(())
    }

    fn git(&self, token: Option<String>) -> anyhow::Result<Git> {
        Ok(Git {
            exe: git_exe()?,
            token,
        })
    }

    /// Clones (or refreshes) the repository and checks out the PR's head.
    pub async fn prepare(
        &self,
        t: &Target,
        token: Option<String>,
        on: impl Fn(Progress) + Send + Sync,
    ) -> anyhow::Result<Workspace> {
        let git = self.git(token)?;
        let head = valid_sha(&t.head_sha)?.to_string();
        let base = valid_sha(&t.base_sha)?.to_string();
        let dir = self.repo_dir(&t.repo)?;
        let clone = dir.join("repo");
        let wt = self.worktree(&t.repo, t.number)?;
        let report = |step: &str, state: &str, message: String, percent: Option<u8>| {
            on(Progress {
                repo: t.repo.clone(),
                number: t.number,
                step: step.into(),
                state: state.into(),
                message,
                percent,
            })
        };
        tokio::fs::create_dir_all(&dir).await?;
        self.mark_used(&t.repo, t.number);

        // 1. The shared clone.
        let reused = clone.join(".git").is_dir() || clone.join("HEAD").is_file();
        if reused {
            report(
                "clone",
                "done",
                "Using the copy on this computer".into(),
                Some(100),
            );
        } else {
            report("clone", "running", format!("Cloning {}", t.repo), Some(0));
            let url = format!("https://github.com/{}.git", t.repo);
            let partial = clone.with_extension("partial");
            let _ = tokio::fs::remove_dir_all(&partial).await;
            let target = partial.to_string_lossy().into_owned();
            git.run_progress(
                &dir,
                &[
                    "clone",
                    "--progress",
                    "--filter=blob:none",
                    "--no-checkout",
                    &url,
                    &target,
                ],
                |m, p| report("clone", "running", m, p),
            )
            .await
            .inspect_err(|e| report("clone", "error", e.to_string(), None))?;
            tokio::fs::rename(&partial, &clone).await?;
            report("clone", "done", format!("Cloned {}", t.repo), Some(100));
        }

        // 2. The PR's head (works for forks too) and the base.
        report("fetch", "running", format!("Fetching #{}", t.number), None);
        let pr_ref = format!("+refs/pull/{}/head:refs/tab/pr/{}", t.number, t.number);
        let base_ref = format!(
            "+refs/heads/{}:refs/remotes/origin/{}",
            t.base_ref, t.base_ref
        );
        let has = |sha: String| {
            let git = &git;
            let clone = clone.clone();
            async move {
                git.run(&clone, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
                    .await
                    .is_ok()
            }
        };
        if !(has(head.clone()).await && has(base.clone()).await) {
            let mut args = vec![
                "fetch",
                "--progress",
                "--filter=blob:none",
                "origin",
                &pr_ref,
            ];
            let valid_branch = !t.base_ref.is_empty()
                && !t.base_ref.starts_with('-')
                && !t.base_ref.contains("..")
                && !t.base_ref.contains(':');
            if valid_branch {
                args.push(&base_ref);
            }
            git.run_progress(&clone, &args, |m, p| report("fetch", "running", m, p))
                .await
                .inspect_err(|e| report("fetch", "error", e.to_string(), None))?;
        }
        report("fetch", "done", format!("Fetched #{}", t.number), Some(100));

        // 3. A worktree at the head.
        let short = &head[..head.len().min(7)];
        report("checkout", "running", format!("Checking out {short}"), None);
        let wt_str = wt.to_string_lossy().into_owned();
        let checkout = async {
            if wt.join(".git").exists() {
                git.run(&wt, &["checkout", "--quiet", "--force", "--detach", &head])
                    .await?;
            } else {
                let _ = git.run(&clone, &["worktree", "prune"]).await;
                let _ = tokio::fs::remove_dir_all(&wt).await;
                git.run_progress(
                    &clone,
                    &["worktree", "add", "--force", "--detach", &wt_str, &head],
                    |m, p| report("checkout", "running", m, p),
                )
                .await?;
            }
            anyhow::Ok(())
        };
        checkout
            .await
            .inspect_err(|e| report("checkout", "error", e.to_string(), None))?;
        report(
            "checkout",
            "done",
            format!("Checked out {short}"),
            Some(100),
        );

        // 4. Index: list files and find the merge base.
        report("index", "running", "Indexing files".into(), None);
        let merge_base = git
            .run(&clone, &["merge-base", &base, &head])
            .await
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or(base);
        let files: Vec<String> = git
            .run(&wt, &["ls-files", "-z"])
            .await?
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        report(
            "index",
            "done",
            format!("Indexed {} files", files.len()),
            Some(100),
        );
        Ok(Workspace {
            path: wt_str,
            head_sha: head,
            merge_base,
            files,
            reused,
        })
    }

    /// A file's content at a commit, from the local clone. Missing files read as empty.
    pub async fn read(&self, repo: &str, sha: &str, path: &str) -> anyhow::Result<String> {
        let git = self.git(None)?;
        let clone = self.repo_dir(repo)?.join("repo");
        let spec = format!("{}:{}", valid_sha(sha)?, valid_path(path)?);
        match git.run(&clone, &["cat-file", "-e", &spec]).await {
            Ok(_) => git.run(&clone, &["cat-file", "blob", &spec]).await,
            Err(_) => Ok(String::new()),
        }
    }

    /// Many files at once from the head checkout, for language features.
    /// Skips anything missing, binary, or large.
    pub async fn read_many(
        &self,
        repo: &str,
        number: u64,
        paths: &[String],
    ) -> anyhow::Result<Vec<(String, String)>> {
        let wt = self.worktree(repo, number)?;
        let mut out = Vec::with_capacity(paths.len());
        let mut total = 0usize;
        for p in paths {
            if total >= MAX_BULK_TOTAL {
                break;
            }
            let Ok(p) = valid_path(p) else { continue };
            let full = wt.join(p);
            let Ok(meta) = tokio::fs::metadata(&full).await else {
                continue;
            };
            if !meta.is_file() || meta.len() as usize > MAX_BULK_FILE {
                continue;
            }
            if let Ok(bytes) = tokio::fs::read(&full).await
                && !bytes.contains(&0)
            {
                total += bytes.len();
                out.push((p.to_string(), String::from_utf8_lossy(&bytes).into_owned()));
            }
        }
        Ok(out)
    }

    /// `git grep` over the PR's checkout.
    pub async fn search(
        &self,
        repo: &str,
        number: u64,
        q: &SearchQuery,
    ) -> anyhow::Result<Vec<SearchHit>> {
        if q.query.is_empty() {
            return Ok(Vec::new());
        }
        let git = self.git(None)?;
        let wt = self.worktree(repo, number)?;
        let mut args: Vec<String> = [
            "grep",
            "-n",
            "--column",
            "-I",
            "--no-color",
            "--full-name",
            "-z",
            "--max-count=50",
        ]
        .map(String::from)
        .to_vec();
        args.push(if q.regex { "-E" } else { "-F" }.into());
        if !q.case_sensitive {
            args.push("-i".into());
        }
        if q.whole_word {
            args.push("-w".into());
        }
        args.push("-e".into());
        args.push(q.query.clone());
        args.push("--".into());
        if let Some(glob) = q.include.as_ref().filter(|g| !g.trim().is_empty()) {
            for g in glob.split(',').map(str::trim).filter(|g| !g.is_empty()) {
                args.push(format!(":(glob){g}"));
            }
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.grep(&git, &wt, &refs).await?;
        let mut hits = Vec::new();
        // With -z: path\0line\0column\0text
        for rec in out.lines() {
            let mut parts = rec.splitn(4, '\0');
            let (Some(path), Some(line), Some(col), Some(text)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            hits.push(SearchHit {
                path: path.into(),
                line: line.parse().unwrap_or(0),
                column: col.parse().unwrap_or(1),
                text: text.chars().take(400).collect(),
            });
            if hits.len() >= MAX_HITS {
                break;
            }
        }
        Ok(hits)
    }

    async fn grep(&self, git: &Git, wt: &Path, args: &[&str]) -> anyhow::Result<String> {
        let out = git
            .cmd(wt)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        // Exit 1 means no matches.
        match out.status.code() {
            Some(0) | Some(1) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
            _ => {
                let err = String::from_utf8_lossy(&out.stderr);
                anyhow::bail!(
                    "{}",
                    err.lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("search failed")
                        .trim_start_matches("fatal: ")
                )
            }
        }
    }
}

/// VS Code's command-line launcher, if it's installed.
pub fn vscode() -> Option<PathBuf> {
    if let Ok(p) = which::which("code") {
        return Some(p);
    }
    let mut c: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        c.push("/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code".into());
        if let Ok(home) = std::env::var("HOME") {
            c.push(
                PathBuf::from(home)
                    .join("Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"),
            );
        }
    } else if cfg!(windows) {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            c.push(PathBuf::from(&local).join(r"Programs\Microsoft VS Code\bin\code.cmd"));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            c.push(PathBuf::from(pf).join(r"Microsoft VS Code\bin\code.cmd"));
        }
    } else {
        c.push("/usr/bin/code".into());
        c.push("/snap/bin/code".into());
    }
    c.into_iter().find(|p| p.is_file())
}

/// Opens a checkout in VS Code, optionally at a file and line.
pub fn open_in_vscode(dir: &Path, file: Option<(&str, u32)>) -> anyhow::Result<()> {
    let code = vscode().ok_or_else(|| anyhow::anyhow!("VS Code's `code` command wasn't found."))?;
    let mut cmd = std::process::Command::new(code);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.arg(dir);
    if let Some((path, line)) = file {
        let path = valid_path(path)?;
        cmd.arg("--goto")
            .arg(format!("{}:{line}", dir.join(path).display()));
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        assert_eq!(b64(b"x-access-token:abc"), "eC1hY2Nlc3MtdG9rZW46YWJj");
        assert_eq!(b64(b"a"), "YQ==");
        assert_eq!(b64(b"ab"), "YWI=");
    }

    #[test]
    fn reads_git_progress() {
        assert_eq!(parse_percent("Receiving objects:  40% (4/10)"), Some(40));
        assert_eq!(
            parse_percent("Resolving deltas: 100% (3/3), done."),
            Some(95)
        );
        assert_eq!(parse_percent("Cloning into 'x'..."), None);
    }

    #[test]
    fn refuses_paths_outside_the_checkout() {
        assert!(valid_path("src/main.rs").is_ok());
        assert!(valid_path("../etc/passwd").is_err());
        assert!(valid_path("/etc/passwd").is_err());
        assert!(valid_path("--output=x").is_err());
        assert!(valid_repo("acme/payments").is_ok());
        assert!(valid_repo("../x").is_err());
        assert!(valid_sha("deadbeef").is_ok());
        assert!(valid_sha("HEAD").is_err());
    }

    /// Clones a local repository end to end: clone, worktree, read, search.
    #[tokio::test]
    async fn prepares_a_workspace_from_a_local_remote() {
        let Ok(git) = git_exe() else { return };
        let tmp = std::env::temp_dir().join(format!("tab-ws-{}", uuid::Uuid::new_v4()));
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("lib")).unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let ok = std::process::Command::new(&git)
                .current_dir(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(
                ok.status.success(),
                "{:?}",
                String::from_utf8_lossy(&ok.stderr)
            );
            String::from_utf8_lossy(&ok.stdout).trim().to_string()
        };
        run(&src, &["init", "-q", "-b", "main"]);
        std::fs::write(src.join("lib/a.txt"), "hello\n").unwrap();
        run(&src, &["add", "."]);
        run(&src, &["commit", "-qm", "base"]);
        let base = run(&src, &["rev-parse", "HEAD"]);
        std::fs::write(src.join("lib/a.txt"), "hello world\n").unwrap();
        run(&src, &["commit", "-qam", "head"]);
        let head = run(&src, &["rev-parse", "HEAD"]);
        run(&src, &["update-ref", "refs/pull/7/head", &head]);
        run(&src, &["reset", "-q", "--hard", &base]);

        // Point "github.com/acme/demo" at the local repo.
        let ws = Workspaces::new(tmp.join("ws"));
        let clone = ws.repo_dir("acme/demo").unwrap().join("repo");
        std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
        run(
            &tmp,
            &[
                "clone",
                "-q",
                "--no-checkout",
                src.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        let t = Target {
            repo: "acme/demo".into(),
            number: 7,
            head_sha: head.clone(),
            base_sha: base.clone(),
            base_ref: "main".into(),
        };
        let steps = std::sync::Mutex::new(Vec::new());
        let w = ws
            .prepare(&t, None, |p| steps.lock().unwrap().push(p.step))
            .await
            .unwrap();
        assert!(w.reused);
        assert_eq!(w.merge_base, base);
        assert_eq!(w.files, vec!["lib/a.txt".to_string()]);
        assert_eq!(
            ws.read("acme/demo", &base, "lib/a.txt").await.unwrap(),
            "hello\n"
        );
        assert_eq!(ws.read("acme/demo", &base, "nope.txt").await.unwrap(), "");
        let hits = ws
            .search(
                "acme/demo",
                7,
                &SearchQuery {
                    query: "WORLD".into(),
                    regex: false,
                    case_sensitive: false,
                    whole_word: false,
                    include: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            (hits[0].path.as_str(), hits[0].line, hits[0].column),
            ("lib/a.txt", 1, 7)
        );
        let many = ws
            .read_many("acme/demo", 7, &["lib/a.txt".into(), "../x".into()])
            .await
            .unwrap();
        assert_eq!(many, vec![("lib/a.txt".into(), "hello world\n".into())]);

        let co = ws.checkouts();
        assert_eq!(co.len(), 1);
        assert_eq!((co[0].repo.as_str(), co[0].number), ("acme/demo", 7));
        assert!(chrono::Utc::now() - co[0].last_used < chrono::Duration::minutes(5));
        ws.remove("acme/demo", 7).await.unwrap();
        assert!(ws.checkouts().is_empty());
        assert!(!clone.exists(), "the clone goes with its last checkout");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
