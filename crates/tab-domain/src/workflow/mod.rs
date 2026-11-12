//! Workflows: "when this happens, do these steps", built from what
//! connectors declare.
//!
//! A workflow is data ([`Workflow`]): one [`Trigger`] and an ordered list of
//! [`Step`]s. Steps call connector actions, stop the run unless conditions
//! hold, compute values, or emit a signal other workflows can start on.
//! Later steps read earlier results through templates
//! (`{{steps.pr.state}}`), so actions feed each other.
//!
//! Runs are deterministic: the same trigger and the same action results
//! produce the same steps with the same inputs. Nothing here calls a model.
//!
//! Guardrails, enforced in the desktop runtime
//! (`tab_core::workflow::runtime`) and checked before saving by [`validate`]:
//! * **Loops.** Every run carries the chain of workflows that caused it.
//!   A workflow never runs again inside its own chain, and chains stop at
//!   [`MAX_CHAIN`] links. Saving refuses workflows that could trigger each
//!   other in a circle.
//! * **Dedup.** The same trigger (change, item, item version) runs a
//!   workflow once.
//! * **Rate.** Each workflow has runs-per-hour and per-item cool-down
//!   limits, there is a global cap, and connector actions run at background
//!   priority, so the gateway defers them before they eat into the budget
//!   interactive use needs.
//! * **Consent.** Agents can draft and dry-run workflows but only the person
//!   turns them on, and destructive actions (merge, close, move mail) stay
//!   blocked until the person allows them on that workflow.

pub mod insights;
pub mod item_status;
pub mod run;
pub mod stats;
pub mod template;

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::model::Change;
use crate::model::{ActionInfo, ConnectorInfo, Effect};

pub use run::{
    Billing, Intervention, InterventionKind, Resume, Run, RunStatus, StepApproval, StepMetrics,
    StepRun, StepStatus,
};

/// A run that was caused by another run stops after this many links.
pub const MAX_CHAIN: usize = 3;
/// Steps per workflow.
pub const MAX_STEPS: usize = 20;
/// Connector actions per run.
pub const MAX_ACTIONS_PER_RUN: usize = 10;
/// Runs per hour across every workflow.
pub const GLOBAL_RUNS_PER_HOUR: usize = 200;
/// The shortest schedule.
pub const MIN_SCHEDULE_MINUTES: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Workflow {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Only the person turns a workflow on.
    #[serde(default)]
    pub enabled: bool,
    /// The person allowed destructive steps (merge, close, move mail).
    #[serde(default)]
    pub allow_destructive: bool,
    /// The person allowed command steps to run programs on this computer.
    #[serde(default)]
    pub allow_commands: bool,
    pub trigger: Trigger,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub limits: Limits,
    /// `user`, `agent` or `template`.
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trigger {
    /// Something happened to a queue item.
    Item {
        /// Which changes count. Usually just `ingested`.
        #[serde(default)]
        changes: Vec<Change>,
        /// Connector ids; empty means any.
        #[serde(default)]
        sources: Vec<String>,
        /// Kinds, exact or `prefix.*`; empty means any.
        #[serde(default)]
        kinds: Vec<String>,
        /// Only when these all hold.
        #[serde(default)]
        when: Vec<Condition>,
    },
    /// Another workflow emitted this signal.
    Signal {
        name: String,
        #[serde(default)]
        when: Vec<Condition>,
    },
    /// Every so often.
    Schedule { every_minutes: u32 },
    /// Only when the person (or an agent, dry) starts it.
    Manual,
}

impl Default for Trigger {
    fn default() -> Self {
        Trigger::Item {
            changes: vec![Change::Ingested],
            sources: Vec::new(),
            kinds: Vec::new(),
            when: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Limits {
    /// Runs that do work, per hour. Further triggers are dropped.
    #[serde(default = "default_runs_per_hour")]
    pub max_runs_per_hour: u32,
    /// Minimum time between runs for the same item. 0 means none.
    #[serde(default)]
    pub cooldown_minutes: u32,
}

fn default_runs_per_hour() -> u32 {
    30
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_runs_per_hour: default_runs_per_hour(),
            cooldown_minutes: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Step {
    /// Short and unique in the workflow; later steps read `{{steps.<id>...}}`.
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// The step only runs when these hold; otherwise it's skipped.
    #[serde(default)]
    pub when: Vec<Condition>,
    /// Keep going when this step fails.
    #[serde(default)]
    pub continue_on_error: bool,
    #[serde(flatten)]
    pub kind: StepKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StepKind {
    /// Run a connector action. Input values may hold templates.
    Action {
        connector: String,
        action: String,
        #[serde(default)]
        input: Map<String, Value>,
    },
    /// Stop the run here unless every condition holds.
    Filter { conditions: Vec<Condition> },
    /// Compute values for later steps.
    Set { values: Map<String, Value> },
    /// Start workflows listening for this signal, with this payload.
    Emit {
        signal: String,
        #[serde(default)]
        payload: Map<String, Value>,
    },
    /// Run a program in tab's sandbox. Arguments are passed as they are,
    /// never through a shell, so templated values can't become commands.
    Command {
        program: String,
        #[serde(default)]
        args: Vec<String>,
        /// The folder it runs in and may change. Empty means a fresh folder
        /// for the run, removed when the run ends.
        #[serde(default)]
        folder: String,
        /// Hosts it may reach: empty for none, `*` for any.
        #[serde(default)]
        network: Vec<String>,
        /// Extra environment. Nothing of tab's own environment that looks
        /// secret is passed on.
        #[serde(default)]
        env: Map<String, Value>,
        /// Run in this container image (Docker or Podman) instead.
        #[serde(default)]
        image: String,
        /// 0 means the default.
        #[serde(default)]
        timeout_secs: u32,
    },
    /// Hand work to an agent on one of the person's runners (a computer,
    /// or tab cloud on a paid plan) and wait for it. The run pauses here as
    /// a job (see [`crate::runner`]) and carries on when a runner finishes
    /// it: `{{steps.<id>.summary}}`, `.branch` and `.pr_url` read what came
    /// of it.
    Agent {
        /// What to ask the agent. May hold templates.
        prompt: String,
        /// `owner/name` to work in. May hold templates. Empty: no repo.
        #[serde(default)]
        repo: String,
        /// A harness id (`claude_code`, `opencode`, `codex`, `pi`). Empty:
        /// the runner's default.
        #[serde(default)]
        harness: String,
        #[serde(default)]
        model: String,
        /// A model family ([`crate::families`]) to run on, used when
        /// `harness` and `model` are empty: the runner takes the first of
        /// its models it can run, and the next if one is unavailable.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        family: String,
        #[serde(default)]
        placement: crate::runner::Placement,
        /// Fail the step if no runner finishes it within this long. 0 means
        /// the default ([`crate::runner::DEFAULT_JOB_MINUTES`]).
        #[serde(default)]
        deadline_minutes: u32,
    },
}

/// Agent steps per workflow.
pub const MAX_AGENT_STEPS: usize = 3;

/// Command steps per workflow.
pub const MAX_COMMANDS: usize = 5;
/// A command's default and longest time.
pub const DEFAULT_COMMAND_SECS: u32 = 600;
pub const MAX_COMMAND_SECS: u32 = 3600;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Condition {
    /// A path into the run's context, like `item.kind` or `steps.pr.state`.
    pub field: String,
    pub op: Op,
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Equals,
    NotEquals,
    Contains,
    NotContains,
    StartsWith,
    /// `*` wildcards, case-insensitive: `github.*`, `*bot*`.
    Matches,
    /// The value is a list (or comma separated) and the field is one of them.
    In,
    Gt,
    Lt,
    Exists,
    Missing,
}

fn as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn as_num(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// `*` wildcards, case-insensitive.
pub fn glob(pattern: &str, s: &str) -> bool {
    let (p, s) = (pattern.to_lowercase(), s.to_lowercase());
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == s;
    }
    let mut rest = s.as_str();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
    }
    true
}

impl Condition {
    pub fn holds(&self, ctx: &Value) -> bool {
        let v = template::lookup(ctx, &self.field).unwrap_or(Value::Null);
        let want = template::render(&self.value, ctx);
        let (text, want_text) = (as_text(&v).to_lowercase(), as_text(&want).to_lowercase());
        match self.op {
            Op::Equals => v == want || (!v.is_object() && !v.is_array() && text == want_text),
            Op::NotEquals => !(v == want || (!v.is_object() && !v.is_array() && text == want_text)),
            Op::Contains => match &v {
                Value::Array(a) => a.iter().any(|x| as_text(x).to_lowercase() == want_text),
                _ => text.contains(&want_text),
            },
            Op::NotContains => match &v {
                Value::Array(a) => !a.iter().any(|x| as_text(x).to_lowercase() == want_text),
                _ => !text.contains(&want_text),
            },
            Op::StartsWith => text.starts_with(&want_text),
            Op::Matches => glob(&want_text, &text),
            Op::In => {
                let list: Vec<String> = match &want {
                    Value::Array(a) => a.iter().map(|x| as_text(x).to_lowercase()).collect(),
                    _ => want_text.split(',').map(|s| s.trim().to_string()).collect(),
                };
                list.contains(&text)
            }
            Op::Gt => matches!((as_num(&v), as_num(&want)), (Some(a), Some(b)) if a > b),
            Op::Lt => matches!((as_num(&v), as_num(&want)), (Some(a), Some(b)) if a < b),
            Op::Exists => !v.is_null() && !text.is_empty(),
            Op::Missing => v.is_null() || text.is_empty(),
        }
    }
}

pub fn all_hold(conditions: &[Condition], ctx: &Value) -> bool {
    conditions.iter().all(|c| c.holds(ctx))
}

/// A problem found before saving. Errors block saving with the workflow on;
/// warnings are shown in the builder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Issue {
    /// The step it's about, or `trigger`, or none for the whole workflow.
    pub at: Option<String>,
    pub error: bool,
    pub message: String,
}

impl Issue {
    fn err(at: Option<&str>, m: impl Into<String>) -> Self {
        Self {
            at: at.map(str::to_string),
            error: true,
            message: m.into(),
        }
    }
    fn warn(at: Option<&str>, m: impl Into<String>) -> Self {
        Self {
            at: at.map(str::to_string),
            error: false,
            message: m.into(),
        }
    }
}

/// Looks up an action in the connectors' descriptors.
pub fn find_action<'a>(
    catalog: &'a [ConnectorInfo],
    connector: &str,
    action: &str,
) -> Option<&'a ActionInfo> {
    catalog
        .iter()
        .find(|c| c.descriptor.id == connector)?
        .descriptor
        .actions
        .iter()
        .find(|a| a.id == action)
}

/// Checks a workflow against what connectors offer, the model families
/// there are (their ids) and the other workflows (for loops). `others` may
/// include an older copy of `w`.
pub fn validate(
    w: &Workflow,
    catalog: &[ConnectorInfo],
    others: &[Workflow],
    families: &[String],
) -> Vec<Issue> {
    let mut out = Vec::new();
    if w.name.trim().is_empty() {
        out.push(Issue::err(None, "Give the workflow a name."));
    }
    let has_item = match &w.trigger {
        Trigger::Item {
            changes,
            sources,
            kinds,
            when,
        } => {
            if changes.is_empty() {
                out.push(Issue::err(
                    Some("trigger"),
                    "Pick at least one change to start on, such as a new item.",
                ));
            }
            if sources.is_empty() && kinds.is_empty() && when.is_empty() {
                out.push(Issue::warn(Some("trigger"), "Runs for every item from every connection. Narrow it to a connection or kind if you can."));
            }
            for s in sources {
                if !catalog.iter().any(|c| &c.descriptor.id == s) {
                    out.push(Issue::err(
                        Some("trigger"),
                        format!("There's no connection called {s}."),
                    ));
                } else if !catalog.iter().any(|c| &c.descriptor.id == s && c.enabled) {
                    out.push(Issue::warn(
                        Some("trigger"),
                        format!("{s} isn't installed, so this won't start until it is."),
                    ));
                }
            }
            true
        }
        Trigger::Signal { name, .. } => {
            if name.trim().is_empty() {
                out.push(Issue::err(
                    Some("trigger"),
                    "Name the signal to listen for.",
                ));
            }
            false
        }
        Trigger::Schedule { every_minutes } => {
            if *every_minutes < MIN_SCHEDULE_MINUTES {
                out.push(Issue::err(
                    Some("trigger"),
                    format!("Schedules run at most every {MIN_SCHEDULE_MINUTES} minutes."),
                ));
            }
            false
        }
        // A manual run can be started on an item.
        Trigger::Manual => true,
    };
    if w.steps.is_empty() {
        out.push(Issue::warn(
            None,
            "Add a step for the workflow to do something.",
        ));
    }
    if w.steps.len() > MAX_STEPS {
        out.push(Issue::err(None, format!("At most {MAX_STEPS} steps.")));
    }
    let actions = w
        .steps
        .iter()
        .filter(|s| matches!(s.kind, StepKind::Action { .. }))
        .count();
    if actions > MAX_ACTIONS_PER_RUN {
        out.push(Issue::err(
            None,
            format!("At most {MAX_ACTIONS_PER_RUN} actions per workflow."),
        ));
    }
    let commands = w
        .steps
        .iter()
        .filter(|s| matches!(s.kind, StepKind::Command { .. }))
        .count();
    if commands > MAX_COMMANDS {
        out.push(Issue::err(
            None,
            format!("At most {MAX_COMMANDS} commands per workflow."),
        ));
    }
    let agents = w
        .steps
        .iter()
        .filter(|s| matches!(s.kind, StepKind::Agent { .. }))
        .count();
    if agents > MAX_AGENT_STEPS {
        out.push(Issue::err(
            None,
            format!("At most {MAX_AGENT_STEPS} agent steps per workflow."),
        ));
    }
    if w.limits.max_runs_per_hour == 0 {
        out.push(Issue::err(None, "Allow at least one run per hour."));
    }

    let mut seen: HashSet<&str> = HashSet::new();
    for step in &w.steps {
        let at = Some(step.id.as_str());
        if step.id.trim().is_empty()
            || !step
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            out.push(Issue::err(
                at,
                format!("Step ids are letters, digits and _, got {:?}.", step.id),
            ));
        }
        if !seen.insert(step.id.as_str()) {
            out.push(Issue::err(at, format!("Two steps are called {}.", step.id)));
        }
        // Templates may only read steps that already ran.
        let mut refs = Vec::new();
        for c in &step.when {
            refs.push(c.field.clone());
            refs.extend(template::refs(&c.value));
        }
        match &step.kind {
            StepKind::Action {
                connector,
                action,
                input,
            } => {
                for v in input.values() {
                    refs.extend(template::refs(v));
                }
                match find_action(catalog, connector, action) {
                    None => out.push(Issue::err(
                        at,
                        format!("{connector} has no action {action}."),
                    )),
                    Some(info) => {
                        if !catalog
                            .iter()
                            .any(|c| &c.descriptor.id == connector && c.enabled)
                        {
                            out.push(Issue::warn(
                                at,
                                format!("{connector} isn't installed, so this step will fail."),
                            ));
                        }
                        for f in info.inputs.iter().filter(|f| f.required) {
                            let given = input
                                .get(&f.key)
                                .is_some_and(|v| !as_text(v).trim().is_empty());
                            if !given {
                                out.push(Issue::err(
                                    at,
                                    format!("{}: {} is required.", info.label, f.label),
                                ));
                            }
                        }
                        for k in input.keys() {
                            if !info.inputs.iter().any(|f| &f.key == k) {
                                out.push(Issue::warn(
                                    at,
                                    format!("{} doesn't take {k}; it's ignored.", info.label),
                                ));
                            }
                        }
                        if info.effect == Effect::Destructive && !w.allow_destructive {
                            out.push(Issue::warn(at, format!("{} is destructive, so it stays blocked until you allow destructive steps.", info.label)));
                        }
                    }
                }
            }
            StepKind::Filter { conditions } => {
                if conditions.is_empty() {
                    out.push(Issue::warn(
                        at,
                        "This filter has no conditions, so it never stops the run.",
                    ));
                }
                for c in conditions {
                    refs.push(c.field.clone());
                    refs.extend(template::refs(&c.value));
                }
            }
            StepKind::Set { values } => {
                for v in values.values() {
                    refs.extend(template::refs(v));
                }
            }
            StepKind::Emit { signal, payload } => {
                if signal.trim().is_empty() {
                    out.push(Issue::err(at, "Name the signal."));
                }
                for v in payload.values() {
                    refs.extend(template::refs(v));
                }
            }
            StepKind::Command {
                program,
                args,
                folder,
                network,
                env,
                image,
                timeout_secs,
            } => {
                if program.trim().is_empty() {
                    out.push(Issue::err(at, "Name the program to run."));
                }
                if !template::refs(&Value::String(program.clone())).is_empty() {
                    out.push(Issue::err(
                        at,
                        "The program can't come from a template; pass values as arguments.",
                    ));
                }
                let shell = ["sh", "bash", "zsh", "cmd", "cmd.exe", "powershell", "pwsh"];
                let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
                if shell.contains(&base)
                    && args
                        .iter()
                        .any(|a| !template::refs(&Value::String(a.clone())).is_empty())
                {
                    out.push(Issue::warn(at, "A shell given templated text can be told to run anything that text says. Call the program directly instead."));
                }
                for a in args {
                    refs.extend(template::refs(&Value::String(a.clone())));
                }
                refs.extend(template::refs(&Value::String(folder.clone())));
                for v in env.values() {
                    refs.extend(template::refs(v));
                }
                if network.iter().any(|d| d.trim() == "*") {
                    out.push(Issue::warn(
                        at,
                        "This command can reach any host. List the hosts it needs if you can.",
                    ));
                }
                if *timeout_secs > MAX_COMMAND_SECS {
                    out.push(Issue::err(
                        at,
                        format!("Commands run at most {} minutes.", MAX_COMMAND_SECS / 60),
                    ));
                }
                if image.trim().is_empty() && !folder.trim().is_empty() {
                    out.push(Issue::warn(
                        at,
                        format!("This command can change everything in {folder}."),
                    ));
                }
                if !w.allow_commands {
                    out.push(Issue::warn(
                        at,
                        "Commands stay blocked until you allow commands on this workflow.",
                    ));
                }
            }
            StepKind::Agent {
                prompt,
                repo,
                harness,
                model,
                family,
                placement,
                ..
            } => {
                if prompt.trim().is_empty() {
                    out.push(Issue::err(at, "Say what the agent should do."));
                }
                refs.extend(template::refs(&Value::String(prompt.clone())));
                refs.extend(template::refs(&Value::String(repo.clone())));
                let known = ["claude_code", "opencode", "codex", "pi"];
                if !harness.is_empty() && !known.contains(&harness.as_str()) {
                    out.push(Issue::err(
                        at,
                        format!("{harness} isn't a harness tab knows."),
                    ));
                }
                if !family.is_empty() {
                    if !families.contains(family) {
                        out.push(Issue::err(
                            at,
                            format!("There's no model family called {family}."),
                        ));
                    } else if !harness.is_empty() || !model.is_empty() {
                        out.push(Issue::warn(
                            at,
                            format!("The pinned model wins, so the {family} family isn't used."),
                        ));
                    }
                }
                if matches!(placement, crate::runner::Placement::Cloud) {
                    out.push(Issue::warn(
                        at,
                        "Only tab cloud runners take this step, which needs a paid plan.",
                    ));
                }
            }
        }
        for r in refs {
            let root = r.split('.').next().unwrap_or_default();
            match root {
                "steps" => {
                    let id = r.split('.').nth(1).unwrap_or_default();
                    if !seen.contains(id) || id == step.id {
                        out.push(Issue::err(at, format!("{{{{{r}}}}} reads a step that hasn't run yet.")));
                    }
                }
                "item" if !has_item => {
                    out.push(Issue::warn(at, format!("{{{{{r}}}}} is empty: this trigger has no item.")));
                }
                "item" | "trigger" | "signal" | "workflow" | "now" => {}
                _ => out.push(Issue::err(at, format!("{{{{{r}}}}} isn't something a run knows. Use item, steps, signal, trigger or now."))),
            }
        }
    }

    if let Some(cycle) = find_loop(w, others) {
        out.push(Issue::err(
            None,
            format!(
                "This could loop: {}. Narrow a trigger (for example to other connections) or drop the step that feeds it.",
                cycle.join(" → ")
            ),
        ));
    }
    out
}

/// What a workflow's steps can cause that might start a workflow.
fn causes(w: &Workflow) -> (Vec<Change>, Vec<String>, bool) {
    let mut changes = Vec::new();
    let mut signals = Vec::new();
    let mut tasks = false;
    for s in &w.steps {
        match &s.kind {
            StepKind::Action {
                connector, action, ..
            } if connector == crate::model::TAB_SOURCE => match action.as_str() {
                "task.create" => tasks = true,
                "item.snooze" => changes.push(Change::Snoozed),
                "item.done" => changes.push(Change::Done),
                "item.reopen" => changes.push(Change::Reopened),
                _ => {}
            },
            StepKind::Emit { signal, .. } => signals.push(signal.clone()),
            _ => {}
        }
    }
    (changes, signals, tasks)
}

/// Whether `a`'s steps could start `b`. Conservative: if an item change
/// could reach `b`'s trigger, it counts.
fn can_start(a: &Workflow, b: &Workflow) -> bool {
    let (changes, signals, tasks) = causes(a);
    match &b.trigger {
        Trigger::Signal { name, .. } => signals.iter().any(|s| s == name),
        Trigger::Item {
            changes: on,
            sources,
            kinds,
            ..
        } => {
            let any = |c: Change| on.contains(&c);
            let new_task = tasks
                && (any(Change::Ingested) || any(Change::Ranked))
                && (sources.is_empty() || sources.iter().any(|s| s == crate::model::TAB_SOURCE))
                && (kinds.is_empty()
                    || kinds
                        .iter()
                        .any(|k| glob(k, "tab.task") || glob(k, "tab.scheduled")));
            // Snooze/done/reopen act on whatever item they're given.
            new_task || changes.iter().any(|c| any(*c))
        }
        Trigger::Schedule { .. } | Trigger::Manual => false,
    }
}

/// A circle of enabled workflows (counting `w` as on) that can start each
/// other, by name, if there is one.
pub fn find_loop(w: &Workflow, others: &[Workflow]) -> Option<Vec<String>> {
    let mut all: Vec<&Workflow> = others
        .iter()
        .filter(|o| o.enabled && (w.id.is_empty() || o.id != w.id))
        .collect();
    all.insert(0, w);
    let n = all.len();
    let edges: Vec<Vec<usize>> = (0..n)
        .map(|i| (0..n).filter(|&j| can_start(all[i], all[j])).collect())
        .collect();
    // Depth-first from `w` (index 0) looking for a path back to it.
    let mut path = vec![0usize];
    let mut visited = HashSet::new();
    fn dfs(
        at: usize,
        edges: &[Vec<usize>],
        path: &mut Vec<usize>,
        visited: &mut HashSet<usize>,
    ) -> bool {
        for &next in &edges[at] {
            if next == 0 {
                return true;
            }
            if visited.insert(next) {
                path.push(next);
                if dfs(next, edges, path, visited) {
                    return true;
                }
                path.pop();
            }
        }
        false
    }
    if dfs(0, &edges, &mut path, &mut visited) {
        let mut names: Vec<String> = path.iter().map(|&i| all[i].name.clone()).collect();
        names.push(all[0].name.clone());
        return Some(names);
    }
    None
}

/// Ready-made workflows the builder offers as starting points.
pub fn templates() -> Vec<Workflow> {
    let step = |id: &str, kind: StepKind| Step {
        id: id.into(),
        label: String::new(),
        when: Vec::new(),
        continue_on_error: false,
        kind,
    };
    let input = |pairs: &[(&str, &str)]| -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
            .collect()
    };
    let item = |sources: &[&str], kinds: &[&str]| Trigger::Item {
        changes: vec![Change::Ingested],
        sources: sources.iter().map(|s| s.to_string()).collect(),
        kinds: kinds.iter().map(|s| s.to_string()).collect(),
        when: Vec::new(),
    };
    let wf = |name: &str, description: &str, trigger: Trigger, steps: Vec<Step>| Workflow {
        name: name.into(),
        description: description.into(),
        trigger,
        steps,
        created_by: "template".into(),
        ..Default::default()
    };
    vec![
        wf(
            "Snooze newsletters until the morning",
            "Newsletters and automated mail wait until you've done real work.",
            item(&["email"], &["email.newsletter", "email.automated"]),
            vec![step(
                "snooze",
                StepKind::Action {
                    connector: "tab".into(),
                    action: "item.snooze".into(),
                    input: input(&[("item_id", "{{item.id}}"), ("minutes", "720")]),
                },
            )],
        ),
        wf(
            "Small PRs get a review task",
            "When your review is requested on a small PR, add an urgent task so it's done today.",
            item(&["github"], &["github.review_requested"]),
            vec![
                step(
                    "pr",
                    StepKind::Action {
                        connector: "github".into(),
                        action: "pr.get".into(),
                        input: input(&[
                            ("repo", "{{item.metadata.repo}}"),
                            ("number", "{{item.metadata.number}}"),
                        ]),
                    },
                ),
                step(
                    "small",
                    StepKind::Filter {
                        conditions: vec![
                            Condition {
                                field: "steps.pr.additions".into(),
                                op: Op::Lt,
                                value: Value::from(200),
                            },
                            Condition {
                                field: "steps.pr.draft".into(),
                                op: Op::Equals,
                                value: Value::Bool(false),
                            },
                        ],
                    },
                ),
                step(
                    "task",
                    StepKind::Action {
                        connector: "tab".into(),
                        action: "task.create".into(),
                        input: input(&[
                            (
                                "title",
                                "Review {{item.metadata.repo}}#{{item.metadata.number}} (+{{steps.pr.additions}})",
                            ),
                            ("priority", "urgent"),
                            ("url", "{{steps.pr.url}}"),
                        ]),
                    },
                ),
            ],
        ),
        wf(
            "Label PRs that fail CI",
            "Adds a ci-failed label so the team sees it, and tells other workflows.",
            item(&["github"], &["github.ci_failed"]),
            vec![
                step(
                    "label",
                    StepKind::Action {
                        connector: "github".into(),
                        action: "issue.add_labels".into(),
                        input: input(&[
                            ("repo", "{{item.metadata.repo}}"),
                            ("number", "{{item.metadata.number}}"),
                            ("labels", "ci-failed"),
                        ]),
                    },
                ),
                step(
                    "tell",
                    StepKind::Emit {
                        signal: "ci_failed".into(),
                        payload: input(&[
                            ("repo", "{{item.metadata.repo}}"),
                            ("number", "{{item.metadata.number}}"),
                            ("title", "{{item.title}}"),
                        ]),
                    },
                ),
            ],
        ),
        wf(
            "Morning sweep",
            "Every morning-ish, a task to go through what came in overnight.",
            Trigger::Schedule {
                every_minutes: 1440,
            },
            vec![step(
                "task",
                StepKind::Action {
                    connector: "tab".into(),
                    action: "task.create".into(),
                    input: input(&[("title", "Sweep the queue"), ("priority", "high")]),
                },
            )],
        ),
    ]
}

/// Where the trigger stands, in words, for lists and agents.
pub fn describe_trigger(t: &Trigger) -> String {
    match t {
        Trigger::Item {
            changes,
            sources,
            kinds,
            ..
        } => {
            let ch: Vec<String> = changes
                .iter()
                .map(|c| {
                    serde_json::to_value(c)
                        .map(|v| as_text(&v))
                        .unwrap_or_default()
                })
                .collect();
            let mut s = format!("When an item is {}", ch.join(" or "));
            if !sources.is_empty() {
                s.push_str(&format!(" from {}", sources.join(", ")));
            }
            if !kinds.is_empty() {
                s.push_str(&format!(" ({})", kinds.join(", ")));
            }
            s
        }
        Trigger::Signal { name, .. } => format!("When a workflow signals {name}"),
        Trigger::Schedule { every_minutes } => format!("Every {every_minutes} minutes"),
        Trigger::Manual => "When started by hand".into(),
    }
}

/// Inputs that are still empty get the action's defaults. Used when an
/// agent or the builder adds a step.
pub fn fill_defaults(w: &mut Workflow, catalog: &[ConnectorInfo]) {
    for s in &mut w.steps {
        if let StepKind::Action {
            connector,
            action,
            input,
        } = &mut s.kind
            && let Some(info) = find_action(catalog, connector, action)
        {
            for f in &info.inputs {
                if let Some(d) = &f.default
                    && !input.contains_key(&f.key)
                {
                    input.insert(f.key.clone(), Value::String(d.clone()));
                }
            }
        }
    }
}

/// Who is saving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedBy {
    Person,
    Agent,
}

/// Gets a workflow ready to store: fills in action defaults, validates it,
/// and applies who may turn what on. An agent's save is always off and can
/// never allow destructive steps; a workflow with errors is stored off.
/// Returns the workflow to store and the issues found.
pub fn prepare_save(
    mut w: Workflow,
    by: SavedBy,
    catalog: &[ConnectorInfo],
    saved: &[Workflow],
    families: &[String],
) -> (Workflow, Vec<Issue>) {
    let existing = saved.iter().find(|o| !w.id.is_empty() && o.id == w.id);
    if by == SavedBy::Agent {
        w.enabled = false;
        w.allow_destructive = existing.is_some_and(|e| e.allow_destructive);
        w.allow_commands = existing.is_some_and(|e| e.allow_commands);
        if existing.is_none() {
            w.created_by = "agent".into();
        }
    }
    fill_defaults(&mut w, catalog);
    let issues = validate(&w, catalog, saved, families);
    if issues.iter().any(|i| i.error) {
        w.enabled = false;
    }
    (w, issues)
}

/// Counts by status, for the list view.
pub fn tally(runs: &[Run]) -> HashMap<RunStatus, usize> {
    let mut m = HashMap::new();
    for r in runs {
        *m.entry(r.status).or_default() += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn conditions_and_globs() {
        let ctx = json!({ "item": { "kind": "github.ci_failed", "title": "Fix Checkout", "metadata": { "labels": ["bug", "p1"], "n": 12 } } });
        let c = |field: &str, op: Op, value: Value| {
            Condition {
                field: field.into(),
                op,
                value,
            }
            .holds(&ctx)
        };
        assert!(c("item.kind", Op::Matches, json!("github.*")));
        assert!(!c("item.kind", Op::Matches, json!("email.*")));
        assert!(c("item.title", Op::Contains, json!("checkout")));
        assert!(c("item.metadata.labels", Op::Contains, json!("P1")));
        assert!(c("item.metadata.n", Op::Gt, json!("10")));
        assert!(c("item.metadata.n", Op::Equals, json!("12")));
        assert!(c(
            "item.kind",
            Op::In,
            json!("email.direct, github.ci_failed")
        ));
        assert!(c("item.nope", Op::Missing, Value::Null));
        assert!(glob("*bot*", "dependabot[bot]"));
        assert!(glob("a*c", "abc") && !glob("a*c", "abd"));
    }

    #[test]
    fn workflows_round_trip_as_json() {
        for t in templates() {
            let s = serde_json::to_string(&t).unwrap();
            let back: Workflow = serde_json::from_str(&s).unwrap();
            assert_eq!(back, t);
        }
        // The shape agents and people write by hand.
        let w: Workflow = serde_json::from_value(json!({
            "name": "x",
            "trigger": { "type": "item", "changes": ["ingested"], "kinds": ["github.*"] },
            "steps": [{ "id": "a", "type": "action", "connector": "tab", "action": "item.snooze", "input": { "minutes": 5 } }]
        }))
        .unwrap();
        assert!(matches!(w.steps[0].kind, StepKind::Action { .. }));
        assert_eq!(w.limits.max_runs_per_hour, 30);
    }

    fn task_on_any_item(name: &str) -> Workflow {
        Workflow {
            id: name.into(),
            name: name.into(),
            enabled: true,
            trigger: Trigger::default(),
            steps: vec![Step {
                id: "t".into(),
                label: String::new(),
                when: vec![],
                continue_on_error: false,
                kind: StepKind::Action {
                    connector: "tab".into(),
                    action: "task.create".into(),
                    input: Map::new(),
                },
            }],
            ..Default::default()
        }
    }

    #[test]
    fn finds_loops_before_saving() {
        // Creating a task on every new item creates a new item.
        let w = task_on_any_item("echo");
        assert_eq!(find_loop(&w, &[]).unwrap(), ["echo", "echo"]);
        // Limited to GitHub, the new task can't start it again.
        let mut narrowed = w.clone();
        narrowed.trigger = Trigger::Item {
            changes: vec![Change::Ingested],
            sources: vec!["github".into()],
            kinds: vec![],
            when: vec![],
        };
        assert!(find_loop(&narrowed, &[]).is_none());

        // Two workflows signalling each other.
        let emit = |sig: &str| Step {
            id: "e".into(),
            label: String::new(),
            when: vec![],
            continue_on_error: false,
            kind: StepKind::Emit {
                signal: sig.into(),
                payload: Map::new(),
            },
        };
        let a = Workflow {
            id: "a".into(),
            name: "A".into(),
            enabled: true,
            trigger: Trigger::Signal {
                name: "ping".into(),
                when: vec![],
            },
            steps: vec![emit("pong")],
            ..Default::default()
        };
        let b = Workflow {
            id: "b".into(),
            name: "B".into(),
            enabled: true,
            trigger: Trigger::Signal {
                name: "pong".into(),
                when: vec![],
            },
            steps: vec![emit("ping")],
            ..Default::default()
        };
        assert_eq!(
            find_loop(&a, std::slice::from_ref(&b)).unwrap(),
            ["A", "B", "A"]
        );
        // A disabled partner can't close the circle.
        let mut off = b.clone();
        off.enabled = false;
        assert!(find_loop(&a, &[off]).is_none());
    }

    #[test]
    fn command_steps_are_checked_and_only_the_person_allows_them() {
        let cmd = |program: &str, args: &[&str]| Step {
            id: "run".into(),
            label: String::new(),
            when: vec![],
            continue_on_error: false,
            kind: StepKind::Command {
                program: program.into(),
                args: args.iter().map(|a| a.to_string()).collect(),
                folder: String::new(),
                network: vec![],
                env: Map::new(),
                image: String::new(),
                timeout_secs: 0,
            },
        };
        let w = |step: Step| Workflow {
            name: "Build".into(),
            trigger: Trigger::Manual,
            steps: vec![step],
            ..Default::default()
        };
        let msgs = |w: &Workflow| -> Vec<(bool, String)> {
            validate(w, &[], &[], &[])
                .into_iter()
                .map(|i| (i.error, i.message))
                .collect()
        };

        let ok = w(cmd("cargo", &["test"]));
        let m = msgs(&ok);
        assert!(m.iter().all(|(e, _)| !e), "{m:?}");
        assert!(m.iter().any(|(_, t)| t.contains("allow commands")), "{m:?}");

        assert!(
            msgs(&w(cmd("", &[])))
                .iter()
                .any(|(e, t)| *e && t.contains("program"))
        );
        assert!(msgs(&w(cmd("{{item.title}}", &[]))).iter().any(|(e, _)| *e));
        assert!(
            msgs(&w(cmd("sh", &["-c", "{{item.title}}"])))
                .iter()
                .any(|(e, t)| !e && t.contains("shell"))
        );
        assert!(
            msgs(&w(cmd("cargo", &["{{steps.later.x}}"])))
                .iter()
                .any(|(e, _)| *e),
            "templates still only read earlier steps"
        );

        // An agent can't turn commands on, or keep them on by resaving.
        let mut allowed = ok.clone();
        allowed.id = "wf1".into();
        allowed.allow_commands = true;
        let (agent, _) = prepare_save(allowed.clone(), SavedBy::Agent, &[], &[], &[]);
        assert!(!agent.allow_commands);
        let mut kept = allowed.clone();
        kept.allow_commands = false;
        let (resaved, _) = prepare_save(kept, SavedBy::Agent, &[], &[allowed.clone()], &[]);
        assert!(resaved.allow_commands, "the person's choice stays");
        let (person, _) = prepare_save(allowed, SavedBy::Person, &[], &[], &[]);
        assert!(person.allow_commands);
    }

    fn agent_step(family: &str, harness: &str) -> Workflow {
        Workflow {
            name: "Fix".into(),
            trigger: Trigger::Manual,
            steps: vec![Step {
                id: "fix".into(),
                label: String::new(),
                when: vec![],
                continue_on_error: false,
                kind: StepKind::Agent {
                    prompt: "Fix the flaky test".into(),
                    repo: String::new(),
                    harness: harness.into(),
                    model: String::new(),
                    family: family.into(),
                    placement: crate::runner::Placement::Anywhere,
                    deadline_minutes: 0,
                },
            }],
            ..Default::default()
        }
    }

    #[test]
    fn an_agent_step_may_only_ask_for_a_family_that_exists() {
        let families = ["fast".to_string(), "mine".to_string()];
        let issues = |w: &Workflow| validate(w, &[], &[], &families);
        assert!(issues(&agent_step("mine", "")).is_empty());
        assert!(issues(&agent_step("", "")).is_empty());
        let missing = issues(&agent_step("huge", ""));
        assert!(
            missing
                .iter()
                .any(|i| i.error && i.message == "There's no model family called huge."),
            "{missing:?}"
        );
        let pinned = issues(&agent_step("fast", "codex"));
        assert!(
            pinned
                .iter()
                .any(|i| !i.error && i.message.contains("pinned model wins")),
            "{pinned:?}"
        );
        let (saved, _) = prepare_save(agent_step("huge", ""), SavedBy::Person, &[], &[], &families);
        assert!(
            !saved.enabled,
            "a missing family is an error, so it's saved off"
        );
    }

    #[test]
    fn workflows_from_before_families_still_parse() {
        let old = json!({
            "name": "Fix", "trigger": { "type": "manual" },
            "steps": [{ "id": "fix", "type": "agent", "prompt": "Fix it", "harness": "codex", "model": "" }]
        });
        let w: Workflow = serde_json::from_value(old).unwrap();
        let StepKind::Agent {
            family, harness, ..
        } = &w.steps[0].kind
        else {
            panic!("not an agent step: {:?}", w.steps[0].kind);
        };
        assert!(family.is_empty());
        assert_eq!(harness, "codex");
        // And a step without a family writes none, for old apps.
        let v = serde_json::to_value(&w).unwrap();
        assert!(v["steps"][0].get("family").is_none());
    }
}
