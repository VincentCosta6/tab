//! Pull request review data for the GitHub module's built-in editor.
//!
//! Everything goes through the gateway: PR metadata is cached briefly, file
//! contents are cached by commit SHA (immutable, so effectively forever), and
//! posting a comment is audited like any other call. The sample connector's
//! PRs get canned content so the review tool can be tried without a token.

use std::time::Duration;

use reqwest::header::{ACCEPT, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::json;

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
    /// Filled in only for sample data; real files load on demand by SHA.
    #[serde(default)]
    pub original: Option<String>,
    #[serde(default)]
    pub modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewComment {
    pub id: i64,
    pub path: String,
    pub line: Option<u32>,
    pub side: Option<String>,
    pub body: String,
    pub user: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PullReview {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub url: String,
    pub base_sha: String,
    pub head_sha: String,
    pub files: Vec<PrFile>,
    pub comments: Vec<ReviewComment>,
    /// True for canned sample data; comments are kept locally.
    pub sample: bool,
}

fn get(purpose: &str, url: String, ttl: Duration) -> Call {
    Call::get("github", "github", purpose, url).cache(ttl)
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
    #[derive(Deserialize)]
    struct User {
        login: String,
    }
    #[derive(Deserialize)]
    struct Ref {
        sha: String,
    }
    #[derive(Deserialize)]
    struct Pr {
        title: String,
        user: User,
        html_url: String,
        base: Ref,
        head: Ref,
    }
    #[derive(Deserialize)]
    struct Comment {
        id: i64,
        path: String,
        line: Option<u32>,
        side: Option<String>,
        body: String,
        user: User,
        created_at: String,
    }
    let pr: Pr = gw
        .send(get(
            "review: pull request",
            format!("{API}/repos/{repo}/pulls/{number}"),
            Duration::from_secs(60),
        ))
        .await?
        .json()?;
    let files: Vec<PrFile> = gw
        .send(
            get(
                "review: changed files",
                format!("{API}/repos/{repo}/pulls/{number}/files"),
                Duration::from_secs(60),
            )
            .query("per_page", "100"),
        )
        .await?
        .json()?;
    let comments: Vec<Comment> = gw
        .send(
            get(
                "review: comments",
                format!("{API}/repos/{repo}/pulls/{number}/comments"),
                Duration::from_secs(30),
            )
            .query("per_page", "100"),
        )
        .await?
        .json()?;
    Ok(PullReview {
        repo: repo.into(),
        number,
        title: pr.title,
        author: pr.user.login,
        url: pr.html_url,
        base_sha: pr.base.sha,
        head_sha: pr.head.sha,
        files,
        comments: comments
            .into_iter()
            .map(|c| ReviewComment {
                id: c.id,
                path: c.path,
                line: c.line,
                side: c.side,
                body: c.body,
                user: c.user.login,
                created_at: c.created_at,
            })
            .collect(),
        sample: false,
    })
}

/// A file's content at a commit. Content at a SHA never changes, so it is
/// cached for a month and costs one request per file ever.
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

pub struct NewComment<'a> {
    pub repo: &'a str,
    pub number: u64,
    pub commit_id: &'a str,
    pub path: &'a str,
    pub line: u32,
    pub side: &'a str,
    pub body: &'a str,
}

pub async fn comment(gw: &Gateway, c: NewComment<'_>) -> anyhow::Result<()> {
    gw.send(
        Call::post(
            "github",
            "github",
            "review: comment",
            format!("{API}/repos/{}/pulls/{}/comments", c.repo, c.number),
        )
        .json(json!({
            "body": c.body,
            "commit_id": c.commit_id,
            "path": c.path,
            "line": c.line,
            "side": c.side,
        })),
    )
    .await?;
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
        original: Some(o.into()),
        modified: Some(m.into()),
    };
    PullReview {
        repo: repo.into(),
        number,
        title: "Retry payment capture with idempotency keys".into(),
        author: "dana-k".into(),
        url: format!("https://github.com/{repo}/pull/{number}"),
        base_sha: "sample-base".into(),
        head_sha: "sample-head".into(),
        files: vec![
            f("src/capture.ts", "modified", 18, 4, retry_before, retry_after),
            f("src/backoff.ts", "added", 5, 0, "", backoff),
            f("test/capture.test.ts", "modified", 6, 0, test_before, test_after),
        ],
        comments: vec![ReviewComment {
            id: 1,
            path: "src/capture.ts".into(),
            line: Some(18),
            side: Some("RIGHT".into()),
            body: "Should card_declined ever be retried? I think this regex is enough but worth a second look.".into(),
            user: "dana-k".into(),
            created_at: "2026-11-04T17:20:00Z".into(),
        }],
        sample: true,
    }
}
