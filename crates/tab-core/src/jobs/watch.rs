//! What a runner sees of a job's chat while it works: when the agent
//! waited on the person, what the person did, and what the chat spent.
//! The usage counters never leave this device, so the runner copies their
//! totals onto the job.

use chrono::{DateTime, Duration, Utc};
use tab_domain::runner::JobUsage;
use tab_domain::workflow::{Billing, Intervention, InterventionKind};

use crate::model::{SessionMessage, SessionStatus};
use crate::store::{PlanWindow, Store};

/// How much earlier than the runner saw it a wait may have begun: the
/// runner reads the chat's status this often.
pub(super) const POLL: std::time::Duration = std::time::Duration::from_secs(3);

pub(super) struct Watch {
    session: String,
    started: DateTime<Utc>,
    /// The plan's windows before the agent started.
    plan: Vec<PlanWindow>,
    /// Messages up to this id were in the chat once the job's prompt was.
    seen: i64,
    waiting: Option<DateTime<Utc>>,
    waits: Vec<(DateTime<Utc>, DateTime<Utc>)>,
}

impl Watch {
    /// `plan` is the plan's windows as they were before the agent started.
    pub fn new(
        store: &Store,
        session: &str,
        plan: Vec<PlanWindow>,
        started: DateTime<Utc>,
    ) -> Self {
        let seen = store
            .messages(session)
            .unwrap_or_default()
            .iter()
            .map(|m| m.id)
            .max()
            .unwrap_or(0);
        Self {
            session: session.into(),
            started,
            plan,
            seen,
            waiting: None,
            waits: Vec::new(),
        }
    }

    /// Notes the chat's status at `now`.
    pub fn saw(&mut self, status: Option<SessionStatus>, now: DateTime<Utc>) {
        match (status == Some(SessionStatus::NeedsInput), self.waiting) {
            (true, None) => self.waiting = Some(now),
            (false, Some(from)) => {
                self.waits.push((from, now));
                self.waiting = None;
            }
            _ => {}
        }
    }

    /// What the chat spent and each time the person stepped in, as of
    /// `now`. `runner_stopped` when the runner stopped the agent itself (at
    /// the deadline), so that stop isn't put down to the person.
    pub fn report(
        &mut self,
        store: &Store,
        now: DateTime<Utc>,
        runner_stopped: bool,
    ) -> (JobUsage, Vec<Intervention>) {
        self.saw(None, now);
        let mut usage =
            crate::usage::job_usage(&store.usage_for_session(&self.session).unwrap_or_default());
        if usage.harness.is_empty()
            && let Ok(Some(s)) = store.session_get(&self.session)
        {
            usage.harness = s.agent.as_str().into();
            usage.model = s.model.unwrap_or_default();
        }
        usage.human_wait_ms = self
            .waits
            .iter()
            .map(|(a, b)| (*b - *a).num_milliseconds().max(0) as u64)
            .sum();
        if usage.billing == Billing::Subscription {
            // The windows are the whole account's: only a job that had the
            // plan to itself since they were last read can claim the rise.
            let since = self
                .plan
                .iter()
                .map(|w| w.updated_at)
                .min()
                .unwrap_or(self.started)
                .min(self.started);
            if matches!(store.plan_used_elsewhere(&self.session, since), Ok(false)) {
                let after = store.plan_windows().unwrap_or_default();
                usage.plan_share = crate::usage::plan_share(&self.plan, &after, self.started);
            }
        }
        let msgs = store.messages(&self.session).unwrap_or_default();
        let mut found = interventions(&msgs, self.seen, &self.waits);
        if runner_stopped {
            found.retain(|i| i.kind != InterventionKind::Stopped);
        }
        (usage, found)
    }
}

/// Each time a person stepped in on a chat, from its messages after id
/// `seen` and the times it waited on them (`waits`): a question for each
/// wait, a message for each one they wrote unasked (outside a wait), and
/// a stop for each time they stopped it.
fn interventions(
    msgs: &[SessionMessage],
    seen: i64,
    waits: &[(DateTime<Utc>, DateTime<Utc>)],
) -> Vec<Intervention> {
    let slack = Duration::from_std(POLL).unwrap_or_default();
    let answering = |at: DateTime<Utc>| waits.iter().any(|(a, b)| at >= *a - slack && at <= *b);
    let mut out: Vec<Intervention> = waits
        .iter()
        .map(|(a, _)| Intervention {
            kind: InterventionKind::Question,
            at: *a,
        })
        .collect();
    for m in msgs.iter().filter(|m| m.id > seen) {
        let kind = match (m.role.as_str(), m.kind.as_str()) {
            ("user", "text") if !answering(m.created_at) => InterventionKind::Message,
            ("system", "status") if m.content == "Stopped" => InterventionKind::Stopped,
            _ => continue,
        };
        out.push(Intervention {
            kind,
            at: m.created_at,
        });
    }
    out.sort_by_key(|i| i.at);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + s, 0).unwrap()
    }

    fn msg(id: i64, role: &str, kind: &str, content: &str, at: i64) -> SessionMessage {
        SessionMessage {
            id,
            session_id: "s".into(),
            role: role.into(),
            kind: kind.into(),
            content: content.into(),
            meta: serde_json::Value::Null,
            created_at: t(at),
        }
    }

    #[test]
    fn a_wait_is_a_question_and_its_answer_is_not_a_message() {
        let msgs = [
            msg(1, "user", "text", "Fix the flaky test", 0),
            msg(2, "assistant", "text", "Which test?", 10),
            // Answered between two looks at the chat, just after the wait
            // the runner saw.
            msg(3, "user", "text", "The clock one", 25),
            msg(4, "user", "text", "Also, don't touch the CI file", 60),
            msg(5, "system", "status", "Stopped", 90),
        ];
        let got = interventions(&msgs, 1, &[(t(12), t(27))]);
        let kinds: Vec<_> = got.iter().map(|i| (i.kind, i.at)).collect();
        assert_eq!(
            kinds,
            [
                (InterventionKind::Question, t(12)),
                (InterventionKind::Message, t(60)),
                (InterventionKind::Stopped, t(90)),
            ]
        );
    }

    #[test]
    fn the_jobs_own_prompt_is_not_the_person_stepping_in() {
        let msgs = [msg(7, "user", "text", "Fix the flaky test", 0)];
        assert!(interventions(&msgs, 7, &[]).is_empty());
    }

    #[test]
    fn a_wait_lasts_from_the_first_look_that_saw_it_to_the_first_that_didnt() {
        let mut w = Watch {
            session: "s".into(),
            started: t(0),
            plan: Vec::new(),
            seen: 0,
            waiting: None,
            waits: Vec::new(),
        };
        w.saw(Some(SessionStatus::Working), t(3));
        w.saw(Some(SessionStatus::NeedsInput), t(6));
        w.saw(Some(SessionStatus::NeedsInput), t(9));
        w.saw(Some(SessionStatus::Working), t(30));
        w.saw(Some(SessionStatus::NeedsInput), t(40));
        w.saw(None, t(45));
        assert_eq!(w.waits, [(t(6), t(30)), (t(40), t(45))]);
    }
}
