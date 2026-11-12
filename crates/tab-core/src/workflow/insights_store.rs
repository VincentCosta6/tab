//! Loads runs for the Workflows tab's Insights and Factory views and adds
//! them up with [`tab_domain::workflow::insights`]. The Tauri commands are
//! thin wrappers over these.

use chrono::{DateTime, Utc};

use super::insights::{self as pure, Factory, WorkflowInsights};
use crate::store::Store;

/// One workflow's stats for the last `days` days (in the person's time)
/// and the same span before it.
pub fn workflow_insights(
    store: &Store,
    workflow_id: &str,
    days: u32,
    utc_offset_minutes: i32,
    now: DateTime<Utc>,
) -> anyhow::Result<WorkflowInsights> {
    let (_, previous) = pure::windows(now, days, utc_offset_minutes);
    let runs = store.workflow_runs_from(Some(workflow_id), previous.from)?;
    Ok(pure::workflow_insights(
        workflow_id,
        &runs,
        now,
        days,
        utc_offset_minutes,
    ))
}

/// Every workflow's stats and health, with hub-wide totals.
pub fn workflow_factory(
    store: &Store,
    days: u32,
    utc_offset_minutes: i32,
    now: DateTime<Utc>,
) -> anyhow::Result<Factory> {
    let (_, previous) = pure::windows(now, days, utc_offset_minutes);
    let workflows = store.workflows()?;
    let runs = store.workflow_runs_from(None, previous.from)?;
    Ok(pure::factory(
        &workflows,
        &runs,
        now,
        days,
        utc_offset_minutes,
    ))
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use serde_json::Value;

    use super::*;
    use crate::workflow::stats::Health;
    use crate::workflow::{Billing, Run, RunStatus, StepMetrics, StepRun, StepStatus, templates};

    fn run(
        id: &str,
        workflow: &str,
        status: RunStatus,
        at: DateTime<Utc>,
        billing: Billing,
    ) -> Run {
        Run {
            id: id.into(),
            workflow_id: workflow.into(),
            workflow_name: "W".into(),
            version: 1,
            status,
            dry_run: false,
            trigger: "t".into(),
            trigger_key: None,
            item_id: None,
            chain: vec![],
            steps: vec![StepRun {
                id: "fix".into(),
                label: "Fix".into(),
                status: if status == RunStatus::Failed {
                    StepStatus::Failed
                } else {
                    StepStatus::Ok
                },
                input: Value::Null,
                output: Value::Null,
                error: None,
                ms: 5,
                metrics: Some(StepMetrics {
                    queue_ms: 1_000,
                    work_ms: 60_000,
                    billing,
                    cost_usd: if billing == Billing::Api { 0.5 } else { 0.0 },
                    plan_share: if billing == Billing::Subscription {
                        0.25
                    } else {
                        0.0
                    },
                    input_tokens: 1_000,
                    output_tokens: 100,
                    model: "m".into(),
                    harness: "claude_code".into(),
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

    #[test]
    fn adds_up_stored_runs_for_one_workflow_and_the_factory() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let w = s.workflow_save(&templates()[0], "user", now).unwrap();
        let other = s.workflow_save(&templates()[1], "user", now).unwrap();
        let day = Duration::days(1);
        let runs = [
            run(
                "a",
                &w.id,
                RunStatus::Succeeded,
                now - Duration::minutes(5),
                Billing::Api,
            ),
            run(
                "b",
                &w.id,
                RunStatus::Succeeded,
                now - day,
                Billing::Subscription,
            ),
            run("c", &w.id, RunStatus::Failed, now - day * 2, Billing::Api),
            run(
                "d",
                &w.id,
                RunStatus::Succeeded,
                now - day * 10,
                Billing::Api,
            ),
            // Past retention and both windows.
            run("e", &w.id, RunStatus::Failed, now - day * 40, Billing::Api),
            run("f", &other.id, RunStatus::Failed, now - day, Billing::Api),
        ];
        for r in &runs {
            assert!(s.workflow_run_insert(r).unwrap());
        }
        assert_eq!(s.workflow_runs_from(None, now - day * 30).unwrap().len(), 5);

        let i = workflow_insights(&s, &w.id, 7, 0, now).unwrap();
        assert_eq!((i.stats.counts.runs, i.stats.counts.succeeded), (3, 2));
        assert_eq!(i.previous.counts.runs, 1);
        assert_eq!(i.health, Health::Degraded);
        // API sessions cost dollars; subscription sessions use the plan.
        assert!((i.stats.spend.cost_usd - 1.0).abs() < 1e-9);
        assert!((i.stats.spend.plan_share - 0.25).abs() < 1e-9);
        assert_eq!(i.stats.spend.plan_input_tokens, 1_000);
        assert_eq!(i.stats.steps[0].work_ms.p50, 60_000);

        let f = workflow_factory(&s, 7, 0, now).unwrap();
        assert_eq!(f.workflows.len(), 2);
        assert_eq!(f.totals.counts.runs, 4);
        assert_eq!(f.previous_totals.counts.runs, 1);
        // Too few runs to judge the other one.
        assert_eq!(f.workflows[0].id, w.id);
        assert_eq!(f.workflows[1].health, Health::Unknown);
    }
}
