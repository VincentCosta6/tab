//! Autofix: keep a task's PR (or anything a connector can report problems
//! on) healthy by handing new problems to the task's agent.
//!
//! A connector reports a link's [`FixReport`] (failing checks, unresolved
//! review threads, conflicts). The runtime polls it on [`next_check`]'s
//! schedule, picks out the problems the agent hasn't been given yet
//! ([`fresh`]) and sends [`prompt`] into the task's chat. Everything here is
//! pure, so the same report always gives the same schedule and message.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// What kind of problem, so the prompt can say what to do about it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FixKind {
    /// A check or CI job failed.
    Check,
    /// A review thread or comment is still open.
    Thread,
    /// The branch no longer merges cleanly.
    Conflict,
}

/// One thing that's wrong with a link, in the connector's words.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FixProblem {
    pub kind: FixKind,
    /// Stable while the problem is the same one. A connector changes it when
    /// the problem comes back new: a check failing again on a later commit,
    /// someone else replying on a thread.
    pub id: String,
    /// "ci / test", "src/app.rs:12".
    pub title: String,
    /// What it says: the comment, the check's summary.
    pub detail: Option<String>,
    pub url: Option<String>,
    /// What the agent needs to act on it (a thread id to reply and resolve).
    #[serde(default)]
    pub handle: Option<String>,
}

/// Where a link stands for autofix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FixReport {
    pub url: String,
    /// "acme/api#12".
    pub label: String,
    pub title: String,
    /// Merged or closed: nothing left to fix, autofix stops.
    pub finished: bool,
    /// The commit the report is for. A new one means the agent pushed (or
    /// someone did), which counts as activity.
    pub head: Option<String>,
    /// Checks still running: wait for them before calling anything fixed.
    pub pending: bool,
    pub problems: Vec<FixProblem>,
    /// How to act on this service, for the agent: which commands reply to
    /// and resolve a thread, read a check's log, and so on.
    pub guide: String,
}

/// A link's bar and autofix state in one task's chat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Watch {
    pub item_id: String,
    pub url: String,
    /// Autofix is on.
    pub enabled: bool,
    /// The person closed the bar for this link.
    pub dismissed: bool,
    /// The last activity: autofix turned on, a push, a new problem.
    pub active_at: DateTime<Utc>,
    pub next_at: Option<DateTime<Utc>>,
    pub checked_at: Option<DateTime<Utc>>,
    pub head: Option<String>,
    /// Problem ids already handed to the agent.
    pub handled: Vec<String>,
    /// What the last check found, for the bar's tooltip.
    pub last: Option<String>,
}

/// Every minute while something is going on, then backing off.
pub const EVERY: Duration = Duration::minutes(1);
/// How long after the last activity checks stay at [`EVERY`].
pub const ACTIVE_FOR: Duration = Duration::minutes(30);
/// The longest wait between checks once backed off.
pub const MAX_WAIT: Duration = Duration::hours(4);

/// When to look again: every minute for 30 minutes after the last activity
/// (turning autofix on, a push, a new problem), then doubling each time
/// (2, 4, 8 minutes…) up to [`MAX_WAIT`].
pub fn next_check(active_at: DateTime<Utc>, now: DateTime<Utc>) -> DateTime<Utc> {
    let quiet = now - active_at;
    if quiet < ACTIVE_FOR {
        return now + EVERY;
    }
    // Wait about as long again as we've been backed off: checks at 30, 32,
    // 36, 44, 60 minutes… (2^bits of the minutes past the window).
    let past = (quiet - ACTIVE_FOR).num_minutes().max(1) as u64;
    let step = (64 - past.leading_zeros()).min(16);
    (now + EVERY * 2i32.pow(step)).min(now + MAX_WAIT)
}

/// The problems in `report` whose ids aren't in `handled`.
pub fn fresh<'a>(report: &'a FixReport, handled: &[String]) -> Vec<&'a FixProblem> {
    report
        .problems
        .iter()
        .filter(|p| !handled.contains(&p.id))
        .collect()
}

/// What to keep in `handled` after sending `sent`: ids still reported
/// (so a fixed-then-broken-again problem with the same id isn't resent
/// while it's still listed) plus the ones just sent.
pub fn remember(report: &FixReport, handled: &[String], sent: &[&FixProblem]) -> Vec<String> {
    let mut out: Vec<String> = handled
        .iter()
        .filter(|h| report.problems.iter().any(|p| &p.id == *h))
        .cloned()
        .collect();
    for p in sent {
        if !out.contains(&p.id) {
            out.push(p.id.clone());
        }
    }
    out
}

/// The message that hands `problems` to the agent.
pub fn prompt(report: &FixReport, problems: &[&FixProblem]) -> String {
    let mut s = format!(
        "Autofix: {} ({}) has {} new problem{} to look at.\n",
        report.label,
        report.title,
        problems.len(),
        if problems.len() == 1 { "" } else { "s" }
    );
    for (kind, heading) in [
        (FixKind::Conflict, "Merge conflicts"),
        (FixKind::Check, "Failing checks"),
        (FixKind::Thread, "Open review threads"),
    ] {
        let of: Vec<_> = problems.iter().filter(|p| p.kind == kind).collect();
        if of.is_empty() {
            continue;
        }
        s.push_str(&format!("\n## {heading}\n"));
        for p in of {
            s.push_str(&format!("- {}", p.title));
            if let Some(u) = &p.url {
                s.push_str(&format!(" ({u})"));
            }
            if let Some(h) = &p.handle {
                s.push_str(&format!(" [id: {h}]"));
            }
            s.push('\n');
            if let Some(d) = p.detail.as_deref().filter(|d| !d.trim().is_empty()) {
                for line in d.trim().lines().take(20) {
                    s.push_str(&format!("  > {line}\n"));
                }
            }
        }
    }
    s.push_str(
        "\n## What to do\n\
         - Merge conflicts: merge or rebase onto the base branch, resolve them, and push.\n\
         - Failing checks: read the failure first. If it comes from this PR's changes, fix it and push. \
         If it doesn't (a flaky test, an outage, something already broken on the base branch), don't \
         change code for it: ask me what to do (re-run it, ignore it, or fix it here anyway).\n\
         - Review threads: investigate each one; not every comment needs a change. If it's a real \
         concern, fix it, push, then reply on the thread with what changed (and the commit) and \
         resolve it. Resolve only once the fixing commit is pushed. If it isn't a real concern, \
         reply explaining why and leave the thread open for the reviewer.\n\
         - When you're done, say in one or two lines what you fixed, replied to or left for me.\n",
    );
    if !report.guide.trim().is_empty() {
        s.push_str(&format!(
            "\n## How, on this service\n{}\n",
            report.guide.trim()
        ));
    }
    s
}

/// One look at a watched link: the watch's new state, and the message to
/// send the agent, if any. `report` is `None` when no connector knows the
/// link; `idle` is whether the chat can take a message now (otherwise new
/// problems wait for the next look, which stays at a minute).
pub fn step(
    w: &Watch,
    report: Option<&FixReport>,
    idle: bool,
    now: DateTime<Utc>,
) -> (Watch, Option<String>) {
    let mut w = w.clone();
    w.checked_at = Some(now);
    let Some(r) = report else {
        w.last = Some("No connection can read this link".into());
        w.next_at = Some(next_check(w.active_at, now));
        return (w, None);
    };
    if r.finished {
        w.enabled = false;
        w.next_at = None;
        w.last = Some("Finished, so autofix is off".into());
        return (w, None);
    }
    if r.head.is_some() && w.head.is_some() && r.head != w.head {
        w.active_at = now;
    }
    w.head = r.head.clone();
    let new = fresh(r, &w.handled);
    let mut message = None;
    if new.is_empty() {
        w.handled = remember(r, &w.handled, &[]);
        w.last = Some(
            if !r.problems.is_empty() {
                "Waiting on the agent or reviewers"
            } else if r.pending {
                "Checks running"
            } else {
                "All clear"
            }
            .into(),
        );
    } else {
        w.active_at = now;
        let n = new.len();
        let s = if n == 1 { "" } else { "s" };
        if idle {
            message = Some(prompt(r, &new));
            w.handled = remember(r, &w.handled, &new);
            w.last = Some(format!("Gave the agent {n} problem{s}"));
        } else {
            w.last = Some(format!("{n} new problem{s}, for when the agent is free"));
        }
    }
    w.next_at = Some(next_check(w.active_at, now));
    (w, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(min: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap() + Duration::minutes(min)
    }

    fn report(ids: &[&str]) -> FixReport {
        FixReport {
            url: "https://github.com/o/r/pull/1".into(),
            label: "o/r#1".into(),
            title: "Fix it".into(),
            finished: false,
            head: Some("abc".into()),
            pending: false,
            problems: ids
                .iter()
                .map(|id| FixProblem {
                    kind: if id.starts_with('t') {
                        FixKind::Thread
                    } else {
                        FixKind::Check
                    },
                    id: id.to_string(),
                    title: id.to_string(),
                    detail: Some("it broke".into()),
                    url: None,
                    handle: Some(format!("H{id}")),
                })
                .collect(),
            guide: "Use gh.".into(),
        }
    }

    #[test]
    fn every_minute_then_backs_off() {
        assert_eq!(next_check(at(0), at(0)) - at(0), EVERY);
        assert_eq!(next_check(at(0), at(29)) - at(29), EVERY);
        // Checks at 30, 32, 36, 44, 60 minutes: each wait twice the last.
        let waits: Vec<i64> = [30, 32, 36, 44, 60]
            .iter()
            .map(|&m| (next_check(at(0), at(m)) - at(m)).num_minutes())
            .collect();
        assert_eq!(waits, [2, 4, 8, 16, 32]);
        assert_eq!(next_check(at(0), at(100_000)) - at(100_000), MAX_WAIT);
    }

    #[test]
    fn hands_over_each_problem_once() {
        let r = report(&["c1", "t1"]);
        assert_eq!(fresh(&r, &[]).len(), 2);
        let sent = fresh(&r, &[]);
        let handled = remember(&r, &[], &sent);
        assert!(fresh(&r, &handled).is_empty());
        // c1 fixed; t2 new. c1 is forgotten so it counts if it comes back.
        let r2 = report(&["t1", "t2"]);
        let new = fresh(&r2, &handled);
        assert_eq!(
            new.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["t2"]
        );
        assert_eq!(remember(&r2, &handled, &new), ["t1", "t2"]);
    }

    fn watch() -> Watch {
        Watch {
            item_id: "i".into(),
            url: "u".into(),
            enabled: true,
            dismissed: false,
            active_at: at(0),
            next_at: Some(at(0)),
            checked_at: None,
            head: None,
            handled: vec![],
            last: None,
        }
    }

    #[test]
    fn steps_wait_for_the_agent_and_stop_when_finished() {
        let r = report(&["c1"]);
        // Busy: nothing sent, nothing marked handled, look again in a minute.
        let (w, msg) = step(&watch(), Some(&r), false, at(40));
        assert!(msg.is_none() && w.handled.is_empty());
        assert_eq!(w.next_at, Some(at(41)));
        // Free: sent once.
        let (w, msg) = step(&w, Some(&r), true, at(41));
        assert!(msg.is_some());
        assert_eq!(w.handled, ["c1"]);
        let (w2, msg) = step(&w, Some(&r), true, at(80));
        assert!(msg.is_none());
        // 39 quiet minutes: backed off.
        assert!(w2.next_at.unwrap() - at(80) > EVERY);
        // A push is activity.
        let mut pushed = r.clone();
        pushed.head = Some("def".into());
        let (w3, _) = step(&w2, Some(&pushed), true, at(90));
        assert_eq!(w3.active_at, at(90));
        // Merged: off.
        let mut done = r.clone();
        done.finished = true;
        let (w4, _) = step(&w3, Some(&done), true, at(91));
        assert!(!w4.enabled && w4.next_at.is_none());
    }

    #[test]
    fn prompt_groups_problems_and_says_what_to_do() {
        let r = report(&["c1", "t1"]);
        let p = prompt(&r, &fresh(&r, &[]));
        assert!(p.starts_with("Autofix: o/r#1 (Fix it) has 2 new problems"));
        assert!(p.find("## Failing checks").unwrap() < p.find("## Open review threads").unwrap());
        assert!(p.contains("[id: Ht1]"));
        assert!(p.contains("ask me what to do"));
        assert!(p.contains("Resolve only once the fixing commit is pushed"));
        assert!(p.ends_with("Use gh.\n"));
    }
}
