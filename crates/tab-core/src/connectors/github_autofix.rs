//! A pull request's problems for autofix: failing checks, unresolved review
//! threads, changes-requested reviews and merge conflicts, with the `gh`
//! commands an agent uses to act on them.

use serde_json::{Value, json};

use crate::autofix::{FixKind, FixProblem, FixReport};
use crate::connectors::github_review::{graphql, split_repo};
use crate::gateway::Gateway;

const FIX_QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!) {
  viewer { login }
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      title state merged mergeable headRefOid
      reviewThreads(first: 100) {
        nodes {
          id isResolved isOutdated path line
          comments(last: 10) { nodes { databaseId author { login } body url } }
        }
      }
      reviews(last: 20, states: CHANGES_REQUESTED) {
        nodes { databaseId author { login } body url }
      }
      commits(last: 1) {
        nodes { commit { statusCheckRollup { contexts(first: 100) { nodes {
          __typename
          ... on CheckRun {
            name conclusion status detailsUrl title summary
            checkSuite { workflowRun { databaseId } }
          }
          ... on StatusContext { context state targetUrl description }
        } } } } }
      }
    }
  }
}"#;

/// How to act on a PR's problems with `gh`, for the agent.
fn guide(repo: &str) -> String {
    format!(
        "Use `gh` (it's signed in).\n\
         - A failed GitHub Actions check's log: `gh run view <run id> --log-failed -R {repo}`. Re-run its failed jobs: `gh run rerun <run id> --failed -R {repo}`.\n\
         - Reply on a review thread: `gh api graphql -f query='mutation($id:ID!,$body:String!){{addPullRequestReviewThreadReply(input:{{pullRequestReviewThreadId:$id,body:$body}}){{comment{{url}}}}}}' -f id=<thread id> -f body='…'`\n\
         - Resolve a thread (only after the fix is pushed): `gh api graphql -f query='mutation($id:ID!){{resolveReviewThread(input:{{threadId:$id}}){{thread{{isResolved}}}}}}' -f id=<thread id>`\n\
         - Answer a review: `gh pr comment <number> -R {repo} --body '…'`."
    )
}

pub async fn fix_report(gw: &Gateway, repo: &str, n: u64) -> anyhow::Result<FixReport> {
    let (owner, name) = split_repo(repo)?;
    let data = graphql(
        gw,
        "autofix: pull request",
        FIX_QUERY,
        json!({ "owner": owner, "name": name, "number": n }),
    )
    .await?;
    let pr = &data["repository"]["pullRequest"];
    anyhow::ensure!(!pr.is_null(), "{repo}#{n} wasn't found");
    Ok(report(repo, n, data["viewer"]["login"].as_str(), pr))
}

fn str_of<'a>(v: &'a Value, p: &str) -> Option<&'a str> {
    v.pointer(p).and_then(Value::as_str)
}

fn list<'a>(v: &'a Value, p: &str) -> &'a [Value] {
    v.pointer(p)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// The report for one PR's GraphQL answer. `viewer` is who tab signs in as:
/// their own replies don't make a thread new again.
fn report(repo: &str, n: u64, viewer: Option<&str>, pr: &Value) -> FixReport {
    let head = str_of(pr, "/headRefOid").map(str::to_string);
    let short = head.as_deref().map(|h| &h[..h.len().min(12)]).unwrap_or("");
    let mut problems = Vec::new();
    let mut pending = false;

    if pr["mergeable"] == "CONFLICTING" {
        problems.push(FixProblem {
            kind: FixKind::Conflict,
            id: format!("conflict:{short}"),
            title: "The branch has merge conflicts with its base".into(),
            detail: None,
            url: Some(format!("https://github.com/{repo}/pull/{n}")),
            handle: None,
        });
    }

    for c in list(
        pr,
        "/commits/nodes/0/commit/statusCheckRollup/contexts/nodes",
    ) {
        let (name, failed, running, url, detail, run) = if c["__typename"] == "CheckRun" {
            let conclusion = c["conclusion"].as_str().unwrap_or("");
            (
                str_of(c, "/name").unwrap_or("check"),
                matches!(
                    conclusion,
                    "FAILURE" | "TIMED_OUT" | "CANCELLED" | "STARTUP_FAILURE" | "ACTION_REQUIRED"
                ),
                c["status"] != "COMPLETED",
                str_of(c, "/detailsUrl"),
                [str_of(c, "/title"), str_of(c, "/summary")]
                    .into_iter()
                    .flatten()
                    .filter(|s| !s.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n"),
                c.pointer("/checkSuite/workflowRun/databaseId")
                    .and_then(Value::as_u64),
            )
        } else {
            let state = c["state"].as_str().unwrap_or("");
            (
                str_of(c, "/context").unwrap_or("status"),
                matches!(state, "FAILURE" | "ERROR"),
                matches!(state, "PENDING" | "EXPECTED"),
                str_of(c, "/targetUrl"),
                str_of(c, "/description").unwrap_or("").to_string(),
                None,
            )
        };
        pending |= running;
        if failed {
            problems.push(FixProblem {
                kind: FixKind::Check,
                // The same check failing on a new commit is a new problem.
                id: format!("check:{name}:{short}"),
                title: name.to_string(),
                detail: (!detail.is_empty()).then_some(detail),
                url: url.map(str::to_string),
                handle: run.map(|r| format!("run {r}")),
            });
        }
    }

    for t in list(pr, "/reviewThreads/nodes") {
        if t["isResolved"].as_bool() != Some(false) {
            continue;
        }
        let comments = list(t, "/comments/nodes");
        let author = |c: &Value| str_of(c, "/author/login").map(str::to_string);
        // New again only when someone other than tab's own account writes.
        let last_other = comments
            .iter()
            .rev()
            .find(|c| viewer.is_none() || author(c).as_deref() != viewer)
            .or(comments.last());
        let Some(id) = str_of(t, "/id") else { continue };
        let mark = last_other
            .and_then(|c| c["databaseId"].as_u64())
            .map(|d| d.to_string())
            .unwrap_or_default();
        let place = match (str_of(t, "/path"), t["line"].as_u64()) {
            (Some(p), Some(l)) => format!("{p}:{l}"),
            (Some(p), None) => p.to_string(),
            _ => "Review thread".into(),
        };
        let detail = comments
            .iter()
            .map(|c| {
                format!(
                    "{}: {}",
                    author(c).unwrap_or_else(|| "someone".into()),
                    c["body"].as_str().unwrap_or("").trim()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        problems.push(FixProblem {
            kind: FixKind::Thread,
            id: format!("thread:{id}:{mark}"),
            title: if t["isOutdated"].as_bool() == Some(true) {
                format!("{place} (outdated)")
            } else {
                place
            },
            detail: (!detail.is_empty()).then_some(detail),
            url: comments
                .first()
                .and_then(|c| str_of(c, "/url"))
                .map(str::to_string),
            handle: Some(format!("thread {id}")),
        });
    }

    for r in list(pr, "/reviews/nodes") {
        let body = r["body"].as_str().unwrap_or("").trim();
        let Some(id) = r["databaseId"].as_u64() else {
            continue;
        };
        if body.is_empty() {
            continue;
        }
        problems.push(FixProblem {
            kind: FixKind::Thread,
            id: format!("review:{id}"),
            title: format!(
                "Changes requested by {}",
                str_of(r, "/author/login").unwrap_or("a reviewer")
            ),
            detail: Some(body.to_string()),
            url: str_of(r, "/url").map(str::to_string),
            handle: None,
        });
    }

    FixReport {
        url: format!("https://github.com/{repo}/pull/{n}"),
        label: format!("{repo}#{n}"),
        title: str_of(pr, "/title").unwrap_or_default().to_string(),
        finished: pr["merged"].as_bool() == Some(true) || pr["state"] == "CLOSED",
        head,
        pending,
        problems,
        guide: guide(repo),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_checks_threads_and_conflicts() {
        let pr = json!({
            "title": "Fix it", "state": "OPEN", "merged": false, "mergeable": "CONFLICTING",
            "headRefOid": "abcdef0123456789",
            "reviewThreads": { "nodes": [
                { "id": "T1", "isResolved": false, "isOutdated": false, "path": "src/a.rs", "line": 4,
                  "comments": { "nodes": [
                    { "databaseId": 1, "author": { "login": "dana" }, "body": "Off by one?", "url": "u1" },
                    { "databaseId": 2, "author": { "login": "me" }, "body": "Not quite: …", "url": "u2" }
                  ] } },
                { "id": "T2", "isResolved": true, "comments": { "nodes": [] } }
            ] },
            "reviews": { "nodes": [ { "databaseId": 9, "author": { "login": "dana" }, "body": "", "url": "r" } ] },
            "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "contexts": { "nodes": [
                { "__typename": "CheckRun", "name": "test", "conclusion": "FAILURE", "status": "COMPLETED",
                  "detailsUrl": "d", "title": "2 failed", "summary": null,
                  "checkSuite": { "workflowRun": { "databaseId": 77 } } },
                { "__typename": "CheckRun", "name": "lint", "conclusion": null, "status": "IN_PROGRESS" },
                { "__typename": "StatusContext", "context": "deploy", "state": "SUCCESS" }
            ] } } } } ] }
        });
        let r = report("o/r", 5, Some("me"), &pr);
        assert!(!r.finished && r.pending);
        let ids: Vec<&str> = r.problems.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "conflict:abcdef012345",
                "check:test:abcdef012345",
                "thread:T1:1"
            ]
        );
        assert_eq!(r.problems[1].handle.as_deref(), Some("run 77"));
        assert_eq!(r.problems[2].title, "src/a.rs:4");
        assert!(
            r.problems[2]
                .detail
                .as_deref()
                .unwrap()
                .contains("dana: Off by one?")
        );
    }

    #[test]
    fn merged_is_finished() {
        let r = report(
            "o/r",
            5,
            None,
            &json!({ "title": "x", "state": "MERGED", "merged": true }),
        );
        assert!(r.finished && r.problems.is_empty());
    }
}
