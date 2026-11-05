//! Pull request review data and actions for the GitHub module's review workspace.
//!
//! Everything goes through the gateway. One GraphQL query reads the PR's
//! state, checks, review threads, the viewer's pending review and which files
//! the viewer has marked as viewed; the REST files endpoint adds rename info.
//! File contents come from the local checkout (see [`crate::workspace`]) or,
//! without one, from the contents API by commit SHA, which is cached for good.
//!
//! Review comments behave like github.com: a comment either goes out right
//! away or joins the viewer's pending review, which lives on GitHub (so it
//! shows up on every machine) until it is submitted or discarded. The sample
//! connector's PRs get canned content and are edited locally by the UI.

use std::time::Duration;

use reqwest::Method;
use reqwest::header::{ACCEPT, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::gateway::{Call, Gateway};

const API: &str = "https://api.github.com";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrFile {
    pub filename: String,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    #[serde(default)]
    pub previous_filename: Option<String>,
    /// `viewed`, `unviewed`, or `dismissed` (viewed, then changed since).
    #[serde(default)]
    pub viewed: String,
    /// Filled in only for sample data; real files load on demand by SHA.
    #[serde(default)]
    pub original: Option<String>,
    #[serde(default)]
    pub modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadComment {
    /// GraphQL node id.
    pub id: String,
    /// REST id, used for replies.
    pub database_id: Option<i64>,
    pub author: String,
    pub avatar: Option<String>,
    pub body: String,
    pub created_at: String,
    /// Part of the viewer's unsubmitted review.
    pub pending: bool,
    pub url: Option<String>,
    pub viewer_did_author: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewThread {
    pub id: String,
    pub path: String,
    /// Last line of the range, on `side`. None when the thread is outdated.
    pub line: Option<u32>,
    pub start_line: Option<u32>,
    pub original_line: Option<u32>,
    /// `LEFT` (the base) or `RIGHT` (the head).
    pub side: String,
    pub resolved: bool,
    pub outdated: bool,
    pub can_resolve: bool,
    pub can_reply: bool,
    /// The hunk GitHub stored with the first comment, for outdated threads.
    pub diff_hunk: Option<String>,
    pub comments: Vec<ThreadComment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CheckRun {
    pub name: String,
    /// `queued`, `in_progress`, `completed`, ...
    pub status: String,
    /// `success`, `failure`, `neutral`, `skipped`, `cancelled`, ... when completed.
    pub conclusion: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Checks {
    /// GitHub's rollup: `success`, `failure`, `pending`, `error`, `expected`.
    pub state: Option<String>,
    pub runs: Vec<CheckRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Label {
    pub name: String,
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingReview {
    pub id: String,
    pub body: String,
}

/// A file in the sample repository, so the explorer and search have
/// something to show without a clone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SampleFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PullReview {
    pub repo: String,
    pub number: u64,
    pub node_id: String,
    pub title: String,
    pub body: String,
    pub author: String,
    pub author_avatar: Option<String>,
    pub url: String,
    /// `open`, `closed`, or `merged`.
    pub state: String,
    pub draft: bool,
    pub base_ref: String,
    pub head_ref: String,
    /// `owner/name` of the head repository; differs from `repo` for forks.
    pub head_repo: Option<String>,
    pub base_sha: String,
    pub head_sha: String,
    pub additions: u32,
    pub deletions: u32,
    pub commits: u32,
    pub created_at: String,
    /// `mergeable`, `conflicting`, or `unknown` (GitHub is still computing).
    pub mergeable: String,
    /// `clean`, `blocked`, `behind`, `dirty`, `unstable`, `draft`, `has_hooks`, `unknown`.
    pub merge_state: String,
    /// `approved`, `changes_requested`, `review_required`, or none.
    pub review_decision: Option<String>,
    pub checks: Checks,
    pub labels: Vec<Label>,
    pub reviewers: Vec<String>,
    pub viewer: String,
    pub viewer_can_update: bool,
    pub files: Vec<PrFile>,
    pub threads: Vec<ReviewThread>,
    pub pending_review: Option<PendingReview>,
    #[serde(default)]
    pub sample_tree: Vec<SampleFile>,
    /// True for canned sample data; edits stay local.
    pub sample: bool,
}

fn get(purpose: &str, url: String, ttl: Duration) -> Call {
    Call::get("github", "github", purpose, url).cache(ttl)
}

/// Runs a GraphQL query or mutation and returns its `data`.
async fn graphql(gw: &Gateway, purpose: &str, query: &str, vars: Value) -> anyhow::Result<Value> {
    let resp = gw
        .send(
            Call::post("github", "github", purpose, format!("{API}/graphql"))
                .json(json!({ "query": query, "variables": vars })),
        )
        .await?;
    let mut v: Value = resp.json()?;
    if let Some(errs) = v.get("errors").and_then(Value::as_array)
        && !errs.is_empty()
    {
        let msg = errs
            .iter()
            .filter_map(|e| e.get("message").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("; ");
        anyhow::bail!("GitHub: {msg}");
    }
    Ok(v.get_mut("data").map(Value::take).unwrap_or(Value::Null))
}

const PULL_QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!) {
  viewer { login }
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      id title body url state isDraft merged createdAt
      additions deletions
      commits { totalCount }
      author { login avatarUrl }
      baseRefName headRefName baseRefOid headRefOid
      headRepository { nameWithOwner }
      mergeable mergeStateStatus reviewDecision viewerCanUpdate
      labels(first: 20) { nodes { name color } }
      reviewRequests(first: 20) {
        nodes { requestedReviewer { ... on User { login } ... on Team { name } } }
      }
      files(first: 100) { nodes { path viewerViewedState } }
      reviews(first: 1, states: [PENDING]) { nodes { id body } }
      reviewThreads(first: 100) {
        nodes {
          id isResolved isOutdated path line startLine originalLine diffSide
          viewerCanResolve viewerCanUnresolve viewerCanReply
          comments(first: 100) {
            nodes {
              id databaseId body createdAt url state viewerDidAuthor diffHunk
              author { login avatarUrl }
            }
          }
        }
      }
      commits_last: commits(last: 1) {
        nodes {
          commit {
            statusCheckRollup {
              state
              contexts(first: 100) {
                nodes {
                  __typename
                  ... on CheckRun { name status conclusion detailsUrl }
                  ... on StatusContext { context state targetUrl }
                }
              }
            }
          }
        }
      }
    }
  }
}
"#;

fn split_repo(repo: &str) -> anyhow::Result<(&str, &str)> {
    repo.split_once('/')
        .filter(|(o, n)| !o.is_empty() && !n.is_empty() && !n.contains('/'))
        .ok_or_else(|| anyhow::anyhow!("not a repository: {repo}"))
}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn opt(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

fn num(v: &Value, k: &str) -> Option<u32> {
    v.get(k).and_then(Value::as_u64).map(|n| n as u32)
}

fn nodes<'a>(v: &'a Value, path: &[&str]) -> Vec<&'a Value> {
    let mut cur = v;
    for p in path {
        cur = &cur[*p];
    }
    cur["nodes"]
        .as_array()
        .map(|a| a.iter().filter(|n| !n.is_null()).collect())
        .unwrap_or_default()
}

fn lower(v: &Value, k: &str) -> String {
    s(v, k).to_ascii_lowercase()
}

pub async fn pull(
    gw: &Gateway,
    repo: &str,
    number: u64,
    sample: bool,
) -> anyhow::Result<PullReview> {
    if sample {
        return Ok(sample_review(repo, number));
    }
    let (owner, name) = split_repo(repo)?;
    let data = graphql(
        gw,
        "review: pull request",
        PULL_QUERY,
        json!({ "owner": owner, "name": name, "number": number }),
    )
    .await?;
    let pr = &data["repository"]["pullRequest"];
    if pr.is_null() {
        anyhow::bail!("{repo}#{number} wasn't found, or this GitHub sign-in can't see it.");
    }

    // REST adds rename info and covers PRs with more than 100 files.
    #[derive(Deserialize)]
    struct RestFile {
        filename: String,
        status: String,
        additions: u32,
        deletions: u32,
        #[serde(default)]
        previous_filename: Option<String>,
    }
    let mut rest: Vec<RestFile> = Vec::new();
    for page in 1..=30 {
        let batch: Vec<RestFile> = gw
            .send(
                get(
                    "review: changed files",
                    format!("{API}/repos/{repo}/pulls/{number}/files"),
                    Duration::from_secs(60),
                )
                .query("per_page", "100")
                .query("page", page.to_string()),
            )
            .await?
            .json()?;
        let done = batch.len() < 100;
        rest.extend(batch);
        if done {
            break;
        }
    }
    let viewed: std::collections::HashMap<String, String> = nodes(pr, &["files"])
        .into_iter()
        .map(|f| (s(f, "path"), lower(f, "viewerViewedState")))
        .collect();
    let files = rest
        .into_iter()
        .map(|f| PrFile {
            viewed: viewed
                .get(&f.filename)
                .cloned()
                .unwrap_or_else(|| "unviewed".into()),
            filename: f.filename,
            status: f.status,
            additions: f.additions,
            deletions: f.deletions,
            previous_filename: f.previous_filename,
            original: None,
            modified: None,
        })
        .collect();

    let threads = nodes(pr, &["reviewThreads"])
        .into_iter()
        .map(|t| {
            let comments: Vec<ThreadComment> = nodes(t, &["comments"])
                .into_iter()
                .map(|c| ThreadComment {
                    id: s(c, "id"),
                    database_id: c.get("databaseId").and_then(Value::as_i64),
                    author: c["author"]
                        .get("login")
                        .and_then(Value::as_str)
                        .unwrap_or("ghost")
                        .into(),
                    avatar: opt(&c["author"], "avatarUrl"),
                    body: s(c, "body"),
                    created_at: s(c, "createdAt"),
                    pending: s(c, "state") == "PENDING",
                    url: opt(c, "url"),
                    viewer_did_author: c["viewerDidAuthor"].as_bool().unwrap_or(false),
                })
                .collect();
            let first = nodes(t, &["comments"]).into_iter().next();
            ReviewThread {
                id: s(t, "id"),
                path: s(t, "path"),
                line: num(t, "line"),
                start_line: num(t, "startLine"),
                original_line: num(t, "originalLine"),
                side: opt(t, "diffSide").unwrap_or_else(|| "RIGHT".into()),
                resolved: t["isResolved"].as_bool().unwrap_or(false),
                outdated: t["isOutdated"].as_bool().unwrap_or(false),
                can_resolve: t["viewerCanResolve"].as_bool().unwrap_or(false)
                    || t["viewerCanUnresolve"].as_bool().unwrap_or(false),
                can_reply: t["viewerCanReply"].as_bool().unwrap_or(true),
                diff_hunk: first.and_then(|c| opt(c, "diffHunk")),
                comments,
            }
        })
        .collect();

    let rollup = &pr["commits_last"]["nodes"][0]["commit"]["statusCheckRollup"];
    let runs = nodes(rollup, &["contexts"])
        .into_iter()
        .map(|c| {
            if s(c, "__typename") == "StatusContext" {
                // Commit statuses have one state; map it onto check-run terms.
                let st = lower(c, "state");
                let (status, conclusion) = match st.as_str() {
                    "pending" | "expected" => ("in_progress", None),
                    "success" => ("completed", Some("success")),
                    _ => ("completed", Some("failure")),
                };
                CheckRun {
                    name: s(c, "context"),
                    status: status.into(),
                    conclusion: conclusion.map(Into::into),
                    url: opt(c, "targetUrl"),
                }
            } else {
                CheckRun {
                    name: s(c, "name"),
                    status: lower(c, "status"),
                    conclusion: opt(c, "conclusion").map(|c| c.to_ascii_lowercase()),
                    url: opt(c, "detailsUrl"),
                }
            }
        })
        .collect();

    let state = if pr["merged"].as_bool().unwrap_or(false) {
        "merged".to_string()
    } else {
        lower(pr, "state")
    };
    let pending = nodes(pr, &["reviews"])
        .into_iter()
        .next()
        .map(|r| PendingReview {
            id: s(r, "id"),
            body: s(r, "body"),
        });

    Ok(PullReview {
        repo: repo.into(),
        number,
        node_id: s(pr, "id"),
        title: s(pr, "title"),
        body: s(pr, "body"),
        author: pr["author"]
            .get("login")
            .and_then(Value::as_str)
            .unwrap_or("ghost")
            .into(),
        author_avatar: opt(&pr["author"], "avatarUrl"),
        url: s(pr, "url"),
        state,
        draft: pr["isDraft"].as_bool().unwrap_or(false),
        base_ref: s(pr, "baseRefName"),
        head_ref: s(pr, "headRefName"),
        head_repo: opt(&pr["headRepository"], "nameWithOwner"),
        base_sha: s(pr, "baseRefOid"),
        head_sha: s(pr, "headRefOid"),
        additions: num(pr, "additions").unwrap_or(0),
        deletions: num(pr, "deletions").unwrap_or(0),
        commits: num(&pr["commits"], "totalCount").unwrap_or(0),
        created_at: s(pr, "createdAt"),
        mergeable: lower(pr, "mergeable"),
        merge_state: lower(pr, "mergeStateStatus"),
        review_decision: opt(pr, "reviewDecision").map(|d| d.to_ascii_lowercase()),
        checks: Checks {
            state: opt(rollup, "state").map(|s| s.to_ascii_lowercase()),
            runs,
        },
        labels: nodes(pr, &["labels"])
            .into_iter()
            .map(|l| Label {
                name: s(l, "name"),
                color: s(l, "color"),
            })
            .collect(),
        reviewers: nodes(pr, &["reviewRequests"])
            .into_iter()
            .filter_map(|r| {
                let rr = &r["requestedReviewer"];
                opt(rr, "login").or_else(|| opt(rr, "name"))
            })
            .collect(),
        viewer: s(&data["viewer"], "login"),
        viewer_can_update: pr["viewerCanUpdate"].as_bool().unwrap_or(false),
        files,
        threads,
        pending_review: pending,
        sample_tree: Vec::new(),
        sample: false,
    })
}

/// A file's content at a commit, through the contents API. Content at a SHA
/// never changes, so it is cached for a month and costs one request ever.
/// Used when there is no local checkout.
pub async fn file_at(gw: &Gateway, repo: &str, sha: &str, path: &str) -> anyhow::Result<String> {
    let call = get(
        "review: file contents",
        format!("{API}/repos/{repo}/contents/{path}"),
        Duration::from_secs(30 * 24 * 3600),
    )
    .query("ref", sha)
    .header(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github.raw+json"),
    );
    match gw.send(call).await {
        Ok(r) => Ok(r.body),
        // Added or deleted files have no content on one side.
        Err(crate::gateway::GatewayError::Http { status: 404, .. }) => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

/// What the UI knows about the PR that every action needs.
#[derive(Debug, Clone, Deserialize)]
pub struct PrRef {
    pub repo: String,
    pub number: u64,
    pub node_id: String,
    pub head_sha: String,
    #[serde(default)]
    pub pending_review_id: Option<String>,
}

/// Everything the review workspace can do to a PR.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReviewAction {
    /// Marks a file viewed (or not) on GitHub, like the checkbox on github.com.
    MarkViewed {
        path: String,
        viewed: bool,
    },
    /// Marks several files at once, like a whole folder.
    MarkViewedMany {
        paths: Vec<String>,
        viewed: bool,
    },
    /// A new comment thread on a line or range. `pending` adds it to the
    /// viewer's review instead of posting it now.
    AddThread {
        path: String,
        line: u32,
        #[serde(default)]
        start_line: Option<u32>,
        side: String,
        body: String,
        pending: bool,
    },
    Reply {
        thread_id: String,
        comment_id: Option<i64>,
        body: String,
        pending: bool,
    },
    EditComment {
        comment_id: String,
        body: String,
    },
    DeleteComment {
        comment_id: String,
    },
    Resolve {
        thread_id: String,
        resolved: bool,
    },
    /// Submits the pending review (or a review with no line comments).
    /// `event` is `comment`, `approve`, or `request_changes`.
    Submit {
        event: String,
        body: String,
    },
    /// Deletes the pending review and its comments.
    Discard,
    /// `open` or `closed`.
    SetState {
        state: String,
    },
    SetDraft {
        draft: bool,
    },
    /// `merge`, `squash`, or `rebase`.
    Merge {
        method: String,
    },
    UpdateBranch,
}

async fn rest(
    gw: &Gateway,
    method: Method,
    purpose: &str,
    path: String,
    body: Value,
) -> anyhow::Result<Value> {
    let r = gw
        .send(Call::new(method, "github", "github", purpose, format!("{API}{path}")).json(body))
        .await?;
    Ok(if r.body.trim().is_empty() {
        Value::Null
    } else {
        r.json().unwrap_or(Value::Null)
    })
}

/// The viewer's pending review, creating one when there is none yet.
async fn ensure_pending(gw: &Gateway, pr: &PrRef) -> anyhow::Result<String> {
    if let Some(id) = pr.pending_review_id.as_ref().filter(|s| !s.is_empty()) {
        return Ok(id.clone());
    }
    let d = graphql(
        gw,
        "review: start review",
        "mutation($pr: ID!, $sha: GitObjectID) { addPullRequestReview(input: { pullRequestId: $pr, commitOID: $sha }) { pullRequestReview { id } } }",
        json!({ "pr": pr.node_id, "sha": pr.head_sha }),
    )
    .await?;
    let id = s(&d["addPullRequestReview"]["pullRequestReview"], "id");
    if id.is_empty() {
        anyhow::bail!("GitHub didn't start a review.");
    }
    Ok(id)
}

pub async fn act(gw: &Gateway, pr: &PrRef, action: ReviewAction) -> anyhow::Result<()> {
    let base = format!("/repos/{}/pulls/{}", pr.repo, pr.number);
    match action {
        ReviewAction::MarkViewed { path, viewed } => {
            let m = if viewed {
                "markFileAsViewed"
            } else {
                "unmarkFileAsViewed"
            };
            graphql(
                gw,
                "review: mark viewed",
                &format!("mutation($pr: ID!, $path: String!) {{ {m}(input: {{ pullRequestId: $pr, path: $path }}) {{ clientMutationId }} }}"),
                json!({ "pr": pr.node_id, "path": path }),
            )
            .await?;
        }
        ReviewAction::MarkViewedMany { paths, viewed } => {
            let m = if viewed {
                "markFileAsViewed"
            } else {
                "unmarkFileAsViewed"
            };
            // Aliased mutations: one request per 25 files.
            for chunk in paths.chunks(25) {
                let vars: Vec<String> = (0..chunk.len())
                    .map(|i| format!("$p{i}: String!"))
                    .collect();
                let body: Vec<String> = (0..chunk.len())
                    .map(|i| format!("f{i}: {m}(input: {{ pullRequestId: $pr, path: $p{i} }}) {{ clientMutationId }}"))
                    .collect();
                let mut v = json!({ "pr": pr.node_id });
                for (i, p) in chunk.iter().enumerate() {
                    v[format!("p{i}")] = json!(p);
                }
                graphql(
                    gw,
                    "review: mark folder viewed",
                    &format!(
                        "mutation($pr: ID!, {}) {{ {} }}",
                        vars.join(", "),
                        body.join(" ")
                    ),
                    v,
                )
                .await?;
            }
        }
        ReviewAction::AddThread {
            path,
            line,
            start_line,
            side,
            body,
            pending,
        } => {
            let start_line = start_line.filter(|s| *s < line);
            if pending {
                let review = ensure_pending(gw, pr).await?;
                graphql(
                    gw,
                    "review: draft comment",
                    "mutation($review: ID!, $path: String!, $body: String!, $line: Int!, $side: DiffSide, $start: Int, $startSide: DiffSide) {
                       addPullRequestReviewThread(input: { pullRequestReviewId: $review, path: $path, body: $body, line: $line, side: $side, startLine: $start, startSide: $startSide }) { thread { id } }
                     }",
                    json!({
                        "review": review, "path": path, "body": body, "line": line, "side": side,
                        "start": start_line, "startSide": start_line.map(|_| side.clone()),
                    }),
                )
                .await?;
            } else {
                let mut b = json!({
                    "body": body, "commit_id": pr.head_sha, "path": path, "line": line, "side": side,
                });
                if let Some(start) = start_line {
                    b["start_line"] = json!(start);
                    b["start_side"] = json!(side);
                }
                rest(
                    gw,
                    Method::POST,
                    "review: comment",
                    format!("{base}/comments"),
                    b,
                )
                .await?;
            }
        }
        ReviewAction::Reply {
            thread_id,
            comment_id,
            body,
            pending,
        } => {
            if pending || comment_id.is_none() {
                let review = ensure_pending(gw, pr).await?;
                graphql(
                    gw,
                    "review: draft reply",
                    "mutation($thread: ID!, $review: ID, $body: String!) {
                       addPullRequestReviewThreadReply(input: { pullRequestReviewThreadId: $thread, pullRequestReviewId: $review, body: $body }) { comment { id } }
                     }",
                    json!({ "thread": thread_id, "review": review, "body": body }),
                )
                .await?;
            } else {
                rest(
                    gw,
                    Method::POST,
                    "review: reply",
                    format!("{base}/comments/{}/replies", comment_id.unwrap_or_default()),
                    json!({ "body": body }),
                )
                .await?;
            }
        }
        ReviewAction::EditComment { comment_id, body } => {
            graphql(
                gw,
                "review: edit comment",
                "mutation($id: ID!, $body: String!) { updatePullRequestReviewComment(input: { pullRequestReviewCommentId: $id, body: $body }) { clientMutationId } }",
                json!({ "id": comment_id, "body": body }),
            )
            .await?;
        }
        ReviewAction::DeleteComment { comment_id } => {
            graphql(
                gw,
                "review: delete comment",
                "mutation($id: ID!) { deletePullRequestReviewComment(input: { id: $id }) { clientMutationId } }",
                json!({ "id": comment_id }),
            )
            .await?;
        }
        ReviewAction::Resolve {
            thread_id,
            resolved,
        } => {
            let m = if resolved {
                "resolveReviewThread"
            } else {
                "unresolveReviewThread"
            };
            graphql(
                gw,
                "review: resolve thread",
                &format!("mutation($id: ID!) {{ {m}(input: {{ threadId: $id }}) {{ clientMutationId }} }}"),
                json!({ "id": thread_id }),
            )
            .await?;
        }
        ReviewAction::Submit { event, body } => {
            let event = event.to_ascii_uppercase();
            match pr.pending_review_id.as_ref().filter(|s| !s.is_empty()) {
                Some(id) => {
                    graphql(
                        gw,
                        "review: submit",
                        "mutation($id: ID!, $event: PullRequestReviewEvent!, $body: String) { submitPullRequestReview(input: { pullRequestReviewId: $id, event: $event, body: $body }) { clientMutationId } }",
                        json!({ "id": id, "event": event, "body": body }),
                    )
                    .await?;
                }
                None => {
                    rest(
                        gw,
                        Method::POST,
                        "review: submit",
                        format!("{base}/reviews"),
                        json!({ "event": event, "body": body, "commit_id": pr.head_sha }),
                    )
                    .await?;
                }
            }
        }
        ReviewAction::Discard => {
            if let Some(id) = pr.pending_review_id.as_ref().filter(|s| !s.is_empty()) {
                graphql(
                    gw,
                    "review: discard",
                    "mutation($id: ID!) { deletePullRequestReview(input: { pullRequestReviewId: $id }) { clientMutationId } }",
                    json!({ "id": id }),
                )
                .await?;
            }
        }
        ReviewAction::SetState { state } => {
            if state != "open" && state != "closed" {
                anyhow::bail!("unknown state {state}");
            }
            rest(
                gw,
                Method::PATCH,
                "review: set state",
                base,
                json!({ "state": state }),
            )
            .await?;
        }
        ReviewAction::SetDraft { draft } => {
            let m = if draft {
                "convertPullRequestToDraft"
            } else {
                "markPullRequestReadyForReview"
            };
            graphql(
                gw,
                "review: draft state",
                &format!("mutation($id: ID!) {{ {m}(input: {{ pullRequestId: $id }}) {{ clientMutationId }} }}"),
                json!({ "id": pr.node_id }),
            )
            .await?;
        }
        ReviewAction::Merge { method } => {
            if !["merge", "squash", "rebase"].contains(&method.as_str()) {
                anyhow::bail!("unknown merge method {method}");
            }
            rest(
                gw,
                Method::PUT,
                "review: merge",
                format!("{base}/merge"),
                json!({ "merge_method": method, "sha": pr.head_sha }),
            )
            .await?;
        }
        ReviewAction::UpdateBranch => {
            rest(
                gw,
                Method::PUT,
                "review: update branch",
                format!("{base}/update-branch"),
                json!({ "expected_head_sha": pr.head_sha }),
            )
            .await?;
        }
    }
    Ok(())
}

fn sample_review(repo: &str, number: u64) -> PullReview {
    let retry_before = r#"import { Charge } from "./types";
import { stripe } from "./clients";

export async function capture(charge: Charge) {
  const result = await stripe.charges.capture(charge.id);
  if (result.status !== "succeeded") {
    throw new Error(`capture failed: ${result.status}`);
  }
  return result;
}
"#;
    let retry_after = r#"import { Charge } from "./types";
import { stripe } from "./clients";
import { backoff } from "./backoff";

const MAX_ATTEMPTS = 4;

export async function capture(charge: Charge) {
  for (let attempt = 1; ; attempt++) {
    try {
      const result = await stripe.charges.capture(charge.id, {
        idempotencyKey: `capture-${charge.id}`,
      });
      if (result.status !== "succeeded") {
        throw new Error(`capture failed: ${result.status}`);
      }
      return result;
    } catch (err) {
      if (attempt >= MAX_ATTEMPTS || !isRetryable(err)) throw err;
      await backoff(attempt);
    }
  }
}

function isRetryable(err: unknown) {
  return err instanceof Error && /timeout|ECONNRESET|rate limit/i.test(err.message);
}
"#;
    let backoff = r#"/** Exponential backoff with full jitter, capped at 8 seconds. */
export function backoff(attempt: number) {
  const ceiling = Math.min(8000, 250 * 2 ** attempt);
  return new Promise((resolve) => setTimeout(resolve, Math.random() * ceiling));
}
"#;
    let test_before = r#"import { capture } from "../src/capture";

test("captures a charge", async () => {
  await expect(capture(fixture("ch_ok"))).resolves.toMatchObject({ status: "succeeded" });
});
"#;
    let test_after = r#"import { capture } from "../src/capture";

test("captures a charge", async () => {
  await expect(capture(fixture("ch_ok"))).resolves.toMatchObject({ status: "succeeded" });
});

test("retries timeouts with the same idempotency key", async () => {
  const calls = mockStripe(["timeout", "timeout", "succeeded"]);
  await capture(fixture("ch_flaky"));
  expect(calls.map((c) => c.idempotencyKey)).toEqual(Array(3).fill("capture-ch_flaky"));
});
"#;
    let f = |name: &str, status: &str, a: u32, d: u32, o: &str, m: &str| PrFile {
        filename: name.into(),
        status: status.into(),
        additions: a,
        deletions: d,
        previous_filename: None,
        viewed: "unviewed".into(),
        original: Some(o.into()),
        modified: Some(m.into()),
    };
    let tree = |path: &str, content: &str| SampleFile {
        path: path.into(),
        content: content.into(),
    };
    PullReview {
        repo: repo.into(),
        number,
        node_id: "sample".into(),
        title: "Retry payment capture with idempotency keys".into(),
        body: "Captures sometimes time out on Stripe's side and we drop the charge.\n\nThis retries timeouts and connection resets up to four times with jittered backoff, and sends an idempotency key so a retry can't double-capture.\n\n- `capture()` retries transient errors\n- new `backoff()` helper\n- test for the retry path".into(),
        author: "dana-k".into(),
        author_avatar: None,
        url: format!("https://github.com/{repo}/pull/{number}"),
        state: "open".into(),
        draft: false,
        base_ref: "main".into(),
        head_ref: "dana/capture-retries".into(),
        head_repo: Some(repo.into()),
        base_sha: "sample-base".into(),
        head_sha: "sample-head".into(),
        additions: 29,
        deletions: 4,
        commits: 3,
        created_at: "2026-11-04T15:02:00Z".into(),
        mergeable: "mergeable".into(),
        merge_state: "unstable".into(),
        review_decision: Some("review_required".into()),
        checks: Checks {
            state: Some("failure".into()),
            runs: vec![
                CheckRun {
                    name: "lint".into(),
                    status: "completed".into(),
                    conclusion: Some("success".into()),
                    url: None,
                },
                CheckRun {
                    name: "unit-tests (node 22)".into(),
                    status: "completed".into(),
                    conclusion: Some("failure".into()),
                    url: None,
                },
                CheckRun {
                    name: "build".into(),
                    status: "completed".into(),
                    conclusion: Some("success".into()),
                    url: None,
                },
            ],
        },
        labels: vec![Label {
            name: "payments".into(),
            color: "0e8a16".into(),
        }],
        reviewers: vec!["you".into()],
        viewer: "you".into(),
        viewer_can_update: true,
        files: vec![
            f("src/capture.ts", "modified", 18, 4, retry_before, retry_after),
            f("src/backoff.ts", "added", 5, 0, "", backoff),
            f("test/capture.test.ts", "modified", 6, 0, test_before, test_after),
        ],
        threads: vec![ReviewThread {
            id: "sample-thread-1".into(),
            path: "src/capture.ts".into(),
            line: Some(19),
            start_line: None,
            original_line: Some(19),
            side: "RIGHT".into(),
            resolved: false,
            outdated: false,
            can_resolve: true,
            can_reply: true,
            diff_hunk: None,
            comments: vec![ThreadComment {
                id: "sample-comment-1".into(),
                database_id: Some(1),
                author: "dana-k".into(),
                avatar: None,
                body: "Should `card_declined` ever be retried? I think this regex is enough but worth a second look.".into(),
                created_at: "2026-11-04T17:20:00Z".into(),
                pending: false,
                url: None,
                viewer_did_author: false,
            }],
        }],
        pending_review: None,
        sample_tree: vec![
            tree("README.md", "# payments\n\nCharges, captures and refunds for acme.\n"),
            tree("package.json", "{\n  \"name\": \"@acme/payments\",\n  \"private\": true,\n  \"scripts\": { \"test\": \"vitest\" }\n}\n"),
            tree("src/types.ts", "export interface Charge {\n  id: string;\n  amount: number;\n  currency: string;\n}\n"),
            tree("src/clients.ts", "import Stripe from \"stripe\";\n\nexport const stripe = new Stripe(process.env.STRIPE_KEY!);\n"),
            tree("src/refund.ts", "import { Charge } from \"./types\";\nimport { stripe } from \"./clients\";\n\nexport async function refund(charge: Charge) {\n  return stripe.refunds.create({ charge: charge.id });\n}\n"),
        ],
        sample: true,
    }
}
