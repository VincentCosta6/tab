//! The workflow builder: the list of workflows, the Factory overview, all
//! runs, and one workflow open as a flow (trigger, then steps) with an
//! inspector whose forms come from each action's descriptor, its runs, its
//! insights and an assistant that builds it with you. The counterpart of
//! `src/modules/workflows/WorkflowsTab.tsx`.
//!
//! Everything a step can do comes from connector metadata (`actions` on
//! each connector), so a new connector action shows up here with no UI
//! change.

mod charts;
mod editor;
mod form;
mod format;
mod insights;
mod runs;

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, AppContext, Context, Entity, EventEmitter, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::Sizable;
use gpui_component::button::Button as GButton;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::popover::Popover;
use gpui_component::switch::Switch;
use serde_json::Map;
use tab_core::EngineEvent;
use tab_core::families::ModelFamily;
use tab_core::model::{ConnectorInfo, Effect, ItemState, QueueItem};
use tab_core::runner::{Placement, RunnerCaps};
use tab_core::workflow::insights::{Factory, WorkflowInsights};
use tab_core::workflow::items::RunView;
use tab_core::workflow::{
    Condition, Issue, Op, Run, RunStatus, Step, StepKind, StepStatus, Trigger, Workflow,
};

use crate::backend::backend;
use crate::chat::{ChatView, Target};
use crate::components::{Button, ButtonKind, ago, badge, icon};
use crate::theme::{Theme, Tone, size, theme};

pub use format::describe_trigger;
use format::*;

/// Asks the shell to open an item in Work.
pub struct OpenItem(pub String);

#[derive(Clone, Copy, PartialEq)]
enum View {
    Factory,
    Builder,
    Runs,
}

#[derive(Clone, Copy, PartialEq)]
enum Panel {
    Edit,
    Runs,
    Insights,
    Assistant,
}

/// The node selected on the canvas.
#[derive(Clone, Copy, PartialEq)]
enum Sel {
    Trigger,
    Step(usize),
}

#[derive(Clone, Copy, PartialEq)]
enum RunFilter {
    All,
    Problems,
    Succeeded,
    Dry,
}

/// A step the add menu makes.
#[derive(Clone)]
enum NewStep {
    Action { connector: String, action: String },
    Filter,
    Set,
    Emit,
    Command,
    Agent,
}

/// Something that would throw away unsaved edits or do real work, waiting
/// for a second click.
#[derive(Clone)]
enum Confirm {
    Open(Box<Workflow>, Panel),
    AskAi,
    RunNow,
    Delete,
}

fn blank() -> Workflow {
    Workflow {
        id: String::new(),
        name: "New workflow".into(),
        description: String::new(),
        enabled: false,
        allow_destructive: false,
        allow_commands: false,
        trigger: Trigger::default(),
        steps: Vec::new(),
        limits: Default::default(),
        created_by: "user".into(),
        version: 0,
        updated_at: None,
    }
}

fn find_action<'a>(
    connectors: &'a [ConnectorInfo],
    connector: &str,
    action: &str,
) -> Option<&'a tab_core::model::ActionInfo> {
    connectors
        .iter()
        .find(|c| c.descriptor.id == connector)?
        .descriptor
        .actions
        .iter()
        .find(|a| a.id == action)
}

fn unique_id(w: &Workflow, base: &str) -> String {
    let b: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let b = b.trim_matches('_');
    let b = if b.is_empty() { "step" } else { b };
    let mut id = b.to_string();
    let mut n = 2;
    while w.steps.iter().any(|s| s.id == id) {
        id = format!("{b}_{n}");
        n += 1;
    }
    id
}

/// A new step of `kind`, with its action's defaults filled in.
fn new_step(w: &Workflow, kind: &NewStep, connectors: &[ConnectorInfo]) -> Step {
    let step = |id: String, kind: StepKind| Step {
        id,
        label: String::new(),
        when: Vec::new(),
        continue_on_error: false,
        kind,
    };
    match kind {
        NewStep::Action { connector, action } => {
            let mut input = Map::new();
            for f in find_action(connectors, connector, action)
                .map(|a| a.inputs.clone())
                .unwrap_or_default()
            {
                if let Some(d) = f.default {
                    input.insert(f.key, d.into());
                }
            }
            step(
                unique_id(w, action.rsplit('.').next().unwrap_or("step")),
                StepKind::Action {
                    connector: connector.clone(),
                    action: action.clone(),
                    input,
                },
            )
        }
        NewStep::Filter => step(
            unique_id(w, "only_if"),
            StepKind::Filter {
                conditions: vec![Condition {
                    field: "item.title".into(),
                    op: Op::Contains,
                    value: "".into(),
                }],
            },
        ),
        NewStep::Set => {
            let mut values = Map::new();
            values.insert("name".into(), "".into());
            step(unique_id(w, "values"), StepKind::Set { values })
        }
        NewStep::Agent => step(
            unique_id(w, "agent"),
            StepKind::Agent {
                prompt: String::new(),
                repo: String::new(),
                harness: String::new(),
                model: String::new(),
                family: "balanced".into(),
                placement: Placement::Computers,
                deadline_minutes: 0,
            },
        ),
        NewStep::Command => step(
            unique_id(w, "run"),
            StepKind::Command {
                program: String::new(),
                args: Vec::new(),
                folder: String::new(),
                network: Vec::new(),
                env: Map::new(),
                image: String::new(),
                timeout_secs: 0,
            },
        ),
        NewStep::Emit => step(
            unique_id(w, "signal"),
            StepKind::Emit {
                signal: String::new(),
                payload: Map::new(),
            },
        ),
    }
}

/// Variables a step can use: the item, earlier steps' outputs, the signal.
fn variables(w: &Workflow, up_to: usize, connectors: &[ConnectorInfo]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut add = |p: &str, l: &str| out.push((p.to_string(), l.to_string()));
    let item_sources = match &w.trigger {
        Trigger::Item { sources, .. } => Some(sources.clone()),
        Trigger::Manual => Some(Vec::new()),
        _ => None,
    };
    if let Some(src) = &item_sources {
        for (p, l) in [
            ("item.title", "Title"),
            ("item.body", "Body"),
            ("item.url", "Link"),
            ("item.actor", "From"),
            ("item.context", "Where"),
            ("item.kind", "Kind"),
            ("item.score", "Score"),
            ("item.id", "Item id"),
        ] {
            add(p, l);
        }
        if src.is_empty() || src.iter().any(|s| s == "github") {
            add("item.metadata.repo", "Repo");
            add("item.metadata.number", "PR / issue #");
        }
        if src.is_empty() || src.iter().any(|s| s == "email") {
            add("item.metadata.folder", "Mail folder");
            add("item.metadata.uid", "Mail UID");
        }
    }
    if matches!(w.trigger, Trigger::Signal { .. }) {
        add("signal", "Signal payload");
    }
    for s in w.steps.iter().take(up_to) {
        let id = &s.id;
        match &s.kind {
            StepKind::Action {
                connector, action, ..
            } => {
                for f in find_action(connectors, connector, action)
                    .map(|a| a.outputs.clone())
                    .unwrap_or_default()
                {
                    add(
                        &format!("steps.{id}.{}", f.key),
                        &format!("{id}: {}", f.label),
                    );
                }
            }
            StepKind::Set { values } => {
                for k in values.keys() {
                    add(&format!("steps.{id}.{k}"), &format!("{id}: {k}"));
                }
            }
            StepKind::Agent { .. } => {
                add(
                    &format!("steps.{id}.summary"),
                    &format!("{id}: What the agent said"),
                );
                add(&format!("steps.{id}.branch"), &format!("{id}: Branch"));
                add(
                    &format!("steps.{id}.pr_url"),
                    &format!("{id}: Pull request"),
                );
            }
            StepKind::Command { .. } => {
                add(
                    &format!("steps.{id}.exit_code"),
                    &format!("{id}: Exit code"),
                );
                add(&format!("steps.{id}.stdout"), &format!("{id}: Output"));
                add(&format!("steps.{id}.stderr"), &format!("{id}: Errors"));
            }
            _ => {}
        }
    }
    add("now", "Now");
    out
}

fn placement_label(p: &Placement) -> String {
    match p {
        Placement::Anywhere => "anywhere".into(),
        Placement::Computers => "your computers".into(),
        Placement::Cloud => "tab cloud".into(),
        Placement::PreferComputer { minutes } => {
            format!("a computer, then tab cloud after {minutes} min")
        }
    }
}

/// Harness ids and names (what an agent step can ask for).
fn harnesses() -> Vec<(String, String)> {
    tab_core::agents::catalog::harnesses()
        .iter()
        .map(|h| (h.id.clone(), h.name.clone()))
        .collect()
}

fn harness_name(id: &str) -> String {
    harnesses()
        .into_iter()
        .find(|(h, _)| h == id)
        .map_or(id.to_string(), |(_, n)| n)
}

fn runs_summary(runs: &[Run]) -> String {
    let now = chrono::Utc::now();
    let day: Vec<&Run> = runs
        .iter()
        .filter(|r| !r.dry_run && now - r.started_at < chrono::Duration::hours(24))
        .collect();
    if day.is_empty() {
        return match runs.first() {
            Some(r) => format!("Last {}", ago(r.started_at, now)),
            None => "None yet".into(),
        };
    }
    let bad = day
        .iter()
        .filter(|r| matches!(r.status, RunStatus::Failed | RunStatus::Blocked))
        .count();
    if bad > 0 {
        format!("{} today · {bad} need a look", day.len())
    } else {
        format!("{} today", day.len())
    }
}

pub struct WorkflowsTab {
    workflows: Vec<Workflow>,
    templates: Vec<Workflow>,
    connectors: Vec<ConnectorInfo>,
    families: Vec<ModelFamily>,
    runners: Vec<(RunnerCaps, bool, bool)>,
    /// Open items, for the test item picker.
    items: Vec<QueueItem>,
    view: View,
    panel: Panel,
    /// The workflow open, as edited.
    draft: Option<Workflow>,
    dirty: bool,
    selected: Sel,
    issues: Vec<Issue>,
    dry: Option<Run>,
    busy: Option<&'static str>,
    error: Option<String>,
    confirm: Option<Confirm>,
    test_item: Option<String>,
    all_runs: Vec<Run>,
    runs: Vec<Run>,
    open_run: Option<String>,
    run_views: HashMap<String, RunView>,
    run_filter: RunFilter,
    run_workflow: Option<String>,
    days: u32,
    factory: Option<Factory>,
    insights: Option<WorkflowInsights>,
    versions: (Option<u32>, Option<u32>),
    /// Where the add menu is open: the index a new step goes in at.
    add_at: Option<usize>,
    add_query: Entity<InputState>,
    /// The field whose variable menu is open.
    var_menu: Option<String>,
    name: Entity<InputState>,
    fields: form::Fields,
    assistant: Option<Entity<ChatView>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<OpenItem> for WorkflowsTab {}

impl WorkflowsTab {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Workflow name"));
        let named = cx.subscribe(&name, |this, s, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                let v = s.read(cx).value().to_string();
                if this.draft.as_ref().is_some_and(|w| w.name != v) {
                    this.edit(move |w| w.name = v.clone(), cx);
                }
            }
        });
        let add_query = cx.new(|cx| InputState::new(window, cx).placeholder("Find an action…"));
        let searched = cx.subscribe(&add_query, |_, _, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                cx.notify();
            }
        });
        let events = crate::events::events(cx);
        let live = cx.subscribe_in(
            &events,
            window,
            |this, _, ev: &Arc<EngineEvent>, window, cx| match &**ev {
                EngineEvent::WorkflowsChanged { id, by } => {
                    this.reload(cx);
                    if by == "agent" {
                        this.agent_saved(id, window, cx);
                    }
                }
                EngineEvent::WorkflowRun { run } => {
                    this.all_runs.retain(|r| r.id != run.id);
                    this.all_runs.insert(0, run.clone());
                    if this.draft.as_ref().is_some_and(|w| w.id == run.workflow_id) {
                        this.runs.retain(|r| r.id != run.id);
                        this.runs.insert(0, run.clone());
                    }
                    this.run_views.remove(&run.id);
                    cx.notify();
                }
                _ => {}
            },
        );
        let b = backend(cx);
        let mut this = WorkflowsTab {
            workflows: Vec::new(),
            templates: b.workflow_templates(),
            connectors: Vec::new(),
            families: b.model_families().unwrap_or_default(),
            runners: b.runners().unwrap_or_default(),
            items: b.store.list(&[ItemState::Open]).unwrap_or_default(),
            view: View::Factory,
            panel: Panel::Edit,
            draft: None,
            dirty: false,
            selected: Sel::Trigger,
            issues: Vec::new(),
            dry: None,
            busy: None,
            error: None,
            confirm: None,
            test_item: None,
            all_runs: Vec::new(),
            runs: Vec::new(),
            open_run: None,
            run_views: HashMap::new(),
            run_filter: RunFilter::All,
            run_workflow: None,
            days: 7,
            factory: None,
            insights: None,
            versions: (None, None),
            add_at: None,
            add_query,
            var_menu: None,
            name,
            fields: Default::default(),
            assistant: None,
            _subs: vec![named, searched, live],
        };
        let task =
            crate::runtime::spawn(async move { b.with_engine(async |e| Ok(e.connectors())).await });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(cs)) = task.await {
                this.update(cx, |this, cx| {
                    this.connectors = cs;
                    this.check(cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        this.reload(cx);
        this.load_factory(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let b = backend(cx);
        match b.workflows() {
            Ok(ws) => self.workflows = ws,
            Err(e) => self.error = Some(e),
        }
        self.all_runs = b.workflow_runs(None, 200).unwrap_or_default();
        // Keep the open one in step, unless it has edits.
        if !self.dirty
            && let Some(d) = &self.draft
            && let Some(w) = self.workflows.iter().find(|w| w.id == d.id)
            && w != d
        {
            self.draft = Some(w.clone());
            self.fields.clear();
        }
        self.load_factory(cx);
        cx.notify();
    }

    /// The assistant saved a workflow: show it, unless that would throw
    /// away the person's edits.
    fn agent_saved(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (draft, dirty) = (self.draft.clone(), self.dirty);
        if dirty && draft.as_ref().is_some_and(|d| d.id == id) {
            return;
        }
        let show = match &draft {
            None => true,
            Some(d) => d.id == id || (d.id.is_empty() && !dirty) || d.steps.is_empty(),
        };
        if show && let Some(w) = self.workflows.iter().find(|w| w.id == id).cloned() {
            let panel = self.panel;
            self.open_now(w, window, cx);
            self.panel = panel;
        }
    }

    fn load_factory(&mut self, cx: &mut Context<Self>) {
        let offset = chrono::Local::now().offset().local_minus_utc() / 60;
        self.factory = backend(cx).workflow_factory(self.days, offset).ok();
    }

    fn load_insights(&mut self, cx: &mut Context<Self>) {
        let offset = chrono::Local::now().offset().local_minus_utc() / 60;
        self.insights = match &self.draft {
            Some(w) if !w.id.is_empty() => {
                backend(cx).workflow_insights(&w.id, self.days, offset).ok()
            }
            _ => None,
        };
        self.versions = (None, None);
    }

    fn set_days(&mut self, days: u32, cx: &mut Context<Self>) {
        self.days = days;
        self.load_factory(cx);
        self.load_insights(cx);
        cx.notify();
    }

    /// Opens `w`, asking first when that would throw away edits.
    fn open(&mut self, w: Workflow, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty {
            self.confirm = Some(Confirm::Open(Box::new(w), panel));
            cx.notify();
            return;
        }
        self.open_now(w, window, cx);
        self.panel = panel;
    }

    fn open_now(&mut self, w: Workflow, window: &mut Window, cx: &mut Context<Self>) {
        self.view = View::Builder;
        self.confirm = None;
        self.dirty = w.id.is_empty() && !w.steps.is_empty();
        self.selected = Sel::Trigger;
        self.dry = None;
        self.error = None;
        self.add_at = None;
        self.var_menu = None;
        self.fields.clear();
        let name = w.name.clone();
        self.name.update(cx, |s, cx| s.set_value(name, window, cx));
        let id_changed = self.draft.as_ref().map(|d| d.id.clone()) != Some(w.id.clone());
        self.runs = if w.id.is_empty() {
            Vec::new()
        } else {
            backend(cx)
                .workflow_runs(Some(&w.id), 50)
                .unwrap_or_default()
        };
        self.draft = Some(w);
        self.load_insights(cx);
        if id_changed {
            self.assistant = None;
        }
        self.check(cx);
        cx.notify();
    }

    fn ask_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty {
            self.confirm = Some(Confirm::AskAi);
            cx.notify();
            return;
        }
        self.open_now(blank(), window, cx);
        self.dirty = false;
        self.panel = Panel::Assistant;
    }

    /// Changes the draft from a field: the field keeps its own text.
    fn edit(&mut self, f: impl FnOnce(&mut Workflow), cx: &mut Context<Self>) {
        let Some(w) = &mut self.draft else { return };
        f(w);
        self.dirty = true;
        self.dry = None;
        if let (Some(a), Some(w)) = (&self.assistant, &self.draft) {
            let w = w.clone();
            a.update(cx, |v, _| v.retarget(Target::Workflow(Box::new(w))));
        }
        self.check(cx);
        cx.notify();
    }

    /// Changes the draft's shape (steps added, moved, removed, a type
    /// changed): every field is rebuilt from it.
    fn restructure(&mut self, f: impl FnOnce(&mut Workflow), cx: &mut Context<Self>) {
        self.edit(f, cx);
        self.fields.clear();
        self.var_menu = None;
    }

    fn insert_step(&mut self, at: usize, kind: NewStep, cx: &mut Context<Self>) {
        let connectors = self.connectors.clone();
        self.add_at = None;
        let mut at_ = at;
        self.restructure(
            |w| {
                let s = new_step(w, &kind, &connectors);
                at_ = at.min(w.steps.len());
                w.steps.insert(at_, s);
            },
            cx,
        );
        self.selected = Sel::Step(at_);
        self.panel = Panel::Edit;
    }

    /// Checks the draft against connectors and other workflows.
    fn check(&mut self, cx: &mut Context<Self>) {
        let Some(w) = self.draft.clone() else { return };
        let b = backend(cx);
        let task = crate::runtime::spawn(async move { b.workflow_validate(&w).await });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(issues)) = task.await {
                this.update(cx, |this, cx| {
                    this.issues = issues;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = self.draft.clone() else { return };
        self.busy = Some("save");
        self.error = None;
        let b = backend(cx);
        let task = crate::runtime::spawn(async move { b.workflow_save(w).await });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await.and_then(|r| r);
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match r {
                    Ok((w, issues)) => {
                        let panel = this.panel;
                        let sel = this.selected;
                        this.dirty = false;
                        this.reload(cx);
                        this.open_now(w, window, cx);
                        this.panel = panel;
                        this.selected = sel;
                        this.issues = issues;
                    }
                    Err(e) => this.error = Some(format!("Couldn't save: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Turns the open workflow on or off, or changes what it's allowed to
    /// do. Edits are saved first.
    fn set_enabled(
        &mut self,
        on: bool,
        allow_destructive: Option<bool>,
        allow_commands: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.draft.clone() else { return };
        self.busy = Some("enable");
        self.error = None;
        let (b, dirty) = (backend(cx), self.dirty);
        let task = crate::runtime::spawn(async move {
            let id = if dirty {
                let (saved, issues) = b.workflow_save(w).await?;
                if on && issues.iter().any(|i| i.error) {
                    return Err("Fix the problems below before turning it on.".to_string());
                }
                saved.id
            } else {
                w.id
            };
            b.workflow_set_enabled(&id, on, allow_destructive, allow_commands)
                .await
        });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await.and_then(|r| r);
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match r {
                    Ok(w) => {
                        let (panel, sel) = (this.panel, this.selected);
                        this.dirty = false;
                        this.reload(cx);
                        this.open_now(w, window, cx);
                        this.panel = panel;
                        this.selected = sel;
                    }
                    Err(e) => {
                        this.error = Some(e);
                        this.reload(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// From the list's switch, for a workflow that may not be open.
    fn toggle_listed(&mut self, id: String, on: bool, cx: &mut Context<Self>) {
        let b = backend(cx);
        let task =
            crate::runtime::spawn(async move { b.workflow_set_enabled(&id, on, None, None).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.and_then(|r| r);
            this.update(cx, |this, cx| {
                if let Err(e) = r {
                    this.error = Some(e);
                }
                this.reload(cx);
            })
            .ok();
        })
        .detach();
    }

    fn run(&mut self, dry: bool, cx: &mut Context<Self>) {
        let Some(w) = self.draft.clone() else { return };
        self.busy = Some(if dry { "dry" } else { "run" });
        self.error = None;
        self.confirm = None;
        let (b, item) = (backend(cx), self.test_item.clone());
        let task = crate::runtime::spawn(async move { b.workflow_run(w, item, dry).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.and_then(|r| r);
            this.update(cx, |this, cx| {
                this.busy = None;
                match r {
                    Ok(run) if dry => this.dry = Some(run),
                    Ok(run) => {
                        this.panel = Panel::Runs;
                        this.open_run = Some(run.id.clone());
                        this.runs.retain(|r| r.id != run.id);
                        this.runs.insert(0, run);
                    }
                    Err(e) => this.error = Some(format!("Couldn't run: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        self.confirm = None;
        let Some(id) = self.draft.as_ref().map(|w| w.id.clone()) else {
            return;
        };
        if !id.is_empty()
            && let Err(e) = backend(cx).workflow_delete(&id)
        {
            self.error = Some(e);
            return;
        }
        self.draft = None;
        self.dirty = false;
        self.assistant = None;
        self.view = View::Factory;
        self.reload(cx);
    }

    fn answer(&mut self, run_id: String, approve: bool, cx: &mut Context<Self>) {
        let b = backend(cx);
        let task =
            crate::runtime::spawn(async move { b.workflow_run_answer(&run_id, approve).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.and_then(|r| r);
            this.update(cx, |this, cx| {
                match r {
                    Ok(run) => {
                        this.run_views.remove(&run.id);
                        for list in [&mut this.runs, &mut this.all_runs] {
                            if let Some(slot) = list.iter_mut().find(|r| r.id == run.id) {
                                *slot = run.clone();
                            }
                        }
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn issues_at(&self, at: Option<&str>) -> Vec<Issue> {
        self.issues
            .iter()
            .filter(|i| i.at.as_deref() == at)
            .cloned()
            .collect()
    }

    // -----------------------------------------------------------------------
    // The list
    // -----------------------------------------------------------------------

    fn render_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let item = |id: SharedString, active: bool| {
            div()
                .id(id)
                .flex()
                .items_start()
                .gap(px(8.))
                .px(px(10.))
                .py(px(7.))
                .rounded(px(size::RADIUS_MD))
                .cursor_pointer()
                .when(active, |el| el.bg(t.accent.bg))
                .when(!active, |el| el.hover(|s| s.bg(t.bg_hover)))
        };
        let text = |name: String, sub: String, t: &Theme| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(size::TEXT_MD))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(t.fg)
                        .truncate()
                        .child(name),
                )
                .child(
                    div()
                        .text_size(px(size::TEXT_SM))
                        .text_color(t.fg_subtle)
                        .truncate()
                        .child(sub),
                )
        };
        let mut list = div().flex().flex_col().gap(px(2.));
        list = list.child(
            item("wf-factory".into(), self.view == View::Factory)
                .child(div().pt(px(2.)).child(icon("gauge", 14., t.fg_subtle)))
                .child(text(
                    "Factory".into(),
                    "How every workflow is doing".into(),
                    &t,
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.view = View::Factory;
                    this.load_factory(cx);
                    cx.notify();
                })),
        );
        list = list.child(
            item("wf-allruns".into(), self.view == View::Runs)
                .child(div().pt(px(2.)).child(icon("list", 14., t.fg_subtle)))
                .child(text("All runs".into(), runs_summary(&self.all_runs), &t))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.view = View::Runs;
                    cx.notify();
                })),
        );
        list = list.child(div().my(px(6.)).mx(px(8.)).h(px(1.)).bg(t.border));
        let open_id = self.draft.as_ref().map(|w| w.id.clone());
        let now = chrono::Utc::now();
        for w in &self.workflows {
            let active = self.view == View::Builder && open_id.as_deref() == Some(&w.id);
            let last = self
                .all_runs
                .iter()
                .find(|r| !r.dry_run && r.workflow_id == w.id);
            let (open, id, on) = (w.clone(), w.id.clone(), w.enabled);
            let this = cx.entity().downgrade();
            list = list.child(
                item(SharedString::from(format!("wf-{}", w.id)), active)
                    .child(
                        div()
                            .mt(px(6.))
                            .size(px(7.))
                            .flex_none()
                            .rounded_full()
                            .bg(if on { t.success.fg } else { t.border_strong }),
                    )
                    .child(
                        text(
                            w.name.clone(),
                            describe_trigger(&w.trigger, &self.connectors),
                            &t,
                        )
                        .when_some(last, |el, r| {
                            el.child(
                                div()
                                    .text_size(px(size::TEXT_XS))
                                    .text_color(run_tone(&t, r.status).fg)
                                    .child(format!(
                                        "{} {}",
                                        run_label(r.status),
                                        ago(r.started_at, now)
                                    )),
                            )
                        }),
                    )
                    .when(w.created_by == "agent", |el| {
                        el.child(div().pt(px(2.)).child(icon("sparkles", 12., t.fg_subtle)))
                    })
                    .child(
                        Switch::new(SharedString::from(format!("wf-on-{}", w.id)))
                            .checked(on)
                            .small()
                            .on_click(move |v, _, cx| {
                                let (v, id) = (*v, id.clone());
                                this.update(cx, |this, cx| this.toggle_listed(id, v, cx))
                                    .ok();
                            }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.draft.as_ref().is_some_and(|d| d.id == open.id) {
                            this.view = View::Builder;
                            cx.notify();
                        } else {
                            this.open(open.clone(), Panel::Edit, window, cx);
                        }
                    })),
            );
        }
        if self.workflows.is_empty() {
            list = list.child(
                div()
                    .px(px(10.))
                    .py(px(4.))
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_subtle)
                    .child(
                        "No workflows yet. Start from a template, describe one to the assistant, or build one.",
                    ),
            );
        }
        let mut templates = div().flex().flex_col().gap(px(8.)).px(px(4.));
        for (i, tmpl) in self.templates.iter().enumerate() {
            let mut w = tmpl.clone();
            w.id = String::new();
            w.enabled = false;
            w.created_by = "template".into();
            templates = templates.child(
                div()
                    .id(("wf-template", i))
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .p(px(10.))
                    .rounded(px(size::RADIUS_MD))
                    .border_1()
                    .border_dashed()
                    .border_color(t.border_strong)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg_hover))
                    .child(
                        div()
                            .text_size(px(size::TEXT_MD))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(tmpl.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(size::TEXT_SM))
                            .text_color(t.fg_muted)
                            .child(tmpl.description.clone()),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open(w.clone(), Panel::Edit, window, cx)
                    })),
            );
        }
        let _ = window;
        div()
            .id("wf-list")
            .w(px(260.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(t.border)
            .bg(t.bg_raised)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(12.))
                    .py(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(size::TEXT_LG))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Workflows"),
                    )
                    .child(
                        Button::new("wf-ask-ai")
                            .icon("sparkles")
                            .label("Ask AI")
                            .on_click(cx.listener(|this, _, window, cx| this.ask_ai(window, cx))),
                    )
                    .child(
                        Button::new("wf-new")
                            .icon("plus")
                            .label("New")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open(blank(), Panel::Edit, window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .id("wf-list-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.))
                    .pb(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(list)
                    .child(
                        div()
                            .px(px(8.))
                            .text_size(px(size::TEXT_XS))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(t.fg_subtle)
                            .child("START FROM A TEMPLATE"),
                    )
                    .child(templates),
            )
            .into_any_element()
    }

    // -----------------------------------------------------------------------
    // The builder
    // -----------------------------------------------------------------------

    fn render_bar(&mut self, w: &Workflow, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let busy = self.busy.is_some();
        let errors = self.issues.iter().filter(|i| i.error).count();
        let picked = self
            .test_item
            .as_ref()
            .and_then(|id| self.items.iter().find(|i| &i.id == id))
            .map(|i| i.event.title.chars().take(36).collect::<String>());
        let items: Vec<(String, String)> = self
            .items
            .iter()
            .take(60)
            .map(|i| (i.id.clone(), i.event.title.chars().take(70).collect()))
            .collect();
        let this = cx.entity().downgrade();
        let picker = GButton::new("wf-test-item")
            .outline()
            .small()
            .label(match picked {
                Some(p) => format!("Test on: {p}"),
                None => "Test on: no item".into(),
            })
            .dropdown_menu(move |mut menu, _, _| {
                let none = this.clone();
                menu = menu.max_h(px(360.)).scrollable(true).item(
                    PopupMenuItem::new("No item").on_click(move |_, _, cx| {
                        none.update(cx, |v, cx| {
                            v.test_item = None;
                            cx.notify();
                        })
                        .ok();
                    }),
                );
                for (id, title) in &items {
                    let (this, id) = (this.clone(), id.clone());
                    menu =
                        menu.item(PopupMenuItem::new(title.clone()).on_click(move |_, _, cx| {
                            let id = id.clone();
                            this.update(cx, |v, cx| {
                                v.test_item = Some(id);
                                cx.notify();
                            })
                            .ok();
                        }));
                }
                menu
            });
        let enabled = w.enabled;
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(12.))
            .py(px(10.))
            .border_b_1()
            .border_color(t.border)
            .bg(t.bg_raised)
            .child(
                div()
                    .flex_1()
                    .min_w(px(120.))
                    .text_size(px(size::TEXT_LG))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(Input::new(&self.name).appearance(false)),
            )
            .when(w.created_by == "agent", |el| {
                el.child(badge("AI draft", t.accent.fg, t.accent.bg))
            })
            .child(if self.dirty {
                badge("Unsaved", t.warning.fg, t.warning.bg).into_any_element()
            } else if !w.id.is_empty() {
                div()
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_subtle)
                    .child(format!("v{}", w.version))
                    .into_any_element()
            } else {
                div().into_any_element()
            })
            .child(picker)
            .child(
                Button::new("wf-dry")
                    .icon("eye")
                    .label(if self.busy == Some("dry") {
                        "Running…"
                    } else {
                        "Dry run"
                    })
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.run(true, cx))),
            )
            .when(
                matches!(w.trigger, Trigger::Manual) && !w.id.is_empty(),
                |el| {
                    el.child(
                        Button::new("wf-run")
                            .icon("zap")
                            .label("Run now")
                            .disabled(busy || self.dirty)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm = Some(Confirm::RunNow);
                                cx.notify();
                            })),
                    )
                },
            )
            .child(
                Button::new("wf-save")
                    .kind(if self.dirty {
                        ButtonKind::Primary
                    } else {
                        ButtonKind::Secondary
                    })
                    .icon("check")
                    .label("Save")
                    .disabled(!self.dirty || busy)
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(size::RADIUS_MD))
                    .bg(if enabled { t.success.bg } else { t.bg_sunken })
                    .child(
                        Switch::new("wf-enabled")
                            .checked(enabled)
                            .small()
                            .tooltip(if errors > 0 {
                                "Fix the problems first"
                            } else {
                                "Turn it on or off"
                            })
                            .on_click(cx.listener(|this, v: &bool, window, cx| {
                                this.set_enabled(*v, None, None, window, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(size::TEXT_SM))
                            .text_color(if enabled { t.success.fg } else { t.fg_muted })
                            .child(if enabled { "On" } else { "Off" }),
                    ),
            )
            .child(
                div()
                    .id("wf-delete")
                    .p(px(6.))
                    .rounded(px(size::RADIUS_SM))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg_hover))
                    .child(icon("trash", 14., t.fg_muted))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.draft.as_ref().is_some_and(|w| w.id.is_empty()) {
                            this.delete(cx);
                        } else {
                            this.confirm = Some(Confirm::Delete);
                        }
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_confirm(&self, c: &Confirm, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let name = self
            .draft
            .as_ref()
            .map(|w| w.name.clone())
            .unwrap_or_default();
        let (question, yes) = match c {
            Confirm::Open(..) | Confirm::AskAi => {
                ("Discard your unsaved changes?".to_string(), "Discard")
            }
            Confirm::RunNow => (
                "Run it for real now? Its write steps will change things.".to_string(),
                "Run it",
            ),
            Confirm::Delete => (format!("Delete \"{name}\" and its run history?"), "Delete"),
        };
        let c = c.clone();
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .mx(px(16.))
            .mt(px(10.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(size::RADIUS_MD))
            .bg(t.warning.bg)
            .text_color(t.warning.fg)
            .text_size(px(size::TEXT_MD))
            .child(icon("alert", 14., t.warning.fg))
            .child(div().flex_1().child(question))
            .child(
                Button::new("wf-confirm-no")
                    .kind(ButtonKind::Ghost)
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm = None;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("wf-confirm-yes")
                    .kind(ButtonKind::Primary)
                    .label(yes)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm = None;
                        match c.clone() {
                            Confirm::Open(w, panel) => {
                                this.dirty = false;
                                this.open_now(*w, window, cx);
                                this.panel = panel;
                            }
                            Confirm::AskAi => {
                                this.dirty = false;
                                this.ask_ai(window, cx);
                            }
                            Confirm::RunNow => this.run(false, cx),
                            Confirm::Delete => this.delete(cx),
                        }
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_dry(&self, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let would = run
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Dry)
            .count();
        let tone = run_tone(&t, run.status);
        let mut words = format!(
            "Dry run {} {}",
            if run.status == RunStatus::Succeeded {
                "finished".to_string()
            } else {
                run_label(run.status).to_lowercase()
            },
            run.trigger.trim_start_matches("by hand")
        );
        words.push_str(" · ");
        words.push_str(&if would > 0 {
            format!(
                "{would} write step{} would run",
                if would == 1 { "" } else { "s" }
            )
        } else {
            "nothing would change".into()
        });
        if let Some(e) = &run.error {
            words.push_str(&format!(" · {e}"));
        }
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .mx(px(16.))
            .mt(px(10.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(size::RADIUS_MD))
            .bg(tone.bg)
            .text_color(tone.fg)
            .text_size(px(size::TEXT_MD))
            .child(icon("eye", 14., tone.fg))
            .child(div().flex_1().child(words))
            .child(
                div()
                    .id("wf-dry-close")
                    .cursor_pointer()
                    .child(icon("x", 13., tone.fg))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.dry = None;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn node_issues(&self, issues: &[Issue], t: &Theme) -> Option<AnyElement> {
        if issues.is_empty() {
            return None;
        }
        let errors = issues.iter().filter(|i| i.error).count();
        let tone = if errors > 0 { t.danger } else { t.warning };
        Some(
            div()
                .flex_none()
                .min_w(px(18.))
                .h(px(18.))
                .px(px(5.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(tone.bg)
                .text_color(tone.fg)
                .text_size(px(size::TEXT_XS))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(if errors > 0 { errors } else { issues.len() }.to_string())
                .into_any_element(),
        )
    }

    /// A node's frame: the icon tile, kicker and title, then whatever else.
    #[allow(clippy::too_many_arguments)]
    fn node(
        &self,
        id: SharedString,
        selected: bool,
        tone: Tone,
        glyph: &str,
        kicker: String,
        title: String,
        right: Vec<AnyElement>,
        t: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .w(px(440.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(size::RADIUS_LG))
            .border_1()
            .border_color(if selected { t.accent.fg } else { t.border })
            .when(selected, |el| el.shadow_md())
            .when(!selected, |el| el.shadow_sm())
            .bg(t.bg_raised)
            .cursor_pointer()
            .hover(|s| s.border_color(t.border_strong))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .size(px(28.))
                            .flex_none()
                            .rounded(px(size::RADIUS_MD))
                            .bg(tone.bg)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(glyph, 15., tone.fg)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(size::TEXT_XS))
                                    .text_color(t.fg_subtle)
                                    .truncate()
                                    .child(kicker),
                            )
                            .child(
                                div()
                                    .text_size(px(size::TEXT_MD))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(t.fg)
                                    .child(title),
                            ),
                    )
                    .children(right),
            )
    }

    fn trigger_node(&self, w: &Workflow, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let tr = &w.trigger;
        let src = match tr {
            Trigger::Item { sources, .. } if sources.len() == 1 => self
                .connectors
                .iter()
                .find(|c| c.descriptor.id == sources[0]),
            _ => None,
        };
        let glyph = match tr {
            Trigger::Schedule { .. } => "clock".to_string(),
            Trigger::Signal { .. } => "zap".into(),
            Trigger::Manual => "user".into(),
            Trigger::Item { .. } => src.map_or("inbox".into(), |c| c.descriptor.icon.clone()),
        };
        let mut chips: Vec<String> = Vec::new();
        match tr {
            Trigger::Item { kinds, when, .. } => {
                for k in kinds {
                    chips.push(
                        self.connectors
                            .iter()
                            .flat_map(|c| c.descriptor.kinds.iter())
                            .find(|x| &x.kind == k)
                            .map_or(k.clone(), |x| x.label.clone()),
                    );
                }
                for c in when {
                    chips.push(
                        format!(
                            "{} {} {}",
                            c.field.trim_start_matches("item."),
                            op_label(c.op),
                            text(&c.value)
                        )
                        .trim()
                        .to_string(),
                    );
                }
            }
            Trigger::Signal { when, .. } => {
                for c in when {
                    chips.push(format!("{} {} {}", c.field, op_label(c.op), text(&c.value)));
                }
            }
            _ => {}
        }
        let issues = self.issues_at(Some("trigger"));
        let right: Vec<AnyElement> = self.node_issues(&issues, t).into_iter().collect();
        self.node(
            "wf-node-trigger".into(),
            self.selected == Sel::Trigger,
            t.accent,
            &glyph,
            "Trigger".into(),
            describe_trigger(tr, &self.connectors),
            right,
            t,
        )
        .when(!chips.is_empty(), |el| {
            el.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .children(chips.into_iter().map(|c| chip(c, t))),
            )
        })
        .on_click(cx.listener(|this, _, _, cx| {
            this.selected = Sel::Trigger;
            this.panel = Panel::Edit;
            this.var_menu = None;
            cx.notify();
        }))
        .into_any_element()
    }

    fn step_node(&self, w: &Workflow, i: usize, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let step = &w.steps[i];
        let (mut glyph, mut tone, kicker, title, lines, mut effect): (
            String,
            Tone,
            String,
            String,
            Vec<String>,
            Option<Effect>,
        );
        effect = None;
        match &step.kind {
            StepKind::Action {
                connector,
                action,
                input,
            } => {
                let info = find_action(&self.connectors, connector, action);
                glyph = info.map_or("zap".into(), |a| a.icon.clone());
                effect = info.map(|a| a.effect);
                tone = match effect {
                    Some(Effect::Destructive) => t.danger,
                    Some(Effect::Write) => t.info,
                    _ => t.neutral,
                };
                kicker = connector_name(&self.connectors, connector);
                title = if step.label.is_empty() {
                    info.map_or(action.clone(), |a| a.label.clone())
                } else {
                    step.label.clone()
                };
                lines = info
                    .map(|a| a.inputs.clone())
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|f| {
                        let v = text(input.get(&f.key).unwrap_or(&serde_json::Value::Null));
                        (!v.is_empty()).then(|| format!("{}: {v}", f.label))
                    })
                    .take(3)
                    .collect();
            }
            StepKind::Filter { conditions } => {
                glyph = "filter".into();
                tone = t.neutral;
                kicker = "Filter".into();
                title = if step.label.is_empty() {
                    "Only continue if".into()
                } else {
                    step.label.clone()
                };
                lines = conditions
                    .iter()
                    .map(|c| {
                        format!(
                            "{} {} {}",
                            c.field,
                            op_label(c.op),
                            if op_takes_value(c.op) {
                                text(&c.value)
                            } else {
                                String::new()
                            }
                        )
                    })
                    .collect();
            }
            StepKind::Set { values } => {
                glyph = "edit".into();
                tone = t.neutral;
                kicker = "Values".into();
                title = if step.label.is_empty() {
                    "Set values".into()
                } else {
                    step.label.clone()
                };
                lines = values
                    .iter()
                    .map(|(k, v)| format!("{k} = {}", text(v)))
                    .collect();
            }
            StepKind::Agent {
                prompt,
                repo,
                harness,
                placement,
                ..
            } => {
                glyph = "bot".into();
                tone = t.accent;
                kicker = format!("Agent · {}", placement_label(placement));
                title = if !step.label.is_empty() {
                    step.label.clone()
                } else {
                    let first: String = prompt
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(60)
                        .collect();
                    if first.is_empty() {
                        "Hand to an agent".into()
                    } else {
                        first
                    }
                };
                lines = vec![
                    if repo.is_empty() {
                        "No repo".into()
                    } else {
                        format!("In {repo}")
                    },
                    if harness.is_empty() {
                        "The runner's default harness".into()
                    } else {
                        harness_name(harness)
                    },
                ];
            }
            StepKind::Command {
                program,
                args,
                folder,
                network,
                image,
                ..
            } => {
                glyph = "terminal".into();
                tone = t.info;
                kicker = if image.is_empty() {
                    "Command".into()
                } else {
                    format!("Command · {image}")
                };
                let cmd = std::iter::once(program.clone())
                    .chain(args.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" ");
                title = if !step.label.is_empty() {
                    step.label.clone()
                } else if cmd.trim().is_empty() {
                    "Run a command".into()
                } else {
                    cmd.trim().to_string()
                };
                lines = vec![
                    if network.is_empty() {
                        "No network".into()
                    } else {
                        format!("Network: {}", network.join(", "))
                    },
                    if folder.is_empty() {
                        "In a fresh folder".into()
                    } else {
                        format!("In {folder}")
                    },
                ];
            }
            StepKind::Emit { signal, payload } => {
                glyph = "zap".into();
                tone = t.warning;
                kicker = "Signal".into();
                title = if !step.label.is_empty() {
                    step.label.clone()
                } else {
                    format!("Signal {}", if signal.is_empty() { "…" } else { signal })
                };
                lines = payload
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", text(v)))
                    .collect();
            }
        }
        if glyph.is_empty() {
            glyph = "dot".into();
        }
        let kicker = if step.when.is_empty() {
            kicker
        } else {
            format!("{kicker} · conditional")
        };
        let result = self
            .dry
            .as_ref()
            .and_then(|d| d.steps.iter().find(|s| s.id == step.id));
        let unreached = self.dry.is_some() && result.is_none();
        let issues = self.issues_at(Some(&step.id));
        let mut right: Vec<AnyElement> = Vec::new();
        if let Some(e) = effect.filter(|e| *e != Effect::Read) {
            let (label, tone_) = if e == Effect::Destructive {
                ("Destructive", t.danger)
            } else {
                ("Writes", t.info)
            };
            right.push(badge(label, tone_.fg, tone_.bg).into_any_element());
        }
        if let Some(i) = self.node_issues(&issues, t) {
            right.push(i);
        }
        if result.is_some_and(|r| r.status == StepStatus::Failed) {
            tone = t.danger;
        }
        let stats = if self.dirty {
            None
        } else {
            self.insights
                .as_ref()
                .filter(|ins| ins.workflow_id == w.id)
                .and_then(|ins| ins.stats.steps.iter().find(|s| s.id == step.id))
                .cloned()
        };
        let days = self.days;
        self.node(
            SharedString::from(format!("wf-node-{i}")),
            self.selected == Sel::Step(i),
            tone,
            &glyph,
            kicker,
            title,
            right,
            t,
        )
        .when(unreached, |el| el.opacity(0.5))
        .when(!lines.is_empty(), |el| {
            el.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .pl(px(38.))
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_muted)
                    .children(lines.into_iter().map(|l| div().truncate().child(l))),
            )
        })
        .when_some(result, |el, r| {
            let tone = step_tone(t, r.status);
            el.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pl(px(38.))
                    .child(badge(step_label(r.status), tone.fg, tone.bg))
                    .when_some(r.error.clone(), |el, e| {
                        el.child(
                            div()
                                .flex_1()
                                .text_size(px(size::TEXT_SM))
                                .text_color(t.danger.fg)
                                .child(e),
                        )
                    }),
            )
        })
        .when(result.is_none(), |el| {
            el.when_some(stats, |el, s| el.child(insights::step_badges(t, &s, days)))
        })
        .child(
            div()
                .flex()
                .justify_end()
                .text_size(px(size::TEXT_XS))
                .font_family(crate::theme::FONT_MONO)
                .text_color(t.fg_subtle)
                .child(step.id.clone()),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.selected = Sel::Step(i);
            this.panel = Panel::Edit;
            this.var_menu = None;
            cx.notify();
        }))
        .into_any_element()
    }

    /// The line between nodes, with a + that opens the add menu there.
    fn link(
        &mut self,
        at: usize,
        end: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let open = self.add_at == Some(at);
        let trigger = if end {
            GButton::new(SharedString::from(format!("wf-add-{at}")))
                .outline()
                .small()
                .icon(gpui_component::Icon::default().path(crate::assets::icon_path("plus")))
                .label("Add step")
        } else {
            GButton::new(SharedString::from(format!("wf-add-{at}")))
                .outline()
                .xsmall()
                .icon(gpui_component::Icon::default().path(crate::assets::icon_path("plus")))
        };
        let menu = if open {
            Some(self.add_menu(at, window, cx))
        } else {
            None
        };
        let this = cx.entity().downgrade();
        let mut pop = Popover::new(SharedString::from(format!("wf-add-pop-{at}")))
            .anchor(gpui::Anchor::TopLeft)
            .open(open)
            .on_open_change(move |o, _, cx| {
                let o = *o;
                this.update(cx, |v, cx| {
                    v.add_at = if o { Some(at) } else { None };
                    cx.notify();
                })
                .ok();
            })
            .trigger(trigger);
        if let Some(m) = menu {
            pop = pop.child(m);
        }
        div()
            .flex()
            .flex_col()
            .items_center()
            .child(div().w(px(2.)).h(px(14.)).bg(t.border_strong))
            .child(pop)
            .when(!end, |el| {
                el.child(div().w(px(2.)).h(px(14.)).bg(t.border_strong))
            })
            .into_any_element()
    }

    fn add_menu(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let q = self.add_query.read(cx).value().to_lowercase();
        let _ = window;
        let kinds = match &self.draft.as_ref().map(|w| w.trigger.clone()) {
            Some(Trigger::Item { kinds, .. }) => kinds.clone(),
            _ => Vec::new(),
        };
        let fits = |a: &tab_core::model::ActionInfo| {
            a.for_kinds.is_empty()
                || kinds.is_empty()
                || a.for_kinds.iter().any(|p| {
                    kinds.iter().any(|k| match p.strip_suffix(".*") {
                        Some(prefix) => k.starts_with(&format!("{prefix}.")),
                        None => k == p,
                    })
                })
        };
        let entry = |id: SharedString,
                     glyph: &str,
                     name: String,
                     desc: String,
                     dim: bool,
                     right: Option<AnyElement>,
                     kind: NewStep,
                     cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .items_start()
                .gap(px(8.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(size::RADIUS_SM))
                .cursor_pointer()
                .hover(|s| s.bg(t.bg_hover))
                .when(dim, |el| el.opacity(0.55))
                .child(div().pt(px(2.)).child(icon(glyph, 14., t.fg_muted)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(size::TEXT_MD))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(t.fg)
                                .child(name),
                        )
                        .child(
                            div()
                                .text_size(px(size::TEXT_SM))
                                .text_color(t.fg_subtle)
                                .child(desc),
                        ),
                )
                .children(right)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.insert_step(at, kind.clone(), cx);
                }))
        };
        let group_head = |s: String| {
            div()
                .px(px(8.))
                .pt(px(8.))
                .pb(px(2.))
                .text_size(px(size::TEXT_XS))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(t.fg_subtle)
                .child(s)
        };
        let mut body = div().flex().flex_col();
        if q.is_empty() {
            body = body.child(group_head("LOGIC".into()));
            for (n, (glyph, name, desc, kind)) in [
                (
                    "filter",
                    "Only continue if",
                    "Stop the run unless conditions hold",
                    NewStep::Filter,
                ),
                (
                    "edit",
                    "Set values",
                    "Compute text for later steps",
                    NewStep::Set,
                ),
                (
                    "zap",
                    "Signal other workflows",
                    "Start workflows listening for it",
                    NewStep::Emit,
                ),
                (
                    "bot",
                    "Hand to an agent",
                    "One of your computers or tab cloud runs it, then the run carries on",
                    NewStep::Agent,
                ),
                (
                    "terminal",
                    "Run a command",
                    "A program in tab's sandbox, or in a container",
                    NewStep::Command,
                ),
            ]
            .into_iter()
            .enumerate()
            {
                body = body.child(entry(
                    SharedString::from(format!("wf-add-logic-{n}")),
                    glyph,
                    name.into(),
                    desc.into(),
                    false,
                    None,
                    kind,
                    cx,
                ));
            }
        }
        for c in self.connectors.clone() {
            let mut actions: Vec<_> = c
                .descriptor
                .actions
                .iter()
                .filter(|a| {
                    q.is_empty()
                        || format!("{} {} {}", a.label, a.description, c.descriptor.name)
                            .to_lowercase()
                            .contains(&q)
                })
                .cloned()
                .collect();
            if actions.is_empty() {
                continue;
            }
            actions.sort_by_key(|a| !fits(a));
            body = body.child(group_head(format!(
                "{}{}",
                c.descriptor.name.to_uppercase(),
                if c.enabled { "" } else { " · NOT INSTALLED" }
            )));
            for a in actions {
                let right = (a.effect != Effect::Read).then(|| {
                    let (label, tone) = if a.effect == Effect::Destructive {
                        ("Destructive", t.danger)
                    } else {
                        ("Writes", t.info)
                    };
                    badge(label, tone.fg, tone.bg).into_any_element()
                });
                body = body.child(entry(
                    SharedString::from(format!("wf-add-{}-{}", c.descriptor.id, a.id)),
                    &a.icon,
                    a.label.clone(),
                    a.description.clone(),
                    !fits(&a),
                    right,
                    NewStep::Action {
                        connector: c.descriptor.id.clone(),
                        action: a.id.clone(),
                    },
                    cx,
                ));
            }
        }
        div()
            .w(px(380.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(Input::new(&self.add_query).small())
            .child(
                div()
                    .id("wf-add-scroll")
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .child(body),
            )
            .into_any_element()
    }

    fn render_canvas(
        &mut self,
        w: &Workflow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let mut flow = div()
            .flex()
            .flex_col()
            .items_center()
            .py(px(28.))
            .child(self.trigger_node(w, &t, cx));
        for i in 0..w.steps.len() {
            flow = flow
                .child(self.link(i, false, window, cx))
                .child(self.step_node(w, i, &t, cx));
        }
        flow = flow.child(self.link(w.steps.len(), true, window, cx));
        if w.steps.is_empty() {
            flow = flow.child(
                div()
                    .mt(px(14.))
                    .w(px(360.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(size::TEXT_MD))
                    .text_color(t.fg_subtle)
                    .child(
                        "Add steps with the + above, or describe what you want and the assistant builds it here.",
                    )
                    .when(self.panel != Panel::Assistant, |el| {
                        el.child(
                            Button::new("wf-canvas-ask")
                                .icon("sparkles")
                                .label("Ask the assistant")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.panel = Panel::Assistant;
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }
        let general = self.issues_at(None);
        if !general.is_empty() {
            flow = flow.child(
                div()
                    .mt(px(16.))
                    .w(px(440.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .children(general.iter().map(|i| issue_line(i, &t))),
            );
        }
        div()
            .id("wf-canvas")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .bg(t.bg_sunken)
            .child(flow)
            .into_any_element()
    }

    fn render_side(
        &mut self,
        w: &Workflow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let step_open = matches!(self.selected, Sel::Step(i) if i < w.steps.len());
        let tabs = [
            (
                Panel::Edit,
                if step_open { "Step" } else { "Trigger" },
                None,
            ),
            (Panel::Runs, "Runs", None),
            (Panel::Insights, "Insights", Some("gauge")),
            (Panel::Assistant, "Assistant", Some("sparkles")),
        ];
        let mut bar = div()
            .flex()
            .gap(px(2.))
            .px(px(10.))
            .border_b_1()
            .border_color(t.border);
        for (p, label, glyph) in tabs {
            let active = self.panel == p;
            bar = bar.child(
                div()
                    .id(SharedString::from(format!("wf-panel-{label}")))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(10.))
                    .py(px(9.))
                    .cursor_pointer()
                    .text_size(px(size::TEXT_MD))
                    .border_b_2()
                    .border_color(if active {
                        t.fg
                    } else {
                        gpui::transparent_black()
                    })
                    .text_color(if active { t.fg } else { t.fg_muted })
                    .when_some(glyph, |el, g| {
                        el.child(icon(g, 12., if active { t.fg } else { t.fg_muted }))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.panel = p;
                        if p == Panel::Insights {
                            this.load_insights(cx);
                        }
                        cx.notify();
                    })),
            );
        }
        let body: AnyElement = match self.panel {
            Panel::Edit => match self.selected {
                Sel::Step(i) if i < w.steps.len() => self.step_editor(w, i, window, cx),
                _ => self.trigger_editor(w, window, cx),
            },
            Panel::Runs => self.runs_panel(w, cx),
            Panel::Insights => self.insights_panel(w, cx),
            Panel::Assistant => {
                let a = self
                    .assistant
                    .get_or_insert_with(|| {
                        let target = Target::Workflow(Box::new(w.clone()));
                        cx.new(|cx| ChatView::of(target, window, cx))
                    })
                    .clone();
                div().size_full().child(a).into_any_element()
            }
        };
        let scrolls = self.panel != Panel::Assistant;
        div()
            .w(px(420.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(t.border)
            .bg(t.bg_raised)
            .child(bar)
            .child(if scrolls {
                div()
                    .id("wf-side-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body)
                    .into_any_element()
            } else {
                div().flex_1().min_h_0().child(body).into_any_element()
            })
            .into_any_element()
    }

    fn render_builder(
        &mut self,
        w: &Workflow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let bar = self.render_bar(w, cx);
        let confirm = self.confirm.clone().map(|c| self.render_confirm(&c, cx));
        let dry = self.dry.clone().map(|d| self.render_dry(&d, cx));
        let canvas = self.render_canvas(w, window, cx);
        let side = self.render_side(w, window, cx);
        div()
            .size_full()
            .flex()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(bar)
                    .children(confirm)
                    .when_some(self.error.clone(), |el, e| {
                        el.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .mx(px(16.))
                                .mt(px(10.))
                                .px(px(12.))
                                .py(px(8.))
                                .rounded(px(size::RADIUS_MD))
                                .bg(t.danger.bg)
                                .text_color(t.danger.fg)
                                .text_size(px(size::TEXT_MD))
                                .child(icon("alert", 13., t.danger.fg))
                                .child(e),
                        )
                    })
                    .children(dry)
                    .child(canvas),
            )
            .child(side)
            .into_any_element()
    }

    fn render_blank(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .child(icon("workflow", 30., t.fg_subtle))
            .child(
                div()
                    .text_size(px(size::TEXT_XL))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Automate the busywork"),
            )
            .child(
                div()
                    .max_w(px(460.))
                    .text_size(px(size::TEXT_MD))
                    .text_color(t.fg_muted)
                    .child("A workflow starts when something happens (a review request, a CI failure, a newsletter, a time of day) and runs steps your connections offer: comment, label, snooze, create a task, signal other workflows."),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        Button::new("wf-blank-new")
                            .kind(ButtonKind::Primary)
                            .icon("plus")
                            .label("New workflow")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open(blank(), Panel::Edit, window, cx)
                            })),
                    )
                    .child(
                        Button::new("wf-blank-ai")
                            .icon("sparkles")
                            .label("Describe it to the assistant")
                            .on_click(cx.listener(|this, _, window, cx| this.ask_ai(window, cx))),
                    ),
            )
            .into_any_element()
    }
}

fn chip(s: String, t: &Theme) -> AnyElement {
    div()
        .px(px(7.))
        .py(px(2.))
        .rounded(px(size::RADIUS_SM))
        .bg(t.bg_sunken)
        .border_1()
        .border_color(t.border)
        .text_size(px(size::TEXT_SM))
        .text_color(t.fg_muted)
        .child(s)
        .into_any_element()
}

fn issue_line(i: &Issue, t: &Theme) -> AnyElement {
    let tone = if i.error { t.danger } else { t.warning };
    div()
        .flex()
        .items_start()
        .gap(px(6.))
        .px(px(10.))
        .py(px(6.))
        .rounded(px(size::RADIUS_MD))
        .bg(tone.bg)
        .text_color(tone.fg)
        .text_size(px(size::TEXT_SM))
        .child(
            div()
                .pt(px(1.))
                .child(icon(if i.error { "alert" } else { "dot" }, 12., tone.fg)),
        )
        .child(div().flex_1().child(i.message.clone()))
        .into_any_element()
}

impl Render for WorkflowsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let list = self.render_list(window, cx);
        let main = match self.view {
            View::Factory => self.factory_view(cx),
            View::Runs => self.all_runs_view(cx),
            View::Builder => match self.draft.clone() {
                Some(w) => self.render_builder(&w, window, cx),
                None => self.render_blank(cx),
            },
        };
        // A confirmation raised from the list while the builder isn't
        // showing still needs an answer.
        let confirm = if self.view != View::Builder {
            self.confirm.clone().map(|c| self.render_confirm(&c, cx))
        } else {
            None
        };
        div()
            .size_full()
            .flex()
            .bg(t.bg)
            .text_color(t.fg)
            .child(list)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .children(confirm)
                    .child(div().flex_1().min_h_0().child(main)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_blank_workflow_starts_on_new_items() {
        let w = blank();
        assert!(w.id.is_empty());
        assert!(w.steps.is_empty());
        assert_eq!(describe_trigger(&w.trigger, &[]), "When an item is new");
    }

    #[test]
    fn new_steps_get_ids_of_their_own() {
        let mut w = blank();
        let s = new_step(&w, &NewStep::Filter, &[]);
        assert_eq!(s.id, "only_if");
        w.steps.push(s);
        assert_eq!(new_step(&w, &NewStep::Filter, &[]).id, "only_if_2");
        let a = new_step(
            &w,
            &NewStep::Action {
                connector: "github".into(),
                action: "pr.comment".into(),
            },
            &[],
        );
        assert_eq!(a.id, "comment");
        assert_eq!(unique_id(&w, "Hello there!"), "hello_there");
    }

    #[test]
    fn steps_can_use_what_came_before() {
        let mut w = blank();
        w.steps.push(new_step(&w, &NewStep::Agent, &[]));
        w.steps.push(new_step(&w, &NewStep::Set, &[]));
        let first = variables(&w, 0, &[]);
        assert!(first.iter().any(|(p, _)| p == "item.title"));
        assert!(!first.iter().any(|(p, _)| p.starts_with("steps.")));
        let later = variables(&w, 2, &[]);
        assert!(later.iter().any(|(p, _)| p == "steps.agent.pr_url"));
        assert!(later.iter().any(|(p, _)| p == "steps.values.name"));
        assert_eq!(later.last().map(|v| v.0.as_str()), Some("now"));
    }

    #[test]
    fn the_runs_summary_counts_today() {
        assert_eq!(runs_summary(&[]), "None yet");
    }
}
