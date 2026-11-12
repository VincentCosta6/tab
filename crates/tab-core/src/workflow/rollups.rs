//! Keeps daily rollups ([`tab_domain::workflow::rollup`]) in step with the
//! run log, prunes both, and reads a workflow's spend this month for its
//! caps ([`tab_domain::workflow::cap`]).

use chrono::{DateTime, Duration, Utc};
use tab_domain::workflow::cap::{self, CapUsage};
use tab_domain::workflow::rollup;
use tab_domain::workflow::stats::Spend;
use tab_domain::workflow::{Limits, Run};

use crate::store::Store;

/// What a maintenance pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Maintained {
    pub written: usize,
    pub deleted: usize,
    pub runs_pruned: usize,
    pub rollups_pruned: usize,
}

/// The hourly pass: rolls up every complete UTC day the run log holds,
/// then prunes runs before [`rollup::run_log_start`] (whole UTC days, and
/// only days this pass just rolled up or had already frozen), then rollups
/// older than 13 months. Safe to run on every device sharing a hub: the
/// rows it writes are the same everywhere (see the rollup module).
pub fn maintain(store: &Store, now: DateTime<Utc>) -> anyhow::Result<Maintained> {
    let today = now.date_naive();
    let runs: Vec<Run> = store
        .workflow_runs_from(None, DateTime::<Utc>::UNIX_EPOCH)?
        .into_iter()
        .filter(|r| r.started_at < rollup::day_start(today))
        .collect();
    let from = runs
        .iter()
        .map(|r| r.started_at.date_naive())
        .min()
        .unwrap_or(today);
    let existing = store.workflow_rollups_stored(None, from, today + Duration::days(1))?;
    let plan = rollup::plan(now, &runs, &existing);
    store.workflow_rollups_apply(&plan)?;
    Ok(Maintained {
        written: plan.put.len(),
        deleted: plan.delete.len(),
        runs_pruned: store.workflow_runs_prune(rollup::run_log_start(now))?,
        rollups_pruned: store.workflow_rollups_prune(rollup::keep_rollups_from(now))?,
    })
}

/// What `workflow_id` spent this UTC month.
pub fn month_spend(store: &Store, workflow_id: &str, now: DateTime<Utc>) -> anyhow::Result<Spend> {
    let month = rollup::month_start(now);
    let runs = store.workflow_runs_from(Some(workflow_id), rollup::day_start(month))?;
    let rollups = store.workflow_rollups(
        Some(workflow_id),
        month,
        rollup::run_log_start(now).date_naive(),
    )?;
    Ok(rollup::month_spend(&runs, &rollups, now))
}

/// Why a run of a workflow with these limits can't start now, if it can't.
pub fn cap_refusal(
    store: &Store,
    workflow_id: &str,
    limits: &Limits,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<String>> {
    if !cap::has_cap(limits) {
        return Ok(None);
    }
    Ok(cap::reached(limits, &month_spend(store, workflow_id, now)?))
}

/// The cap meter's numbers, when the workflow has a cap.
pub fn cap_usage(
    store: &Store,
    workflow_id: &str,
    limits: &Limits,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<CapUsage>> {
    if !cap::has_cap(limits) {
        return Ok(None);
    }
    Ok(CapUsage::of(
        limits,
        month_spend(store, workflow_id, now)?,
        now,
    ))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::Value;

    use super::*;
    use crate::workflow::{Billing, RunStatus, StepMetrics, StepRun, StepStatus, templates};

    fn run(id: &str, workflow: &str, at: DateTime<Utc>, cost: f64, billing: Billing) -> Run {
        Run {
            id: id.into(),
            workflow_id: workflow.into(),
            workflow_name: "W".into(),
            version: 1,
            status: RunStatus::Succeeded,
            dry_run: false,
            trigger: "t".into(),
            trigger_key: None,
            item_id: None,
            chain: vec![],
            steps: vec![StepRun {
                id: "fix".into(),
                label: "Fix".into(),
                status: StepStatus::Ok,
                input: Value::Null,
                output: Value::Null,
                error: None,
                ms: 5,
                metrics: Some(StepMetrics {
                    work_ms: 60_000,
                    billing,
                    cost_usd: cost,
                    input_tokens: 1_000,
                    output_tokens: 500,
                    ..Default::default()
                }),
                interventions: vec![],
            }],
            error: None,
            started_at: at,
            finished_at: Some(at + Duration::seconds(70)),
            runner: String::new(),
            resume: None,
            rerun_of: None,
            started_by: None,
            caused_by: None,
            approved: Vec::new(),
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 12, 15, 0, 0).unwrap()
    }

    #[test]
    fn a_pass_rolls_days_up_before_pruning_them_and_repeats_as_a_no_op() {
        let s = Store::in_memory().unwrap();
        let w = s.workflow_save(&templates()[0], "user", now()).unwrap();
        let day = Duration::days(1);
        for n in 0..45 {
            assert!(
                s.workflow_run_insert(&run(
                    &format!("r{n}"),
                    &w.id,
                    now() - day * n,
                    0.5,
                    Billing::Api
                ))
                .unwrap()
            );
        }
        let m = maintain(&s, now()).unwrap();
        // Days 1..=44 back (today isn't rolled up); 14 of them leave the log.
        assert_eq!((m.written, m.runs_pruned), (44, 14));
        let all = s
            .workflow_rollups(None, now().date_naive() - day * 60, now().date_naive())
            .unwrap();
        assert_eq!(all.len(), 44);
        assert_eq!(all.iter().map(|r| r.body.counts.runs).sum::<u32>(), 44);
        // The run log keeps whole UTC days from its start.
        let kept = s
            .workflow_runs_from(None, DateTime::<Utc>::UNIX_EPOCH)
            .unwrap();
        assert_eq!(kept.len(), 31);
        assert!(
            kept.iter()
                .all(|r| r.started_at >= rollup::run_log_start(now()))
        );

        // Again: nothing to write, and the frozen days are left as they are.
        assert_eq!(
            maintain(&s, now() + Duration::hours(1)).unwrap(),
            Maintained::default()
        );

        // Months on, the rest of the log goes, once today's runs (never
        // rolled up while it was today) are.
        let m = maintain(&s, now() + day * 100).unwrap();
        assert_eq!((m.written, m.runs_pruned, m.rollups_pruned), (1, 31, 0));
        assert!(
            s.workflow_runs_from(None, DateTime::<Utc>::UNIX_EPOCH)
                .unwrap()
                .is_empty()
        );
        // Thirteen months on, the rollups go too.
        let m = maintain(&s, now() + day * 420).unwrap();
        assert_eq!(m.rollups_pruned, 45);
    }

    #[test]
    fn deleting_a_workflow_deletes_its_rollups() {
        let s = Store::in_memory().unwrap();
        let w = s.workflow_save(&templates()[0], "user", now()).unwrap();
        s.workflow_run_insert(&run(
            "a",
            &w.id,
            now() - Duration::days(2),
            0.1,
            Billing::Api,
        ))
        .unwrap();
        maintain(&s, now()).unwrap();
        let range = (now().date_naive() - Duration::days(9), now().date_naive());
        assert_eq!(s.workflow_rollups(None, range.0, range.1).unwrap().len(), 1);
        s.workflow_delete(&w.id).unwrap();
        assert!(
            s.workflow_rollups(None, range.0, range.1)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn caps_read_this_months_spend_in_dollars_and_plan_tokens_apart() {
        let s = Store::in_memory().unwrap();
        let w = s.workflow_save(&templates()[0], "user", now()).unwrap();
        let day = Duration::days(1);
        // Last month's spend doesn't count.
        s.workflow_run_insert(&run("old", &w.id, now() - day * 13, 50.0, Billing::Api))
            .unwrap();
        s.workflow_run_insert(&run("a", &w.id, now() - day * 3, 12.0, Billing::Api))
            .unwrap();
        s.workflow_run_insert(&run("b", &w.id, now() - day, 8.0, Billing::Api))
            .unwrap();
        s.workflow_run_insert(&run("c", &w.id, now(), 0.0, Billing::Subscription))
            .unwrap();
        maintain(&s, now()).unwrap();
        let spent = month_spend(&s, &w.id, now()).unwrap();
        assert!((spent.cost_usd - 20.0).abs() < 1e-9);
        assert_eq!(spent.plan_input_tokens + spent.plan_output_tokens, 1_500);

        let no_cap = Limits::default();
        assert_eq!(cap_refusal(&s, &w.id, &no_cap, now()).unwrap(), None);
        assert_eq!(cap_usage(&s, &w.id, &no_cap, now()).unwrap(), None);
        let usd = Limits {
            monthly_cap_usd: 20.0,
            ..Default::default()
        };
        assert_eq!(
            cap_refusal(&s, &w.id, &usd, now()).unwrap().unwrap(),
            "This month's $20.00 spend cap is reached; runs start again on the 1st (UTC)."
        );
        let roomy = Limits {
            monthly_cap_usd: 25.0,
            monthly_cap_plan_tokens: 1_501,
            ..Default::default()
        };
        assert_eq!(cap_refusal(&s, &w.id, &roomy, now()).unwrap(), None);
        let tokens = Limits {
            monthly_cap_plan_tokens: 1_500,
            ..Default::default()
        };
        assert!(
            cap_refusal(&s, &w.id, &tokens, now())
                .unwrap()
                .unwrap()
                .contains("1,500 subscription token cap")
        );
        let u = cap_usage(&s, &w.id, &roomy, now()).unwrap().unwrap();
        assert!((u.spend.cost_usd - 20.0).abs() < 1e-9 && u.cap_usd == 25.0);
        // A new month starts again.
        let december = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
        assert_eq!(cap_refusal(&s, &w.id, &usd, december).unwrap(), None);
    }
}
