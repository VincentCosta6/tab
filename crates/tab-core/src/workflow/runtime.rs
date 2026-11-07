//! Runs workflows off the engine's event stream.
//!
//! The runtime is one more subscriber to [`EngineEvent`]s. For each item
//! change it finds the enabled workflows whose trigger matches, checks the
//! guardrails (loops, dedup, rate, cool-down), and runs each one's steps in
//! order. Every run is stored with its steps' inputs and outputs, so the
//! builder can show exactly what happened and why.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use super::{
    GLOBAL_RUNS_PER_HOUR, MAX_ACTIONS_PER_RUN, MAX_CHAIN, StepKind, Trigger, Workflow, all_hold,
    describe_trigger, find_action, glob, template,
};
use crate::connector::Connector;
use crate::engine::{Change, EngineEvent};
use crate::gateway::GatewayError;
use crate::model::{ConnectorInfo, Effect, QueueItem};
use crate::ratelimit;
use crate::store::Store;

/// One action may take this long.
const STEP_TIMEOUT: Duration = Duration::from_secs(60);
/// How long an item a run touched remembers that run's chain.
const PROVENANCE_TTL: Duration = Duration::from_secs(10 * 60);
/// Runs at once.
const CONCURRENT_RUNS: usize = 2;

pub use tab_domain::workflow::run::{Run, RunStatus, StepRun, StepStatus};

/// What the runtime needs from the engine.
#[async_trait]
pub trait Host: Send + Sync + 'static {
    fn store(&self) -> &Store;
    /// Connector types with their actions and whether they're installed.
    fn catalog(&self) -> Vec<ConnectorInfo>;
    fn connector(&self, id: &str) -> Option<Arc<dyn Connector>>;
    /// Runs one of tab's own actions. `chain` is stamped on what it creates.
    /// Returns the output and the item it changed, if any.
    async fn tab_action(
        &self,
        action: &str,
        input: &Value,
        chain: &[String],
        workflow: &str,
    ) -> anyhow::Result<(Value, Option<String>)>;
    fn publish(&self, e: EngineEvent);
}

/// Where a run came from.
struct Start {
    trigger: String,
    key: Option<String>,
    item_id: Option<String>,
    chain: Vec<String>,
    ctx: Value,
    dry_run: bool,
}

pub struct Runtime {
    host: Arc<dyn Host>,
    cache: Mutex<(u64, Arc<Vec<Workflow>>)>,
    /// item id → chain of the run that last changed it.
    provenance: Mutex<HashMap<String, (Instant, Vec<String>)>>,
    recent: Mutex<VecDeque<Instant>>,
    /// (workflow, reason) → when a refused run was last written down, so a
    /// storm of refusals leaves one row an hour, not thousands.
    noted: Mutex<HashMap<(String, RunStatus), Instant>>,
    slots: Semaphore,
}

fn item_json(item: &QueueItem) -> Value {
    let mut v = serde_json::to_value(item).unwrap_or(Value::Null);
    if let Some(r) = &item.ranking {
        v["score"] = r.score.into();
        v["urgency"] = serde_json::to_value(r.urgency).unwrap_or(Value::Null);
    }
    v
}

fn change_name(c: Change) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn base_ctx(w: &Workflow, trigger: Value, item: Option<&QueueItem>, signal: Value) -> Value {
    json!({
        "trigger": trigger,
        "item": item.map(item_json).unwrap_or(Value::Null),
        "signal": signal,
        "steps": {},
        "workflow": { "id": w.id, "name": w.name },
        "now": Utc::now().to_rfc3339(),
    })
}

fn budget_error(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<GatewayError>(),
        Some(GatewayError::Deferred { .. } | GatewayError::RateLimited { .. })
    )
}

impl Runtime {
    pub fn new(host: Arc<dyn Host>) -> Arc<Self> {
        Arc::new(Self {
            host,
            cache: Mutex::new((u64::MAX, Arc::new(Vec::new()))),
            provenance: Mutex::new(HashMap::new()),
            recent: Mutex::new(VecDeque::new()),
            noted: Mutex::new(HashMap::new()),
            slots: Semaphore::new(CONCURRENT_RUNS),
        })
    }

    /// Enabled workflows, re-read only when the store says they changed.
    fn enabled(&self) -> Arc<Vec<Workflow>> {
        let rev = self.host.store().workflows_rev();
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if c.0 != rev {
            match self.host.store().workflows() {
                Ok(all) => {
                    *c = (
                        rev,
                        Arc::new(all.into_iter().filter(|w| w.enabled).collect()),
                    )
                }
                Err(e) => tracing::error!(error = %e, "loading workflows failed"),
            }
        }
        c.1.clone()
    }

    /// The chain that led to this item changing, if a run did it.
    fn chain_for(&self, item: &QueueItem) -> Vec<String> {
        let mut p = self.provenance.lock().unwrap_or_else(|e| e.into_inner());
        p.retain(|_, (at, _)| at.elapsed() < PROVENANCE_TTL);
        if let Some((_, chain)) = p.get(&item.id) {
            return chain.clone();
        }
        item.event.metadata["workflow_chain"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn remember(&self, item_id: &str, chain: Vec<String>) {
        self.provenance
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(item_id.to_string(), (Instant::now(), chain));
    }

    /// Feed every engine event here.
    pub fn on_event(self: &Arc<Self>, e: &EngineEvent) {
        if let EngineEvent::ItemChanged { change, item } = e {
            self.on_item(*change, item);
        }
    }

    fn on_item(self: &Arc<Self>, change: Change, item: &QueueItem) {
        let workflows = self.enabled();
        if workflows.is_empty() {
            return;
        }
        let chain = self.chain_for(item);
        for w in workflows.iter() {
            let Trigger::Item {
                changes,
                sources,
                kinds,
                when,
            } = &w.trigger
            else {
                continue;
            };
            if !changes.contains(&change)
                || (!sources.is_empty() && !sources.contains(&item.event.source))
                || (!kinds.is_empty() && !kinds.iter().any(|k| glob(k, &item.event.kind)))
            {
                continue;
            }
            let ctx = base_ctx(
                w,
                json!({ "type": "item", "change": change_name(change) }),
                Some(item),
                Value::Null,
            );
            if !all_hold(when, &ctx) {
                continue;
            }
            self.clone().spawn(
                w.clone(),
                Start {
                    trigger: format!(
                        "{} {}: {}",
                        item.event.kind,
                        change_name(change),
                        item.event.title
                    ),
                    key: Some(format!(
                        "{}:{}:{}",
                        change_name(change),
                        item.id,
                        item.updated_at.timestamp_millis()
                    )),
                    item_id: Some(item.id.clone()),
                    chain: chain.clone(),
                    ctx,
                    dry_run: false,
                },
            );
        }
    }

    fn signal(
        self: &Arc<Self>,
        name: &str,
        payload: Value,
        chain: Vec<String>,
        item: Option<String>,
    ) {
        for w in self.enabled().iter() {
            let Trigger::Signal { name: on, when } = &w.trigger else {
                continue;
            };
            if on != name {
                continue;
            }
            let ctx = base_ctx(
                w,
                json!({ "type": "signal", "name": name }),
                None,
                payload.clone(),
            );
            if !all_hold(when, &ctx) {
                continue;
            }
            self.clone().spawn(
                w.clone(),
                Start {
                    trigger: format!("signal {name}"),
                    key: None,
                    item_id: item.clone(),
                    chain: chain.clone(),
                    ctx,
                    dry_run: false,
                },
            );
        }
    }

    /// Starts scheduled workflows whose slot came round. Call every ~30s.
    pub fn tick(self: &Arc<Self>, now: DateTime<Utc>) {
        for w in self.enabled().iter() {
            let Trigger::Schedule { every_minutes } = w.trigger else {
                continue;
            };
            let every = i64::from(every_minutes.max(super::MIN_SCHEDULE_MINUTES)) * 60;
            let slot = now.timestamp() / every;
            let ctx = base_ctx(w, json!({ "type": "schedule" }), None, Value::Null);
            self.clone().spawn(
                w.clone(),
                Start {
                    trigger: describe_trigger(&w.trigger),
                    key: Some(format!("schedule:{slot}")),
                    item_id: None,
                    chain: Vec::new(),
                    ctx,
                    dry_run: false,
                },
            );
        }
    }

    /// Runs a workflow now, on an item if given, and waits for it. Used by
    /// "Run" and "Dry run" in the builder and by agents (dry only). The
    /// workflow doesn't have to be saved or on for a dry run.
    pub async fn run_now(
        self: &Arc<Self>,
        w: Workflow,
        item: Option<QueueItem>,
        dry_run: bool,
    ) -> anyhow::Result<Run> {
        let change = item.as_ref().map(|_| "manual");
        let ctx = base_ctx(
            &w,
            json!({ "type": "manual", "change": change }),
            item.as_ref(),
            Value::Null,
        );
        let start = Start {
            trigger: match &item {
                Some(i) => format!("by hand on {}", i.event.title),
                None => "by hand".into(),
            },
            key: None,
            item_id: item.map(|i| i.id),
            chain: Vec::new(),
            ctx,
            dry_run,
        };
        let run = self.clone().begin(&w, start).await?;
        Ok(run)
    }

    fn spawn(self: Arc<Self>, w: Workflow, start: Start) {
        tokio::spawn(async move {
            if let Err(e) = self.begin(&w, start).await {
                tracing::error!(workflow = %w.name, error = %e, "workflow run failed to start");
            }
        });
    }

    fn new_run(w: &Workflow, s: &Start, status: RunStatus) -> Run {
        Run {
            id: uuid::Uuid::new_v4().to_string(),
            workflow_id: w.id.clone(),
            workflow_name: w.name.clone(),
            version: w.version,
            status,
            dry_run: s.dry_run,
            trigger: s.trigger.clone(),
            trigger_key: s.key.clone(),
            item_id: s.item_id.clone(),
            chain: s.chain.clone(),
            steps: Vec::new(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
        }
    }

    /// Writes down a refused run, at most once an hour per workflow and reason.
    fn refuse(
        &self,
        w: &Workflow,
        s: &Start,
        status: RunStatus,
        why: String,
    ) -> anyhow::Result<Run> {
        tracing::info!(workflow = %w.name, reason = %why, "workflow run refused");
        let mut run = Self::new_run(w, s, status);
        run.trigger_key = None;
        run.error = Some(why);
        run.finished_at = Some(run.started_at);
        let due = {
            let mut n = self.noted.lock().unwrap_or_else(|e| e.into_inner());
            let k = (w.id.clone(), status);
            let due = n
                .get(&k)
                .is_none_or(|t| t.elapsed() > Duration::from_secs(3600));
            if due {
                n.insert(k, Instant::now());
            }
            due
        };
        if due && !s.dry_run && !w.id.is_empty() {
            self.host.store().workflow_run_insert(&run)?;
            self.host
                .publish(EngineEvent::WorkflowRun { run: run.clone() });
        }
        Ok(run)
    }

    /// Guardrails, then the run itself.
    async fn begin(self: Arc<Self>, w: &Workflow, s: Start) -> anyhow::Result<Run> {
        if !w.id.is_empty() && s.chain.contains(&w.id) {
            let names = self.names(&s.chain);
            return self.refuse(
                w,
                &s,
                RunStatus::LoopStopped,
                format!(
                    "Stopped a loop: this run was caused by {}, which this workflow started.",
                    names.join(" → ")
                ),
            );
        }
        if s.chain.len() >= MAX_CHAIN {
            let names = self.names(&s.chain);
            return self.refuse(
                w,
                &s,
                RunStatus::LoopStopped,
                format!(
                    "Stopped a chain {MAX_CHAIN} workflows long ({}).",
                    names.join(" → ")
                ),
            );
        }
        let store = self.host.store();
        let now = Utc::now();
        if !s.dry_run && !w.id.is_empty() {
            if let Some(key) = &s.key
                && store.workflow_run_seen(&w.id, key)?
            {
                return Ok(Self::new_run(w, &s, RunStatus::Stopped));
            }
            let hour = store.workflow_runs_since(&w.id, now - chrono::Duration::hours(1))?;
            if hour >= w.limits.max_runs_per_hour as usize {
                return self.refuse(w, &s, RunStatus::Throttled, format!(
                    "Hit its limit of {} runs an hour; later triggers are skipped until the hour rolls over.",
                    w.limits.max_runs_per_hour));
            }
            if w.limits.cooldown_minutes > 0
                && let Some(item) = &s.item_id
                && let Some(last) = store.workflow_last_run_for_item(&w.id, item)?
                && now - last < chrono::Duration::minutes(i64::from(w.limits.cooldown_minutes))
            {
                return Ok(Self::new_run(w, &s, RunStatus::Stopped));
            }
        }
        {
            let mut r = self.recent.lock().unwrap_or_else(|e| e.into_inner());
            while r
                .front()
                .is_some_and(|t| t.elapsed() > Duration::from_secs(3600))
            {
                r.pop_front();
            }
            if r.len() >= GLOBAL_RUNS_PER_HOUR {
                drop(r);
                return self.refuse(w, &s, RunStatus::Throttled, format!(
                    "All workflows together hit {GLOBAL_RUNS_PER_HOUR} runs an hour; this one was skipped."));
            }
            r.push_back(Instant::now());
        }

        let mut run = Self::new_run(w, &s, RunStatus::Running);
        let saved = !s.dry_run && !w.id.is_empty();
        if saved && !store.workflow_run_insert(&run)? {
            // The same trigger won a race to start.
            return Ok(run);
        }
        let _slot = self.slots.acquire().await?;
        self.host
            .publish(EngineEvent::WorkflowRun { run: run.clone() });
        self.execute(w, &mut run, s.ctx).await;
        run.finished_at = Some(Utc::now());
        if saved {
            store.workflow_run_update(&run)?;
        }
        self.host
            .publish(EngineEvent::WorkflowRun { run: run.clone() });
        Ok(run)
    }

    fn names(&self, chain: &[String]) -> Vec<String> {
        let all = self.host.store().workflows().unwrap_or_default();
        chain
            .iter()
            .map(|id| {
                all.iter()
                    .find(|w| &w.id == id)
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| id.clone())
            })
            .collect()
    }

    async fn execute(self: &Arc<Self>, w: &Workflow, run: &mut Run, mut ctx: Value) {
        let catalog = self.host.catalog();
        let mut chain = run.chain.clone();
        if !w.id.is_empty() {
            chain.push(w.id.clone());
        }
        let trigger_item = run.item_id.clone();
        let mut actions = 0;
        // A command step without a folder of its own gets this run's
        // scratch folder, shared by the run's steps and removed after.
        let mut scratch: Option<std::path::PathBuf> = None;
        run.status = RunStatus::Succeeded;
        for step in &w.steps {
            let started = Instant::now();
            let mut sr = StepRun {
                id: step.id.clone(),
                label: step.label.clone(),
                status: StepStatus::Ok,
                input: Value::Null,
                output: Value::Null,
                error: None,
                ms: 0,
            };
            if !all_hold(&step.when, &ctx) {
                sr.status = StepStatus::Skipped;
                run.steps.push(sr);
                continue;
            }
            let mut stop: Option<RunStatus> = None;
            match &step.kind {
                StepKind::Command {
                    program,
                    args,
                    folder,
                    network,
                    env,
                    image,
                    timeout_secs,
                } => {
                    let text = |s: &str| {
                        let v = template::render(&Value::String(s.to_string()), &ctx);
                        match v {
                            Value::String(s) => s,
                            Value::Null => String::new(),
                            other => other.to_string(),
                        }
                    };
                    let args: Vec<String> = args
                        .iter()
                        .filter(|a| !a.trim().is_empty())
                        .map(|a| text(a))
                        .collect();
                    let folder = text(folder);
                    let env: Vec<(String, String)> = env
                        .iter()
                        .map(|(k, v)| {
                            let v = template::render(v, &ctx);
                            (
                                k.clone(),
                                v.as_str().map(str::to_string).unwrap_or_else(|| {
                                    if v.is_null() {
                                        String::new()
                                    } else {
                                        v.to_string()
                                    }
                                }),
                            )
                        })
                        .collect();
                    sr.input = json!({
                        "program": program,
                        "args": args,
                        "folder": folder,
                        "network": network,
                        "image": image,
                        "env": env.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
                    });
                    if run.dry_run {
                        sr.status = StepStatus::Dry;
                    } else if !w.allow_commands {
                        sr.status = StepStatus::Blocked;
                        sr.error = Some(
                            "Commands are blocked. Allow commands on this workflow to let it run programs."
                                .into(),
                        );
                        stop = Some(RunStatus::Blocked);
                    } else {
                        let dir = if folder.trim().is_empty() {
                            let d = scratch.get_or_insert_with(|| {
                                std::env::temp_dir().join("tab-runs").join(&run.id)
                            });
                            d.clone()
                        } else {
                            std::path::PathBuf::from(folder.trim())
                        };
                        let secs = if *timeout_secs == 0 {
                            super::DEFAULT_COMMAND_SECS
                        } else {
                            (*timeout_secs).min(super::MAX_COMMAND_SECS)
                        };
                        let spec = super::command::Spec {
                            program: program.trim().to_string(),
                            args,
                            folder: dir,
                            network: crate::sandbox::Network::from_list(network),
                            env,
                            image: Some(image.trim().to_string()).filter(|i| !i.is_empty()),
                            timeout: Duration::from_secs(u64::from(secs)),
                        };
                        match super::command::run(spec).await {
                            Ok(done) => {
                                sr.output = done.output;
                                if done.exit_code != Some(0) {
                                    sr.status = StepStatus::Failed;
                                    let err = sr.output["stderr"].as_str().unwrap_or_default();
                                    let last = err
                                        .lines()
                                        .rev()
                                        .find(|l| !l.trim().is_empty())
                                        .unwrap_or_default();
                                    sr.error = Some(match done.exit_code {
                                        Some(c) => format!(
                                            "{program} exited with {c}{}",
                                            if last.is_empty() {
                                                String::new()
                                            } else {
                                                format!(": {last}")
                                            }
                                        ),
                                        None => format!("{program} was killed"),
                                    });
                                }
                            }
                            Err(e) => {
                                sr.status = StepStatus::Failed;
                                sr.error = Some(format!("{e:#}"));
                            }
                        }
                    }
                }
                StepKind::Filter { conditions } => {
                    let failed: Vec<String> = conditions
                        .iter()
                        .filter(|c| !c.holds(&ctx))
                        .map(|c| {
                            let got = template::lookup(&ctx, &c.field).unwrap_or(Value::Null);
                            format!("{} {:?} {} (was {})", c.field, c.op, c.value, got)
                        })
                        .collect();
                    if !failed.is_empty() {
                        sr.status = StepStatus::Stopped;
                        sr.output = json!({ "failed": failed });
                        stop = Some(RunStatus::Stopped);
                    }
                }
                StepKind::Set { values } => {
                    sr.output = template::render(&Value::Object(values.clone()), &ctx);
                }
                StepKind::Emit { signal, payload } => {
                    let p = template::render(&Value::Object(payload.clone()), &ctx);
                    sr.input = json!({ "signal": signal, "payload": p });
                    if run.dry_run {
                        sr.status = StepStatus::Dry;
                    } else {
                        self.signal(signal, p, chain.clone(), trigger_item.clone());
                    }
                }
                StepKind::Action {
                    connector,
                    action,
                    input,
                } => {
                    let rendered = template::render(&Value::Object(input.clone()), &ctx);
                    sr.input = rendered.clone();
                    actions += 1;
                    match find_action(&catalog, connector, action) {
                        None => {
                            sr.status = StepStatus::Failed;
                            sr.error = Some(format!("{connector} has no action {action}"));
                        }
                        Some(_) if actions > MAX_ACTIONS_PER_RUN => {
                            sr.status = StepStatus::Failed;
                            sr.error = Some(format!(
                                "more than {MAX_ACTIONS_PER_RUN} actions in one run"
                            ));
                        }
                        Some(info)
                            if info.effect == Effect::Destructive
                                && !w.allow_destructive
                                && !run.dry_run =>
                        {
                            sr.status = StepStatus::Blocked;
                            sr.error = Some(format!(
                                "{} is destructive. Allow destructive steps on this workflow to let it run.",
                                info.label
                            ));
                            stop = Some(RunStatus::Blocked);
                        }
                        Some(info) if run.dry_run && info.effect != Effect::Read => {
                            sr.status = StepStatus::Dry;
                            sr.output = info
                                .outputs
                                .iter()
                                .map(|f| (f.key.clone(), Value::String(format!("<{}>", f.label))))
                                .collect::<serde_json::Map<_, _>>()
                                .into();
                        }
                        Some(info) => {
                            if info.effect != Effect::Read && !w.id.is_empty() {
                                // Changes this causes carry the chain. Noted
                                // before acting: the change event can arrive
                                // before the action returns.
                                let target = rendered["item_id"].as_str().map(str::to_string);
                                for id in target.iter().chain(trigger_item.iter()) {
                                    self.remember(id, chain.clone());
                                }
                            }
                            let fut = async {
                                if connector == crate::connectors::tab::SOURCE {
                                    self.host
                                        .tab_action(action, &rendered, &chain, &w.name)
                                        .await
                                } else {
                                    let c = self.host.connector(connector).ok_or_else(|| {
                                        anyhow::anyhow!(
                                            "{connector} isn't installed or isn't running"
                                        )
                                    })?;
                                    // Nobody is waiting: a tight budget defers it.
                                    ratelimit::background(c.act(action, &rendered))
                                        .await
                                        .map(|v| (v, None))
                                }
                            };
                            match tokio::time::timeout(STEP_TIMEOUT, fut).await {
                                Err(_) => {
                                    sr.status = StepStatus::Failed;
                                    sr.error = Some(format!(
                                        "took longer than {}s",
                                        STEP_TIMEOUT.as_secs()
                                    ));
                                }
                                Ok(Ok((out, touched))) => {
                                    sr.output = out;
                                    if info.effect != Effect::Read && !w.id.is_empty() {
                                        for id in touched.iter() {
                                            self.remember(id, chain.clone());
                                        }
                                    }
                                }
                                Ok(Err(e)) if budget_error(&e) => {
                                    sr.status = StepStatus::Throttled;
                                    sr.error =
                                        Some(format!("Held back to protect the rate budget: {e}"));
                                    stop = Some(RunStatus::Throttled);
                                }
                                Ok(Err(e)) => {
                                    sr.status = StepStatus::Failed;
                                    sr.error = Some(format!("{e:#}"));
                                }
                            }
                        }
                    }
                }
            }
            sr.ms = started.elapsed().as_millis() as u64;
            if sr.status == StepStatus::Failed && !step.continue_on_error && stop.is_none() {
                stop = Some(RunStatus::Failed);
                run.error = sr.error.clone();
            }
            ctx["steps"][&step.id] = if sr.output.is_null() {
                json!({})
            } else {
                sr.output.clone()
            };
            run.steps.push(sr);
            if let Some(st) = stop {
                run.status = st;
                if run.error.is_none() {
                    run.error = run.steps.last().and_then(|s| s.error.clone());
                }
                break;
            }
        }
        if let Some(d) = scratch {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
