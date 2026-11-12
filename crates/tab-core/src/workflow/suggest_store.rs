//! Suggestions for one workflow ([`tab_domain::workflow::suggest`]) and
//! the compact performance report agents read over MCP
//! (`tab_workflow_stats`, `tab_workflow_failures`). Loads the workflow,
//! its runs and the model families, then leaves the adding up to the pure
//! domain code.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::Workflow;
use super::insights::windows;
use super::stats::{Quantiles, Spend, Stats, Window, health, summarize};
use super::suggest::{Suggestion, suggest};
use crate::store::Store;

/// One workflow's stats over a window, and what's suggested from them.
pub struct Report {
    pub workflow: Workflow,
    pub window: Window,
    pub days: u32,
    pub stats: Stats,
    pub suggestions: Vec<Suggestion>,
    pub health: super::stats::Health,
}

pub fn report(
    store: &Store,
    workflow_id: &str,
    days: u32,
    utc_offset_minutes: i32,
    now: DateTime<Utc>,
) -> anyhow::Result<Report> {
    let workflow = store
        .workflow_get(workflow_id)?
        .ok_or_else(|| anyhow::anyhow!("no workflow {workflow_id}"))?;
    let (window, _) = windows(now, days, utc_offset_minutes);
    let runs = store.workflow_runs_from(Some(workflow_id), window.from)?;
    let stats = summarize(&runs, &window);
    let suggestions = suggest(&workflow, &stats, &store.model_families()?);
    let days = ((window.to - window.from).num_days()).max(1) as u32;
    Ok(Report {
        health: health(&runs),
        workflow,
        window,
        days,
        stats,
        suggestions,
    })
}

/// The suggestions for the Insights panel.
pub fn workflow_suggestions(
    store: &Store,
    workflow_id: &str,
    days: u32,
    utc_offset_minutes: i32,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Suggestion>> {
    Ok(report(store, workflow_id, days, utc_offset_minutes, now)?.suggestions)
}

fn rate(n: u32, of: u32) -> Value {
    if of == 0 {
        Value::Null
    } else {
        json!(((n as f64 / of as f64) * 1000.0).round() / 1000.0)
    }
}

fn quantiles(q: &Quantiles) -> Value {
    if q.n == 0 {
        Value::Null
    } else {
        json!({ "p50_ms": q.p50, "p95_ms": q.p95 })
    }
}

/// Dollars only for API keys; subscriptions as tokens and plan share.
fn spend(s: &Spend) -> Value {
    let mut out = serde_json::Map::new();
    if s.cost_usd > 0.0 {
        out.insert(
            "api_usd".into(),
            json!((s.cost_usd * 10_000.0).round() / 10_000.0),
        );
    }
    if s.plan_input_tokens + s.plan_output_tokens > 0 {
        out.insert(
            "plan_tokens".into(),
            json!(s.plan_input_tokens + s.plan_output_tokens),
        );
        out.insert(
            "plan_share".into(),
            json!((s.plan_share * 1000.0).round() / 1000.0),
        );
    }
    if s.input_tokens + s.output_tokens > 0 {
        out.insert("tokens".into(), json!(s.input_tokens + s.output_tokens));
    }
    Value::Object(out)
}

fn suggestions_json(r: &Report) -> Vec<Value> {
    r.suggestions
        .iter()
        .map(|s| {
            json!({
                "kind": s.kind,
                "step": s.step_id,
                "title": s.title,
                "because": s.because,
                "change": s.change.is_some().then_some(
                    "The person can apply this as a draft in the Insights panel; or save your own version with tab_save_workflow.",
                ),
            })
        })
        .collect()
}

fn head(r: &Report) -> serde_json::Map<String, Value> {
    let c = &r.stats.counts;
    let mut out = serde_json::Map::new();
    out.insert("workflow_id".into(), json!(r.workflow.id));
    out.insert("workflow".into(), json!(r.workflow.name));
    out.insert("version".into(), json!(r.workflow.version));
    out.insert("enabled".into(), json!(r.workflow.enabled));
    out.insert("days".into(), json!(r.days));
    out.insert("from".into(), json!(r.window.from.to_rfc3339()));
    out.insert("health".into(), json!(r.health));
    out.insert(
        "runs".into(),
        json!({
            "did_work": c.runs, "succeeded": c.succeeded, "failed": c.failed,
            "unattended": c.unattended, "attended": c.attended, "unplanned": c.unplanned,
            "declined": r.stats.declined, "held_back": r.stats.held_back,
            "held_by_own_limit": r.stats.held_by_limit, "in_progress": r.stats.in_progress,
            "loop_stopped": r.stats.loop_stopped,
        }),
    );
    out.insert(
        "rates".into(),
        json!({
            "success": rate(c.succeeded, c.runs),
            "unattended_success": rate(c.unattended, c.runs),
            "intervention": rate(c.attended, c.runs),
        }),
    );
    out
}

/// `tab_workflow_stats`: rates, time and spend per step, models compared
/// and versions, with the suggestions. Rates are 0 to 1 over runs that did
/// work; declined and held-back runs are counted apart.
pub fn stats_json(r: &Report) -> Value {
    let s = &r.stats;
    let mut out = head(r);
    out.insert("duration".into(), quantiles(&s.duration_ms));
    out.insert("spend".into(), spend(&s.spend));
    out.insert(
        "steps".into(),
        s.steps
            .iter()
            .map(|st| {
                json!({
                    "id": st.id, "label": st.label, "runs": st.runs, "ok": st.ok, "failed": st.failed,
                    "failure_rate": rate(st.failed, st.runs), "skipped": st.skipped, "blocked": st.blocked,
                    "time": quantiles(&st.ms), "queue": quantiles(&st.queue_ms), "work": quantiles(&st.work_ms),
                    "waiting_on_person": quantiles(&st.human_wait_ms), "spend": spend(&st.spend),
                    "interventions": st.interventions, "unplanned_interventions": st.unplanned_interventions,
                    "approved": st.approved, "declined": st.declined, "fallbacks": st.fallbacks,
                })
            })
            .collect::<Value>(),
    );
    out.insert(
        "models".into(),
        s.models
            .iter()
            .map(|m| {
                json!({
                    "step": m.step_id, "harness": m.harness, "model": m.model, "family": m.family,
                    "runs": m.runs, "failed": m.failed, "unattended_rate": rate(m.unattended, m.runs),
                    "work": quantiles(&m.work_ms), "spend": spend(&m.spend),
                })
            })
            .collect::<Value>(),
    );
    out.insert(
        "versions".into(),
        s.versions
            .iter()
            .map(|v| {
                json!({
                    "version": v.version, "runs": v.counts.runs,
                    "success": rate(v.counts.succeeded, v.counts.runs),
                    "unattended_success": rate(v.counts.unattended, v.counts.runs),
                    "duration": quantiles(&v.duration_ms), "spend": spend(&v.spend),
                })
            })
            .collect::<Value>(),
    );
    out.insert("failure_groups".into(), json!(s.failures.len()));
    out.insert("suggestions".into(), json!(suggestions_json(r)));
    Value::Object(out)
}

/// `tab_workflow_failures`: what fails, grouped by step and error with
/// numbers and ids taken out, most frequent first, each with the latest
/// run it happened in (read it with `tab_get_workflow_run`).
pub fn failures_json(r: &Report) -> Value {
    let mut out = head(r);
    out.insert(
        "failure_groups".into(),
        r.stats
            .failures
            .iter()
            .map(|f| {
                json!({
                    "step": f.step_id, "label": f.label, "error": f.error, "count": f.count,
                    "first_seen": f.first_seen.to_rfc3339(), "last_seen": f.last_seen.to_rfc3339(),
                    "sample_run_id": f.sample_run,
                })
            })
            .collect::<Value>(),
    );
    out.insert("suggestions".into(), json!(suggestions_json(r)));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;
    use crate::workflow::{
        Intervention, InterventionKind, Run, RunStatus, StepRun, StepStatus, templates,
    };

    fn run(
        id: &str,
        workflow: &str,
        status: RunStatus,
        at: DateTime<Utc>,
        steps: Vec<StepRun>,
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
            steps,
            error: None,
            started_at: at,
            finished_at: Some(at + Duration::seconds(3)),
            runner: String::new(),
            resume: None,
            rerun_of: None,
            started_by: None,
            caused_by: None,
            approved: Vec::new(),
        }
    }

    fn step(id: &str, status: StepStatus, error: Option<&str>) -> StepRun {
        StepRun {
            id: id.into(),
            label: id.into(),
            status,
            input: Value::Null,
            output: Value::Null,
            error: error.map(str::to_string),
            ms: 40,
            metrics: None,
            interventions: vec![],
        }
    }

    #[test]
    fn reports_a_workflows_stats_failures_and_suggestions_for_agents() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        // A workflow with a destructive step the person always approves.
        let mut w = templates()[0].clone();
        let first = w.steps[0].id.clone();
        w.steps.truncate(1);
        let w = s.workflow_save(&w, "user", now).unwrap();
        for n in 0..12 {
            let mut ok = step(&first, StepStatus::Ok, None);
            ok.interventions.push(Intervention {
                kind: InterventionKind::Approval,
                at: now,
            });
            let r = run(
                &format!("ok{n}"),
                &w.id,
                RunStatus::Succeeded,
                now - Duration::minutes(n * 10 + 5),
                vec![ok],
            );
            assert!(s.workflow_run_insert(&r).unwrap());
        }
        for n in 0..2 {
            let r = run(
                &format!("bad{n}"),
                &w.id,
                RunStatus::Failed,
                now - Duration::hours(n + 3),
                vec![step(
                    &first,
                    StepStatus::Failed,
                    Some(&format!("GitHub said 50{n}")),
                )],
            );
            assert!(s.workflow_run_insert(&r).unwrap());
        }
        let mut declined = run(
            "no",
            &w.id,
            RunStatus::Stopped,
            now - Duration::hours(1),
            vec![step(
                &first,
                StepStatus::Blocked,
                Some("You declined this step."),
            )],
        );
        declined.error = Some("You declined it.".into());
        assert!(s.workflow_run_insert(&declined).unwrap());

        let r = report(&s, &w.id, 7, 0, now).unwrap();
        assert_eq!((r.stats.counts.runs, r.stats.declined), (14, 1));
        // One decline: the approval suggestion stays quiet.
        assert!(r.suggestions.is_empty());

        let v = stats_json(&r);
        assert_eq!(v["runs"]["did_work"], 14);
        assert_eq!(v["runs"]["declined"], 1);
        assert_eq!(v["rates"]["success"], 0.857);
        assert_eq!(v["steps"][0]["approved"], 12);
        assert_eq!(v["failure_groups"], 1);
        let f = failures_json(&r);
        assert_eq!(f["failure_groups"][0]["error"], "GitHub said #");
        assert_eq!(f["failure_groups"][0]["count"], 2);
        assert_eq!(f["failure_groups"][0]["sample_run_id"], "bad0");

        // Had they said yes that time too, it's pointed out, with no change.
        declined.status = RunStatus::Succeeded;
        declined.steps[0].status = StepStatus::Ok;
        declined.steps[0].interventions.push(Intervention {
            kind: InterventionKind::Approval,
            at: now,
        });
        s.workflow_run_update(&declined).unwrap();
        let r = report(&s, &w.id, 7, 0, now).unwrap();
        assert_eq!(r.suggestions.len(), 1);
        assert!(r.suggestions[0].change.is_none());
        let v = stats_json(&r);
        assert_eq!(v["suggestions"][0]["kind"], "always_approved");
        assert!(v["suggestions"][0]["change"].is_null());

        assert!(report(&s, "nope", 7, 0, now).is_err());
    }
}
