//! Where a chat the person starts away from a computer runs.
//!
//! A phone can't run agents. A chat started there becomes a job
//! ([`Job::chat`]) that one of the person's runners takes: tab cloud's
//! runner or one of their computers. By default ([`RunOn::Auto`]) tab
//! prefers the cloud: it gets first refusal for [`CLOUD_FIRST_MINUTES`],
//! and if it doesn't take the job by then (asleep, broken, busy) any
//! computer that's online may. When the plan has no cloud runner, or the
//! cloud runner can't run the job, a computer takes it. The person can
//! instead pick the cloud, their computers, or one runner.
//!
//! [`plan`] decides the job's placement and says who will most likely take
//! it, so the app can show "Runs on tab cloud" or "Waits for your MacBook"
//! before the person sends. It is pure: the caller passes the runner
//! records (synced from the hub), the cloud runner's state (from tab
//! cloud's account API) and `now`.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::runner::{Job, JobSpec, NotForYou, Placement, RunnerCaps, RunnerKind};

/// How long the cloud has the job to itself under [`RunOn::Auto`]: time
/// to wake a sleeping cloud runner and for it to look for jobs.
pub const CLOUD_FIRST_MINUTES: u32 = 3;

/// Where the person asked the chat to run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunOn {
    /// tab cloud if it can, else a computer that's online.
    #[default]
    Auto,
    /// Only tab cloud.
    Cloud,
    /// Only one of the person's computers.
    Computers,
    /// Only this runner (its [`RunnerCaps::id`]).
    Runner { id: String },
}

/// The person's tab cloud runner, as tab cloud's account API reports it.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Cloud {
    /// Their plan has no cloud runner.
    #[default]
    NotOnPlan,
    /// On the plan but not running; tab wakes it.
    Asleep,
    /// Running (or starting).
    Awake,
}

/// Who will most likely take the job, for the app to say before it's sent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Expect {
    /// This runner is online and can run it.
    Runner {
        id: String,
        name: String,
        kind: RunnerKind,
    },
    /// tab cloud can run it once it's woken; the app wakes it on send.
    WakeCloud,
    /// Nobody who may run it is online yet; it starts when one is.
    Waits { why: String },
    /// Nobody who may run it ever could, as things stand.
    Never { why: String },
}

/// What [`plan`] decided.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plan {
    pub placement: Placement,
    /// With [`Placement::Pinned`], the runner.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runner: String,
    pub expect: Expect,
}

impl Plan {
    /// Puts this plan's placement on `spec`.
    pub fn apply(&self, spec: &mut JobSpec) {
        spec.placement = self.placement;
        spec.runner = self.runner.clone();
    }

    /// Whether sending should wake the cloud runner.
    pub fn wakes_cloud(&self) -> bool {
        matches!(self.expect, Expect::WakeCloud)
    }
}

/// Where a chat with `spec`'s work runs, for the person's choice `on`.
/// `runners` are the hub's runner records, `cloud` the cloud runner's
/// state.
pub fn plan(
    on: &RunOn,
    spec: &JobSpec,
    runners: &[RunnerCaps],
    cloud: Cloud,
    now: DateTime<Utc>,
) -> Plan {
    let cloud_ok = cloud_can(spec, runners, cloud, now);
    let computer = best_computer(spec, runners, now);
    match on {
        RunOn::Auto => match (cloud_ok, computer) {
            (Ok(expect), _) => Plan {
                placement: Placement::PreferCloud {
                    minutes: CLOUD_FIRST_MINUTES,
                },
                runner: String::new(),
                expect,
            },
            (Err(_), Some(r)) => computers(Some(r)),
            (Err(_), None) => computers(None),
        },
        RunOn::Cloud => Plan {
            placement: Placement::Cloud,
            runner: String::new(),
            expect: cloud_ok.unwrap_or_else(|why| Expect::Never { why }),
        },
        RunOn::Computers => computers(computer),
        RunOn::Runner { id } => {
            let Some(r) = runners.iter().find(|r| &r.id == id) else {
                return Plan {
                    placement: Placement::Pinned,
                    runner: id.clone(),
                    expect: Expect::Never {
                        why: "That runner isn't connected to your hub.".into(),
                    },
                };
            };
            let expect = match r.kind {
                RunnerKind::Cloud => cloud_ok.unwrap_or_else(|why| Expect::Never { why }),
                RunnerKind::Computer => match fits(spec, r, now) {
                    Err(why) => Expect::Never { why },
                    Ok(()) if r.awake(now) => found(r),
                    Ok(()) => Expect::Waits {
                        why: format!("{} is offline; it starts when it's back.", name(r)),
                    },
                },
            };
            Plan {
                placement: Placement::Pinned,
                runner: id.clone(),
                expect,
            }
        }
    }
}

fn computers(r: Option<&RunnerCaps>) -> Plan {
    Plan {
        placement: Placement::Computers,
        runner: String::new(),
        expect: match r {
            Some(r) => found(r),
            None => Expect::Waits {
                why: "None of your computers that can run it is online; it starts when one is."
                    .into(),
            },
        },
    }
}

/// Whether the cloud can run it, and how.
fn cloud_can(
    spec: &JobSpec,
    runners: &[RunnerCaps],
    cloud: Cloud,
    now: DateTime<Utc>,
) -> Result<Expect, String> {
    if cloud == Cloud::NotOnPlan {
        return Err("Your plan doesn't include tab cloud agents.".into());
    }
    // Its record says what it has. One that never started has none yet:
    // trust it to have the harnesses tab cloud sets up.
    let record = runners
        .iter()
        .filter(|r| r.kind == RunnerKind::Cloud)
        .max_by_key(|r| r.seen_at);
    if let Some(r) = record {
        fits(spec, r, now)?;
        if cloud == Cloud::Awake && r.awake(now) {
            return Ok(found(r));
        }
    }
    Ok(Expect::WakeCloud)
}

/// The computer most likely to take it: online, able, heard from last.
fn best_computer<'a>(
    spec: &JobSpec,
    runners: &'a [RunnerCaps],
    now: DateTime<Utc>,
) -> Option<&'a RunnerCaps> {
    runners
        .iter()
        .filter(|r| r.kind == RunnerKind::Computer && r.awake(now))
        .filter(|r| fits(spec, r, now).is_ok())
        .max_by_key(|r| r.seen_at)
}

/// Whether `r` could run the work at all, whatever the placement.
fn fits(spec: &JobSpec, r: &RunnerCaps, now: DateTime<Utc>) -> Result<(), String> {
    let mut probe = Job::chat("probe", spec.clone(), now - Duration::days(1));
    probe.spec.placement = Placement::Anywhere;
    probe.deadline = now + Duration::days(1);
    probe.can_take(r, false, now).map_err(|e| match e {
        NotForYou::NoHarness(h) => format!("{} doesn't have {h} set up.", name(r)),
        NotForYou::NoRepo(repo) => format!("{} doesn't have {repo} checked out.", name(r)),
        _ => format!("{} can't run it.", name(r)),
    })
}

fn found(r: &RunnerCaps) -> Expect {
    Expect::Runner {
        id: r.id.clone(),
        name: name(r),
        kind: r.kind,
    }
}

fn name(r: &RunnerCaps) -> String {
    if !r.name.is_empty() {
        r.name.clone()
    } else if r.kind == RunnerKind::Cloud {
        "tab cloud".into()
    } else {
        r.id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(min: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap() + Duration::minutes(min)
    }

    fn spec(repo: &str) -> JobSpec {
        JobSpec {
            prompt: "Fix it".into(),
            repo: repo.into(),
            ..Default::default()
        }
    }

    fn laptop(seen: i64) -> RunnerCaps {
        RunnerCaps {
            id: "laptop".into(),
            name: "Vincent's MacBook".into(),
            kind: RunnerKind::Computer,
            harnesses: vec!["claude_code".into()],
            repos: vec!["acme/api".into()],
            seen_at: Some(t(seen)),
            ..Default::default()
        }
    }

    fn cloud(seen: i64) -> RunnerCaps {
        RunnerCaps {
            id: "runner-hub1".into(),
            kind: RunnerKind::Cloud,
            harnesses: vec!["claude_code".into()],
            seen_at: Some(t(seen)),
            ..Default::default()
        }
    }

    fn runner(id: &str, name: &str, kind: RunnerKind) -> Expect {
        Expect::Runner {
            id: id.into(),
            name: name.into(),
            kind,
        }
    }

    #[test]
    fn auto_prefers_an_awake_cloud_and_gives_computers_a_turn_after() {
        let p = plan(
            &RunOn::Auto,
            &spec("acme/api"),
            &[laptop(0), cloud(0)],
            Cloud::Awake,
            t(1),
        );
        assert_eq!(
            p.placement,
            Placement::PreferCloud {
                minutes: CLOUD_FIRST_MINUTES
            }
        );
        assert_eq!(
            p.expect,
            runner("runner-hub1", "tab cloud", RunnerKind::Cloud)
        );
        assert!(!p.wakes_cloud());
    }

    #[test]
    fn auto_wakes_a_sleeping_cloud() {
        let p = plan(
            &RunOn::Auto,
            &spec(""),
            &[laptop(0), cloud(-600)],
            Cloud::Asleep,
            t(1),
        );
        assert!(matches!(p.placement, Placement::PreferCloud { .. }));
        assert!(p.wakes_cloud());
        // A cloud runner that never started yet is woken too.
        let p = plan(&RunOn::Auto, &spec(""), &[], Cloud::Asleep, t(1));
        assert!(p.wakes_cloud());
    }

    #[test]
    fn auto_uses_an_online_computer_without_tab_cloud() {
        let p = plan(
            &RunOn::Auto,
            &spec("acme/api"),
            &[laptop(0)],
            Cloud::NotOnPlan,
            t(1),
        );
        assert_eq!(p.placement, Placement::Computers);
        assert_eq!(
            p.expect,
            runner("laptop", "Vincent's MacBook", RunnerKind::Computer)
        );
    }

    #[test]
    fn auto_falls_to_computers_when_the_cloud_lacks_the_harness() {
        let mut s = spec("acme/api");
        s.harness = "codex".into();
        let mut l = laptop(0);
        l.harnesses.push("codex".into());
        let p = plan(&RunOn::Auto, &s, &[l, cloud(0)], Cloud::Awake, t(1));
        assert_eq!(p.placement, Placement::Computers);
        assert!(matches!(p.expect, Expect::Runner { ref id, .. } if id == "laptop"));
    }

    #[test]
    fn auto_waits_when_nobody_who_can_is_online() {
        // The laptop was last heard from ten minutes ago.
        let p = plan(
            &RunOn::Auto,
            &spec("acme/api"),
            &[laptop(-10)],
            Cloud::NotOnPlan,
            t(0),
        );
        assert_eq!(p.placement, Placement::Computers);
        assert!(matches!(p.expect, Expect::Waits { .. }));
    }

    #[test]
    fn a_computer_without_the_repo_isnt_expected() {
        let p = plan(
            &RunOn::Computers,
            &spec("acme/web"),
            &[laptop(0)],
            Cloud::NotOnPlan,
            t(1),
        );
        assert!(matches!(p.expect, Expect::Waits { .. }));
    }

    #[test]
    fn the_newest_able_computer_is_expected() {
        let mut desk = laptop(1);
        desk.id = "desk".into();
        desk.name = "Desk".into();
        let p = plan(
            &RunOn::Computers,
            &spec(""),
            &[laptop(0), desk],
            Cloud::Awake,
            t(2),
        );
        assert_eq!(p.expect, runner("desk", "Desk", RunnerKind::Computer));
    }

    #[test]
    fn choosing_the_cloud_without_it_on_the_plan_never_runs() {
        let p = plan(
            &RunOn::Cloud,
            &spec(""),
            &[laptop(0)],
            Cloud::NotOnPlan,
            t(1),
        );
        assert_eq!(p.placement, Placement::Cloud);
        assert!(matches!(p.expect, Expect::Never { .. }));
    }

    #[test]
    fn choosing_a_runner_pins_the_job_to_it() {
        let mut s = spec("acme/api");
        let p = plan(
            &RunOn::Runner {
                id: "laptop".into(),
            },
            &s,
            &[laptop(0), cloud(0)],
            Cloud::Awake,
            t(1),
        );
        assert_eq!(p.placement, Placement::Pinned);
        assert_eq!(p.runner, "laptop");
        p.apply(&mut s);
        assert_eq!(
            (s.placement, s.runner.as_str()),
            (Placement::Pinned, "laptop")
        );

        // Offline: it waits for that one.
        let p = plan(
            &RunOn::Runner {
                id: "laptop".into(),
            },
            &spec("acme/api"),
            &[laptop(-10)],
            Cloud::Awake,
            t(0),
        );
        assert!(matches!(p.expect, Expect::Waits { .. }));

        // Can't: it says why.
        let p = plan(
            &RunOn::Runner {
                id: "laptop".into(),
            },
            &spec("acme/web"),
            &[laptop(0)],
            Cloud::Awake,
            t(1),
        );
        assert_eq!(
            p.expect,
            Expect::Never {
                why: "Vincent's MacBook doesn't have acme/web checked out.".into()
            }
        );

        // Unknown.
        let p = plan(
            &RunOn::Runner { id: "gone".into() },
            &spec(""),
            &[],
            Cloud::Awake,
            t(1),
        );
        assert!(matches!(p.expect, Expect::Never { .. }));
    }

    #[test]
    fn a_job_auto_plans_is_taken_the_way_it_said() {
        // The plan and the runner's own check agree: the cloud first, the
        // laptop once the cloud's head start is over.
        let mut s = spec("acme/api");
        plan(&RunOn::Auto, &s, &[laptop(0), cloud(0)], Cloud::Awake, t(0)).apply(&mut s);
        let j = Job::chat("j", s, t(0));
        assert!(j.can_take(&cloud(0), false, t(1)).is_ok());
        assert_eq!(j.can_take(&laptop(0), false, t(1)), Err(NotForYou::NotYet));
        assert!(
            j.can_take(&laptop(0), false, t(i64::from(CLOUD_FIRST_MINUTES)))
                .is_ok()
        );
    }

    #[test]
    fn choices_round_trip_as_json() {
        for on in [
            RunOn::Auto,
            RunOn::Cloud,
            RunOn::Computers,
            RunOn::Runner {
                id: "laptop".into(),
            },
        ] {
            let back: RunOn = serde_json::from_str(&serde_json::to_string(&on).unwrap()).unwrap();
            assert_eq!(back, on);
        }
        assert_eq!(
            serde_json::to_string(&RunOn::Runner { id: "x".into() }).unwrap(),
            r#"{"type":"runner","id":"x"}"#
        );
    }
}
