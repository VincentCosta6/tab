//! Workflows: the list, one workflow's flow, its definition, runs, and
//! answering a run that waits on you. The counterpart of
//! `src/modules/workflows/WorkflowsTab.tsx`.
//!
//! Workflows are edited as their JSON definition here, checked as you type;
//! the drag-and-drop step builder, insights and the factory view are still
//! only in the Tauri app.

use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::Sizable;
use gpui_component::button::Button as MenuButton;
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::switch::Switch;
use tab_core::EngineEvent;
use tab_core::model::{Change, ConnectorInfo};
use tab_core::workflow::{Issue, Run, RunStatus, Step, StepKind, StepStatus, Trigger, Workflow};

use crate::backend::backend;
use crate::components::{Button, ButtonKind, ago, badge, icon};
use crate::theme::{FONT_MONO, Theme, Tone, size, theme};

fn change_label(c: &Change) -> &'static str {
    match c {
        Change::Ingested => "new",
        Change::Updated => "updated",
        Change::Ranked => "ranked",
        Change::Snoozed => "snoozed",
        Change::Woke => "woke",
        Change::Done => "done",
        Change::Reopened => "reopened",
    }
}

/// "When an item is new from GitHub", "Every 2 hours".
pub fn describe_trigger(t: &Trigger, connectors: &[ConnectorInfo]) -> String {
    match t {
        Trigger::Item {
            changes, sources, ..
        } => {
            let ch = changes
                .iter()
                .map(change_label)
                .collect::<Vec<_>>()
                .join(" or ");
            let src = sources
                .iter()
                .map(|s| {
                    connectors
                        .iter()
                        .find(|c| c.descriptor.id == *s)
                        .map_or(s.clone(), |c| c.descriptor.name.clone())
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "When an item is {}{}",
                if ch.is_empty() { "…" } else { &ch },
                if src.is_empty() {
                    String::new()
                } else {
                    format!(" from {src}")
                }
            )
        }
        Trigger::Signal { name, .. } => format!(
            "When a workflow signals {}",
            if name.is_empty() { "…" } else { name }
        ),
        Trigger::Schedule { every_minutes: m } => {
            let m = *m;
            if m > 0 && m % 1440 == 0 {
                if m == 1440 {
                    "Every day".into()
                } else {
                    format!("Every {} days", m / 1440)
                }
            } else if m > 0 && m % 60 == 0 {
                if m == 60 {
                    "Every hour".into()
                } else {
                    format!("Every {} hours", m / 60)
                }
            } else {
                format!("Every {m} minutes")
            }
        }
        Trigger::Manual => "When you run it".into(),
        #[allow(unreachable_patterns)]
        _ => "When it's triggered".into(),
    }
}

/// What a step does, in a few words, and its icon.
pub fn describe_step(s: &Step) -> (&'static str, String) {
    let (icon, what) = match &s.kind {
        StepKind::Action {
            connector, action, ..
        } => ("zap", format!("{connector}: {action}")),
        StepKind::Filter { conditions } => (
            "filter",
            format!(
                "Continue only if {} condition{} hold",
                conditions.len(),
                if conditions.len() == 1 { "" } else { "s" }
            ),
        ),
        StepKind::Set { values } => ("edit", format!("Set {} value(s)", values.len())),
        StepKind::Emit { signal, .. } => ("send", format!("Signal {signal}")),
        StepKind::Command { program, .. } => ("terminal", format!("Run {program}")),
        StepKind::Agent { prompt, .. } => {
            let p: String = prompt.chars().take(80).collect();
            ("bot", format!("Agent: {p}"))
        }
        #[allow(unreachable_patterns)]
        _ => ("dot", s.id.clone()),
    };
    let what = if s.label.is_empty() {
        what
    } else {
        s.label.clone()
    };
    (icon, what)
}

fn run_tone(t: &Theme, s: RunStatus) -> Tone {
    match s {
        RunStatus::Succeeded => t.success,
        RunStatus::Failed | RunStatus::LoopStopped => t.danger,
        RunStatus::Blocked | RunStatus::Throttled => t.warning,
        RunStatus::Running | RunStatus::Waiting => t.info,
        RunStatus::Stopped => t.neutral,
    }
}

fn run_label(s: RunStatus) -> &'static str {
    match s {
        RunStatus::Running => "Running",
        RunStatus::Succeeded => "Succeeded",
        RunStatus::Stopped => "Stopped by a filter",
        RunStatus::Failed => "Failed",
        RunStatus::Blocked => "Waiting for your OK",
        RunStatus::Throttled => "Throttled",
        RunStatus::LoopStopped => "Loop stopped",
        RunStatus::Waiting => "Waiting on a runner",
    }
}

fn step_glyph(s: StepStatus) -> &'static str {
    match s {
        StepStatus::Ok => "check",
        StepStatus::Skipped => "minus",
        StepStatus::Stopped => "filter",
        StepStatus::Failed => "x-circle",
        StepStatus::Blocked => "lock",
        StepStatus::Throttled => "clock",
        StepStatus::Dry => "eye",
        StepStatus::Waiting => "clock",
        #[allow(unreachable_patterns)]
        _ => "dot",
    }
}

fn blank() -> Workflow {
    serde_json::from_value(serde_json::json!({
        "id": "", "name": "New workflow", "description": "", "enabled": false,
        "allow_destructive": false,
        "trigger": { "type": "item", "changes": ["ingested"], "sources": [], "kinds": [], "when": [] },
        "steps": [], "limits": { "max_runs_per_hour": 30, "cooldown_minutes": 0 },
        "created_by": "user", "version": 0, "updated_at": null
    }))
    .expect("a blank workflow")
}

pub struct WorkflowsTab {
    workflows: Vec<Workflow>,
    templates: Vec<Workflow>,
    connectors: Vec<ConnectorInfo>,
    /// The workflow open, as edited.
    draft: Option<Workflow>,
    /// Whether the draft differs from what's saved.
    dirty: bool,
    parse_error: Option<String>,
    issues: Vec<Issue>,
    runs: Vec<Run>,
    open_run: Option<String>,
    note: Option<String>,
    busy: bool,
    editor: Entity<TextareaState>,
    _subs: Vec<Subscription>,
}

impl WorkflowsTab {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| TextareaState::new(window, cx).auto_grow(12, 40));
        let edited = cx.subscribe(&editor, |this, s, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                let text = s.read(cx).value().to_string();
                match serde_json::from_str::<Workflow>(&text) {
                    Ok(w) => {
                        this.parse_error = None;
                        this.draft = Some(w);
                        this.dirty = true;
                        this.check(cx);
                    }
                    Err(e) => this.parse_error = Some(e.to_string()),
                }
                cx.notify();
            }
        });
        let events = crate::events::events(cx);
        let live = cx.subscribe(&events, |this, _, ev: &Arc<EngineEvent>, cx| match &**ev {
            EngineEvent::WorkflowsChanged { .. } => this.reload(cx),
            EngineEvent::WorkflowRun { run }
                if this.draft.as_ref().is_some_and(|w| w.id == run.workflow_id) =>
            {
                this.runs.retain(|r| r.id != run.id);
                this.runs.insert(0, run.clone());
                cx.notify();
            }
            _ => {}
        });
        let b = backend(cx);
        let mut this = WorkflowsTab {
            workflows: Vec::new(),
            templates: b.workflow_templates(),
            connectors: Vec::new(),
            draft: None,
            dirty: false,
            parse_error: None,
            issues: Vec::new(),
            runs: Vec::new(),
            open_run: None,
            note: None,
            busy: false,
            editor,
            _subs: vec![edited, live],
        };
        let task =
            crate::runtime::spawn(async move { b.with_engine(async |e| Ok(e.connectors())).await });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(cs)) = task.await {
                this.update(cx, |this, cx| {
                    this.connectors = cs;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        this.reload(cx);
        if let Some(w) = this.workflows.first().cloned() {
            this.open(w, window, cx);
        }
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        match backend(cx).workflows() {
            Ok(ws) => self.workflows = ws,
            Err(e) => self.note = Some(e),
        }
        // Keep the open one in step, unless it has edits.
        if !self.dirty
            && let Some(d) = &self.draft
            && let Some(w) = self.workflows.iter().find(|w| w.id == d.id)
        {
            self.draft = Some(w.clone());
        }
        cx.notify();
    }

    fn open(&mut self, w: Workflow, window: &mut Window, cx: &mut Context<Self>) {
        let text = serde_json::to_string_pretty(&w).unwrap_or_default();
        self.editor
            .update(cx, |s, cx| s.set_value(text, window, cx));
        self.runs = if w.id.is_empty() {
            Vec::new()
        } else {
            backend(cx)
                .workflow_runs(Some(&w.id), 30)
                .unwrap_or_default()
        };
        self.dirty = w.id.is_empty();
        self.draft = Some(w);
        self.parse_error = None;
        self.note = None;
        self.open_run = None;
        self.check(cx);
        cx.notify();
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
        self.busy = true;
        let b = backend(cx);
        let task = crate::runtime::spawn(async move { b.workflow_save(w).await });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await.and_then(|r| r);
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match r {
                    Ok((w, issues)) => {
                        this.note = Some(if issues.iter().any(|i| i.error) {
                            "Saved, turned off until its errors are fixed.".into()
                        } else {
                            "Saved.".into()
                        });
                        this.reload(cx);
                        this.open(w, window, cx);
                        this.issues = issues;
                    }
                    Err(e) => this.note = Some(format!("Couldn't save: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_enabled(&mut self, id: String, on: bool, cx: &mut Context<Self>) {
        let b = backend(cx);
        let task = crate::runtime::spawn(async move { b.workflow_set_enabled(&id, on).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.and_then(|r| r);
            this.update(cx, |this, cx| {
                if let Err(e) = r {
                    this.note = Some(e);
                }
                this.reload(cx);
            })
            .ok();
        })
        .detach();
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.draft.as_ref().map(|w| w.id.clone()) else {
            return;
        };
        if !id.is_empty()
            && let Err(e) = backend(cx).workflow_delete(&id)
        {
            self.note = Some(e);
            return;
        }
        self.draft = None;
        self.dirty = false;
        self.reload(cx);
        if let Some(w) = self.workflows.first().cloned() {
            self.open(w, window, cx);
        }
    }

    fn run(&mut self, dry: bool, cx: &mut Context<Self>) {
        let Some(w) = self.draft.clone() else { return };
        self.busy = true;
        self.note = None;
        let b = backend(cx);
        let task = crate::runtime::spawn(async move { b.workflow_run(w, None, dry).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.and_then(|r| r);
            this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(run) => {
                        this.open_run = Some(run.id.clone());
                        this.runs.retain(|r| r.id != run.id);
                        this.runs.insert(0, run);
                    }
                    Err(e) => this.note = Some(format!("Couldn't run: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
                        if let Some(slot) = this.runs.iter_mut().find(|r| r.id == run.id) {
                            *slot = run;
                        }
                    }
                    Err(e) => this.note = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_list(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme(cx).clone();
        let selected = self.draft.as_ref().map(|w| w.id.clone());
        let mut list = div().flex().flex_col().gap(px(2.));
        for w in &self.workflows {
            let active = selected.as_ref() == Some(&w.id);
            let (open, id, on) = (w.clone(), w.id.clone(), w.enabled);
            let this = cx.entity().downgrade();
            list = list.child(
                div()
                    .id(SharedString::from(format!("wf-{}", w.id)))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(size::RADIUS_MD))
                    .cursor_pointer()
                    .when(active, |el| el.bg(t.bg_raised))
                    .hover(|s| s.bg(t.bg_hover))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(size::TEXT_MD))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .truncate()
                                    .child(w.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(size::TEXT_SM))
                                    .text_color(t.fg_subtle)
                                    .truncate()
                                    .child(describe_trigger(&w.trigger, &self.connectors)),
                            ),
                    )
                    .child(
                        Switch::new(SharedString::from(format!("wf-on-{}", w.id)))
                            .checked(on)
                            .on_click(move |v, _, cx| {
                                let v = *v;
                                let id = id.clone();
                                this.update(cx, |this, cx| this.set_enabled(id, v, cx)).ok();
                            }),
                    )
                    .on_click(cx.listener(move |this, _, w, cx| this.open(open.clone(), w, cx))),
            );
        }
        if self.workflows.is_empty() {
            list = list.child(
                div()
                    .px(px(10.))
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_subtle)
                    .child("No workflows yet. Start from a template, or a blank one."),
            );
        }
        let this = cx.entity().downgrade();
        let templates = self.templates.clone();
        let new = MenuButton::new("new-wf")
            .outline()
            .small()
            .icon(gpui_component::Icon::default().path(crate::assets::icon_path("plus")))
            .label("New")
            .dropdown_menu(move |mut menu, _, _| {
                let blank_this = this.clone();
                menu = menu.item(PopupMenuItem::new("Blank workflow").on_click(
                    move |_, window, cx| {
                        blank_this
                            .update(cx, |v, cx| v.open(blank(), window, cx))
                            .ok();
                    },
                ));
                for tmpl in &templates {
                    let this = this.clone();
                    let mut w = tmpl.clone();
                    w.id = String::new();
                    w.enabled = false;
                    w.created_by = "template".into();
                    menu = menu.item(PopupMenuItem::new(tmpl.name.clone()).on_click(
                        move |_, window, cx| {
                            let w = w.clone();
                            this.update(cx, |v, cx| v.open(w, window, cx)).ok();
                        },
                    ));
                }
                menu
            });
        div()
            .id("wf-list")
            .w(px(300.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(10.))
            .py(px(20.))
            .border_r_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(10.))
                    .pb(px(6.))
                    .child(
                        div()
                            .text_size(px(size::TEXT_XL))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Workflows"),
                    )
                    .child(new),
            )
            .child(list)
            .into_any_element()
    }

    fn render_flow(&self, w: &Workflow, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let issues_at = |at: &str| -> Vec<Issue> {
            self.issues
                .iter()
                .filter(|i| i.at.as_deref() == Some(at))
                .cloned()
                .collect()
        };
        let node = |icon_name: &'static str, title: String, sub: String, issues: Vec<Issue>| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(px(14.))
                .py(px(10.))
                .rounded(px(size::RADIUS_LG))
                .border_1()
                .border_color(if issues.iter().any(|i| i.error) {
                    t.danger.fg
                } else {
                    t.border
                })
                .bg(t.bg_raised)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(icon(icon_name, 14., t.fg_muted))
                        .child(
                            div()
                                .text_size(px(size::TEXT_MD))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(title),
                        ),
                )
                .when(!sub.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(size::TEXT_SM))
                            .text_color(t.fg_subtle)
                            .child(sub),
                    )
                })
                .children(issues.into_iter().map(|i| {
                    div()
                        .text_size(px(size::TEXT_SM))
                        .text_color(if i.error { t.danger.fg } else { t.warning.fg })
                        .child(i.message)
                }))
        };
        let arrow = || {
            div()
                .flex()
                .justify_center()
                .py(px(2.))
                .child(icon("chevron-down", 14., t.fg_subtle))
        };
        let mut flow = div().flex().flex_col().gap(px(2.)).child(node(
            "zap",
            describe_trigger(&w.trigger, &self.connectors),
            "Trigger".into(),
            issues_at("trigger"),
        ));
        for s in &w.steps {
            let (ic, what) = describe_step(s);
            flow = flow.child(arrow()).child(node(
                ic,
                what,
                format!(
                    "{}{}",
                    s.id,
                    if s.when.is_empty() {
                        String::new()
                    } else {
                        format!(" · only when {} hold", s.when.len())
                    }
                ),
                issues_at(&s.id),
            ));
        }
        if w.steps.is_empty() {
            flow = flow.child(arrow()).child(
                div()
                    .px(px(14.))
                    .py(px(10.))
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_subtle)
                    .child("No steps yet. Add them to `steps` in the definition."),
            );
        }
        flow
    }

    fn render_runs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let mut col = div().flex().flex_col().gap(px(6.));
        if self.runs.is_empty() {
            col = col.child(
                div()
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_subtle)
                    .child("No runs yet."),
            );
        }
        for r in self.runs.iter().take(30) {
            let tone = run_tone(&t, r.status);
            let open = self.open_run.as_ref() == Some(&r.id);
            let id = r.id.clone();
            let mut card = div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(size::RADIUS_MD))
                .border_1()
                .border_color(t.border)
                .bg(t.bg_raised)
                .child(
                    div()
                        .id(SharedString::from(format!("run-{}", r.id)))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .child(badge(run_label(r.status), tone.fg, tone.bg))
                        .when(r.dry_run, |el| {
                            el.child(badge("Dry run", t.fg_muted, t.bg_sunken))
                        })
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(size::TEXT_SM))
                                .text_color(t.fg_muted)
                                .child(r.trigger.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(size::TEXT_SM))
                                .text_color(t.fg_subtle)
                                .child(ago(r.started_at, chrono::Utc::now())),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_run = if this.open_run.as_ref() == Some(&id) {
                                None
                            } else {
                                Some(id.clone())
                            };
                            cx.notify();
                        })),
                );
            if r.status == RunStatus::Blocked {
                let (a, d) = (r.id.clone(), r.id.clone());
                card = card.child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(
                            Button::new(SharedString::from(format!("approve-{}", r.id)))
                                .kind(ButtonKind::Primary)
                                .label("Approve this step")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.answer(a.clone(), true, cx)
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("decline-{}", r.id)))
                                .label("Decline")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.answer(d.clone(), false, cx)
                                })),
                        ),
                );
            }
            if open {
                for s in &r.steps {
                    card = card.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .text_size(px(size::TEXT_SM))
                                    .child(icon(step_glyph(s.status), 12., t.fg_muted))
                                    .child(if s.label.is_empty() {
                                        s.id.clone()
                                    } else {
                                        s.label.clone()
                                    })
                                    .child(
                                        div().text_color(t.fg_subtle).child(format!("{} ms", s.ms)),
                                    ),
                            )
                            .when_some(s.error.clone(), |el, e| {
                                el.child(
                                    div()
                                        .text_size(px(size::TEXT_SM))
                                        .text_color(t.danger.fg)
                                        .child(e),
                                )
                            }),
                    );
                }
                if let Some(e) = &r.error
                    && !r.steps.iter().any(|s| s.error.as_ref() == Some(e))
                {
                    card = card.child(
                        div()
                            .text_size(px(size::TEXT_SM))
                            .text_color(t.danger.fg)
                            .child(e.clone()),
                    );
                }
            }
            col = col.child(card);
        }
        col
    }

    fn render_open(&self, w: &Workflow, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme(cx).clone();
        let label = |s: &'static str| {
            div()
                .text_size(px(size::TEXT_XS))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(t.fg_subtle)
                .child(s)
        };
        let general: Vec<_> = self
            .issues
            .iter()
            .filter(|i| i.at.is_none())
            .cloned()
            .collect();
        let saved = !w.id.is_empty();
        div()
            .id("wf-open")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .px(px(32.))
                    .py(px(24.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .text_size(px(size::TEXT_2XL))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(w.name.clone()),
                                    )
                                    .when(!w.description.is_empty(), |el| {
                                        el.child(
                                            div()
                                                .text_size(px(size::TEXT_MD))
                                                .text_color(t.fg_muted)
                                                .child(w.description.clone()),
                                        )
                                    }),
                            )
                            .child(
                                Button::new("dry")
                                    .icon("eye")
                                    .label("Dry run")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.run(true, cx))),
                            )
                            .when(saved, |el| {
                                el.child(
                                    Button::new("run")
                                        .icon("arrow-right")
                                        .label("Run now")
                                        .disabled(self.busy || self.dirty)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.run(false, cx)),
                                        ),
                                )
                            })
                            .child(
                                Button::new("save-wf")
                                    .kind(ButtonKind::Primary)
                                    .label(if self.busy { "Saving…" } else { "Save" })
                                    .disabled(
                                        self.busy || !self.dirty || self.parse_error.is_some(),
                                    )
                                    .on_click(cx.listener(|this, _, w, cx| this.save(w, cx))),
                            )
                            .child(
                                Button::new("delete-wf")
                                    .kind(ButtonKind::Ghost)
                                    .icon("trash")
                                    .on_click(cx.listener(|this, _, w, cx| this.delete(w, cx))),
                            ),
                    )
                    .when_some(self.note.clone(), |el, n| {
                        el.child(
                            div()
                                .text_size(px(size::TEXT_MD))
                                .text_color(t.fg_muted)
                                .child(n),
                        )
                    })
                    .children(general.into_iter().map(|i| {
                        div()
                            .text_size(px(size::TEXT_SM))
                            .text_color(if i.error { t.danger.fg } else { t.warning.fg })
                            .child(i.message)
                    }))
                    .child(
                        div()
                            .flex()
                            .gap(px(24.))
                            .child(
                                div()
                                    .w(px(360.))
                                    .flex_none()
                                    .flex()
                                    .flex_col()
                                    .gap(px(10.))
                                    .child(label("FLOW"))
                                    .child(self.render_flow(w, cx))
                                    .child(label("RUNS"))
                                    .child(self.render_runs(cx)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(10.))
                                    .child(label("DEFINITION"))
                                    .child(
                                        div()
                                            .font_family(FONT_MONO)
                                            .text_size(px(size::TEXT_SM))
                                            .child(Textarea::new(&self.editor)),
                                    )
                                    .when_some(self.parse_error.clone(), |el, e| {
                                        el.child(
                                            div()
                                                .text_size(px(size::TEXT_SM))
                                                .text_color(t.danger.fg)
                                                .child(e),
                                        )
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for WorkflowsTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let list = self.render_list(cx);
        let main = match self.draft.clone() {
            Some(w) => self.render_open(&w, cx),
            None => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .child(icon("workflow", 28., t.fg_subtle))
                .child(
                    div()
                        .text_size(px(size::TEXT_MD))
                        .text_color(t.fg_muted)
                        .child("Pick a workflow, or start one with New."),
                )
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .bg(t.bg)
            .child(list)
            .child(div().flex_1().min_w_0().h_full().child(main))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_read_naturally() {
        let d = |m| describe_trigger(&Trigger::Schedule { every_minutes: m }, &[]);
        assert_eq!(d(1440), "Every day");
        assert_eq!(d(2880), "Every 2 days");
        assert_eq!(d(60), "Every hour");
        assert_eq!(d(120), "Every 2 hours");
        assert_eq!(d(15), "Every 15 minutes");
    }

    #[test]
    fn the_blank_workflow_parses() {
        let w = blank();
        assert!(w.id.is_empty());
        assert!(w.steps.is_empty());
        assert_eq!(describe_trigger(&w.trigger, &[]), "When an item is new");
    }
}
