//! The inspector's forms: the trigger, and the step selected on the
//! canvas. The counterparts of `TriggerEditor`, `StepEditor`,
//! `AgentFields` and `CommandFields` in `WorkflowsTab.tsx`.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*,
    px,
};
use serde_json::{Map, Value};
use tab_core::model::Effect;
use tab_core::runner::{Placement, RunnerKind};
use tab_core::workflow::{Condition, Step, StepKind, Trigger, Workflow};

use super::form::{
    AREA, AREA_MONO, Choice, Commit, LINE, MONO, action_chip, field, lens, note, outputs, row2,
    safety, sanitize_id, section, toggle_chip,
};
use super::format::{CHANGES, connector_name};
use super::{Sel, WorkflowsTab, find_action, harness_name, harnesses, issue_line, variables};
use crate::components::{icon, segmented};
use crate::theme::{size, theme};

fn item_trigger(
    w: &mut Workflow,
) -> Option<(
    &mut Vec<tab_core::model::Change>,
    &mut Vec<String>,
    &mut Vec<String>,
)> {
    match &mut w.trigger {
        Trigger::Item {
            changes,
            sources,
            kinds,
            ..
        } => Some((changes, sources, kinds)),
        _ => None,
    }
}

fn toggle<T: PartialEq + Clone>(v: &mut Vec<T>, x: &T) {
    if let Some(i) = v.iter().position(|y| y == x) {
        v.remove(i);
    } else {
        v.push(x.clone());
    }
}

type StepAction = dyn Fn(&mut WorkflowsTab, &mut Context<WorkflowsTab>);

#[derive(Clone, Copy, PartialEq)]
enum TriggerType {
    Item,
    Schedule,
    Signal,
    Manual,
}

impl WorkflowsTab {
    pub(super) fn trigger_editor(
        &mut self,
        w: &Workflow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let connectors = self.connectors.clone();
        let current = match &w.trigger {
            Trigger::Item { .. } => TriggerType::Item,
            Trigger::Schedule { .. } => TriggerType::Schedule,
            Trigger::Signal { .. } => TriggerType::Signal,
            Trigger::Manual => TriggerType::Manual,
        };
        let this = cx.entity().downgrade();
        let seg = segmented(
            "wf-trigger-type",
            &[
                (TriggerType::Item, "Item"),
                (TriggerType::Schedule, "Schedule"),
                (TriggerType::Signal, "Signal"),
                (TriggerType::Manual, "By hand"),
            ],
            current,
            cx,
            move |k, _, cx| {
                if k == current {
                    return;
                }
                this.update(cx, |v, cx| {
                    v.restructure(
                        |x| {
                            x.trigger = match k {
                                TriggerType::Item => Trigger::default(),
                                TriggerType::Signal => Trigger::Signal {
                                    name: String::new(),
                                    when: Vec::new(),
                                },
                                TriggerType::Schedule => Trigger::Schedule { every_minutes: 60 },
                                TriggerType::Manual => Trigger::Manual,
                            }
                        },
                        cx,
                    )
                })
                .ok();
            },
        );
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(16.))
            .child(heading(&t, "Trigger"))
            .children(
                self.issues_at(Some("trigger"))
                    .iter()
                    .map(|i| issue_line(i, &t)),
            )
            .child(seg);
        let vars = variables(w, 0, &connectors);
        match &w.trigger {
            Trigger::Item {
                changes,
                sources,
                kinds,
                when,
            } => {
                let mut ch = div().flex().flex_wrap().gap(px(5.));
                for (c, label) in CHANGES {
                    ch = ch.child(toggle_chip(
                        &t,
                        format!("wf-change-{label}"),
                        label.into(),
                        None,
                        changes.contains(&c),
                        cx,
                        move |x| {
                            if let Some((changes, _, _)) = item_trigger(x) {
                                toggle(changes, &c);
                            }
                        },
                    ));
                }
                body = body.child(field(&t, "When an item is", None, ch));
                let offered: Vec<_> = connectors
                    .iter()
                    .filter(|c| !c.descriptor.kinds.is_empty() && c.descriptor.available)
                    .cloned()
                    .collect();
                let mut src = div().flex().flex_wrap().gap(px(5.));
                for c in &offered {
                    let id = c.descriptor.id.clone();
                    let all = connectors.clone();
                    src = src.child(toggle_chip(
                        &t,
                        format!("wf-src-{id}"),
                        c.descriptor.name.clone(),
                        Some(&c.descriptor.icon),
                        sources.contains(&id),
                        cx,
                        move |x| {
                            if let Some((_, sources, kinds)) = item_trigger(x) {
                                toggle(sources, &id);
                                let s = sources.clone();
                                kinds.retain(|k| {
                                    s.is_empty()
                                        || k.contains('*')
                                        || all.iter().any(|cc| {
                                            s.contains(&cc.descriptor.id)
                                                && cc
                                                    .descriptor
                                                    .kinds
                                                    .iter()
                                                    .any(|kk| &kk.kind == k)
                                        })
                                });
                            }
                        },
                    ));
                }
                body = body.child(field(
                    &t,
                    "From",
                    Some("Narrow triggers keep workflows from feeding each other."),
                    src,
                ));
                let kind_list: Vec<_> = offered
                    .iter()
                    .filter(|c| sources.is_empty() || sources.contains(&c.descriptor.id))
                    .flat_map(|c| c.descriptor.kinds.clone())
                    .collect();
                let mut ks = div().flex().flex_wrap().gap(px(5.));
                for k in kind_list {
                    let kind = k.kind.clone();
                    ks = ks.child(toggle_chip(
                        &t,
                        format!("wf-kind-{}", k.kind),
                        k.label.clone(),
                        Some(&k.icon),
                        kinds.contains(&k.kind),
                        cx,
                        move |x| {
                            if let Some((_, _, kinds)) = item_trigger(x) {
                                toggle(kinds, &kind);
                            }
                        },
                    ));
                }
                body = body.child(field(&t, "Kinds", Some("None picked means any kind."), ks));
                let at = lens(|x: &mut Workflow| match &mut x.trigger {
                    Trigger::Item { when, .. } => Some(when),
                    _ => None,
                });
                let conds = self.conditions("t/when", when, &vars, at, window, cx);
                body = body.child(field(&t, "Only when", None, conds));
            }
            Trigger::Schedule { every_minutes } => {
                let n = self.number(
                    "t/every",
                    *every_minutes,
                    5,
                    525_600,
                    |x, n| {
                        if let Trigger::Schedule { every_minutes } = &mut x.trigger {
                            *every_minutes = n;
                        }
                    },
                    window,
                    cx,
                );
                let mut row = div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().w(px(90.)).child(n))
                    .child(
                        div()
                            .text_size(px(size::TEXT_SM))
                            .text_color(t.fg_subtle)
                            .child("minutes"),
                    );
                for m in [15u32, 60, 240, 1440] {
                    let label = if m < 60 {
                        format!("{m}m")
                    } else if m < 1440 {
                        format!("{}h", m / 60)
                    } else {
                        "1 day".into()
                    };
                    row = row.child(action_chip(
                        &t,
                        format!("wf-every-{m}"),
                        label,
                        cx,
                        move |x| {
                            x.trigger = Trigger::Schedule { every_minutes: m };
                        },
                    ));
                }
                body = body.child(field(
                    &t,
                    "Every",
                    Some("At least every 5 minutes. A day starts at midnight UTC."),
                    row,
                ));
            }
            Trigger::Signal { name, when } => {
                let n = self.input(
                    "t/signal",
                    name,
                    LINE,
                    "ci_failed",
                    Commit::Live,
                    |x, s| {
                        if let Trigger::Signal { name, .. } = &mut x.trigger {
                            *name = s;
                        }
                    },
                    window,
                    cx,
                );
                body = body.child(field(
                    &t,
                    "Signal name",
                    Some("Another workflow's Signal step with this name starts this one. Its payload is {{signal.<key>}}."),
                    n,
                ));
                let at = lens(|x: &mut Workflow| match &mut x.trigger {
                    Trigger::Signal { when, .. } => Some(when),
                    _ => None,
                });
                let conds = self.conditions("t/when", when, &vars, at, window, cx);
                body = body.child(field(&t, "Only when", None, conds));
            }
            Trigger::Manual => {
                body = body.child(note(
                    &t,
                    "Runs only when you press Run now, on the item picked in the toolbar.",
                ));
            }
        }

        body = body.child(section(&t, "About"));
        let d = self.input(
            "t/description",
            &w.description,
            AREA,
            "What it's for",
            Commit::Live,
            |x, s| x.description = s,
            window,
            cx,
        );
        body = body.child(field(&t, "Description", None, d));

        body = body.child(section(&t, "Limits and safety"));
        let per_hour = self.number(
            "t/per_hour",
            w.limits.max_runs_per_hour,
            1,
            10_000,
            |x, n| x.limits.max_runs_per_hour = n,
            window,
            cx,
        );
        let cool = self.number(
            "t/cooldown",
            w.limits.cooldown_minutes,
            0,
            100_000,
            |x, n| x.limits.cooldown_minutes = n,
            window,
            cx,
        );
        body = body.child(row2(
            field(
                &t,
                "Runs per hour",
                Some("Triggers past this are skipped."),
                per_hour,
            ),
            field(&t, "Cool-down (min)", Some("Per item. 0 is none."), cool),
        ));
        let has_destructive = w.steps.iter().any(|s| match &s.kind {
            StepKind::Action {
                connector, action, ..
            } => find_action(&connectors, connector, action)
                .is_some_and(|a| a.effect == Effect::Destructive),
            _ => false,
        });
        let has_commands = w
            .steps
            .iter()
            .any(|s| matches!(s.kind, StepKind::Command { .. }));
        let (on, allow_c) = (w.enabled, w.allow_commands);
        let this = cx.entity().downgrade();
        body = body.child(safety(
            &t,
            "wf-allow-destructive",
            "Allow destructive steps",
            format!(
                "Merging, closing and moving mail stay blocked until you allow them here.{}",
                if has_destructive {
                    ""
                } else {
                    " This workflow has none."
                }
            ),
            w.allow_destructive,
            move |v, window, cx| {
                let v = *v;
                this.update(cx, |this, cx| {
                    this.set_enabled(on, Some(v), None, window, cx)
                })
                .ok();
            },
        ));
        let this = cx.entity().downgrade();
        body = body.child(safety(
            &t,
            "wf-allow-commands",
            "Allow commands",
            format!(
                "Command steps run programs on this computer, in tab's sandbox: only their folder, the hosts they list, and none of your credentials.{}",
                if has_commands { "" } else { " This workflow has none." }
            ),
            allow_c,
            move |v, window, cx| {
                let v = *v;
                this.update(cx, |this, cx| this.set_enabled(on, None, Some(v), window, cx)).ok();
            },
        ));
        body = body.child(
            div()
                .flex()
                .items_start()
                .gap(px(6.))
                .text_size(px(size::TEXT_SM))
                .text_color(t.fg_subtle)
                .child(div().pt(px(2.)).child(icon("shield", 12., t.fg_subtle)))
                .child("A workflow never re-runs from its own changes, chains stop after three workflows, the same trigger runs once, and actions wait when a connection's rate budget is tight."),
        );
        body.into_any_element()
    }

    pub(super) fn step_editor(
        &mut self,
        w: &Workflow,
        i: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let connectors = self.connectors.clone();
        let step = &w.steps[i];
        let vars = variables(w, i, &connectors);
        let k = format!("s{i}");
        let info = match &step.kind {
            StepKind::Action {
                connector, action, ..
            } => find_action(&connectors, connector, action).cloned(),
            _ => None,
        };
        let title = match &step.kind {
            StepKind::Action { action, .. } => {
                info.as_ref().map_or(action.clone(), |a| a.label.clone())
            }
            StepKind::Filter { .. } => "Only continue if".into(),
            StepKind::Set { .. } => "Set values".into(),
            StepKind::Command { .. } => "Run a command".into(),
            StepKind::Agent { .. } => "Hand to an agent".into(),
            StepKind::Emit { .. } => "Signal".into(),
        };
        let n = w.steps.len();
        let icon_btn = |id: &str,
                        glyph: &'static str,
                        enabled: bool,
                        cx: &mut Context<Self>,
                        f: Box<StepAction>| {
            div()
                .id(SharedString::from(format!("wf-step-{id}")))
                .p(px(5.))
                .rounded(px(size::RADIUS_SM))
                .when(enabled, |el| {
                    el.cursor_pointer().hover(|s| s.bg(t.bg_hover))
                })
                .when(!enabled, |el| el.opacity(0.35))
                .child(icon(glyph, 13., t.fg_muted))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if enabled {
                        f(this, cx)
                    }
                }))
        };
        let head = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .child(div().flex_1().child(heading(&t, &title)))
            .child(icon_btn(
                "up",
                "arrow-up",
                i > 0,
                cx,
                Box::new(move |this, cx| {
                    this.restructure(|x| x.steps.swap(i, i - 1), cx);
                    this.selected = Sel::Step(i - 1);
                }),
            ))
            .child(icon_btn(
                "down",
                "arrow-down",
                i + 1 < n,
                cx,
                Box::new(move |this, cx| {
                    this.restructure(|x| x.steps.swap(i, i + 1), cx);
                    this.selected = Sel::Step(i + 1);
                }),
            ))
            .child(icon_btn(
                "remove",
                "trash",
                true,
                cx,
                Box::new(move |this, cx| {
                    this.restructure(
                        |x| {
                            if i < x.steps.len() {
                                x.steps.remove(i);
                            }
                        },
                        cx,
                    );
                    this.selected = Sel::Trigger;
                }),
            ));
        let mut body = div().flex().flex_col().gap(px(12.)).p(px(16.)).child(head);
        if let Some(a) = &info {
            let mut d = a.description.clone();
            if let Some(s) = &a.spends {
                d.push_str(&format!(" Spends from {} budget.", s.replace('/', " ")));
            }
            body = body.child(note(&t, d));
        }
        body = body.children(
            self.issues_at(Some(&step.id))
                .iter()
                .map(|x| issue_line(x, &t)),
        );

        match &step.kind {
            StepKind::Action {
                connector,
                action,
                input,
            } => {
                let mut choices = Vec::new();
                for c in connectors
                    .iter()
                    .filter(|c| !c.descriptor.actions.is_empty())
                {
                    for (j, a) in c.descriptor.actions.iter().enumerate() {
                        let (cid, aid, next) =
                            (c.descriptor.id.clone(), a.id.clone(), a.inputs.clone());
                        let mut ch = Choice::new(a.label.clone(), move |x| {
                            if let Some(StepKind::Action {
                                connector,
                                action,
                                input,
                            }) = x.steps.get_mut(i).map(|s| &mut s.kind)
                            {
                                *connector = cid.clone();
                                *action = aid.clone();
                                let mut m = Map::new();
                                for f in &next {
                                    let v = input
                                        .get(&f.key)
                                        .cloned()
                                        .or_else(|| f.default.clone().map(Value::String));
                                    if let Some(v) = v.filter(|v| v.as_str() != Some("")) {
                                        m.insert(f.key.clone(), v);
                                    }
                                }
                                *input = m;
                            }
                        });
                        if j == 0 {
                            ch = ch.group(c.descriptor.name.clone());
                        }
                        choices.push(ch);
                    }
                }
                let label = info
                    .as_ref()
                    .map_or(format!("{connector} / {action}"), |a| {
                        format!("{} · {}", connector_name(&connectors, connector), a.label)
                    });
                let sel = self.select(format!("{k}/action"), label, choices, cx);
                body = body.child(field(&t, "Action", None, sel));
                let at =
                    lens(
                        move |x: &mut Workflow| match x.steps.get_mut(i).map(|s| &mut s.kind) {
                            Some(StepKind::Action { input, .. }) => Some(input),
                            _ => None,
                        },
                    );
                for f in info.as_ref().map(|a| a.inputs.clone()).unwrap_or_default() {
                    let v = input.get(&f.key).cloned().unwrap_or(Value::Null);
                    let el = self.action_input(
                        &format!("{k}/in/{}", f.key),
                        &f,
                        &v,
                        &vars,
                        at.clone(),
                        window,
                        cx,
                    );
                    body = body.child(el);
                }
                if let Some(a) = info.as_ref().filter(|a| !a.outputs.is_empty()) {
                    body = body.child(outputs(
                        &t,
                        a.outputs
                            .iter()
                            .map(|o| format!("steps.{}.{}", step.id, o.key))
                            .collect(),
                    ));
                }
            }
            StepKind::Filter { conditions } => {
                let at =
                    lens(
                        move |x: &mut Workflow| match x.steps.get_mut(i).map(|s| &mut s.kind) {
                            Some(StepKind::Filter { conditions }) => Some(conditions),
                            _ => None,
                        },
                    );
                body = body.child(self.conditions(
                    &format!("{k}/filter"),
                    conditions,
                    &vars,
                    at,
                    window,
                    cx,
                ));
            }
            StepKind::Set { values } => {
                let at =
                    lens(
                        move |x: &mut Workflow| match x.steps.get_mut(i).map(|s| &mut s.kind) {
                            Some(StepKind::Set { values }) => Some(values),
                            _ => None,
                        },
                    );
                body = body.child(self.pairs(
                    &format!("{k}/set"),
                    values,
                    &vars,
                    "Name",
                    at,
                    window,
                    cx,
                ));
            }
            StepKind::Emit { signal, payload } => {
                let s = self.input(
                    &format!("{k}/signal"),
                    signal,
                    LINE,
                    "ci_failed",
                    Commit::Live,
                    move |x, v| {
                        if let Some(StepKind::Emit { signal, .. }) =
                            x.steps.get_mut(i).map(|s| &mut s.kind)
                        {
                            *signal = v;
                        }
                    },
                    window,
                    cx,
                );
                body = body.child(field(
                    &t,
                    "Signal name",
                    Some("Workflows with a Signal trigger of this name start, with this payload."),
                    s,
                ));
                let at =
                    lens(
                        move |x: &mut Workflow| match x.steps.get_mut(i).map(|s| &mut s.kind) {
                            Some(StepKind::Emit { payload, .. }) => Some(payload),
                            _ => None,
                        },
                    );
                let p = self.pairs(
                    &format!("{k}/payload"),
                    payload,
                    &vars,
                    "Key",
                    at,
                    window,
                    cx,
                );
                body = body.child(field(&t, "Payload", None, p));
            }
            StepKind::Command { .. } => {
                let el = self.command_fields(step, i, &vars, window, cx);
                body = body.child(el);
            }
            StepKind::Agent { .. } => {
                let el = self.agent_fields(step, i, &vars, window, cx);
                body = body.child(el);
            }
        }

        body = body.child(section(&t, "Options"));
        let id = self.input(
            &format!("{k}/id"),
            &step.id,
            MONO,
            "",
            Commit::OnLeave,
            move |x, v| {
                let v = sanitize_id(&v);
                if !v.is_empty()
                    && let Some(s) = x.steps.get_mut(i)
                {
                    s.id = v;
                }
            },
            window,
            cx,
        );
        let label = self.input(
            &format!("{k}/label"),
            &step.label,
            LINE,
            "Optional",
            Commit::Live,
            move |x, v| {
                if let Some(s) = x.steps.get_mut(i) {
                    s.label = v;
                }
            },
            window,
            cx,
        );
        body = body.child(row2(
            field(&t, "Step id", Some("How later steps refer to it."), id),
            field(&t, "Label", None, label),
        ));
        if !matches!(step.kind, StepKind::Filter { .. }) {
            let at = lens(move |x: &mut Workflow| x.steps.get_mut(i).map(|s| &mut s.when));
            let when: Vec<Condition> = step.when.clone();
            let c = self.conditions(&format!("{k}/when"), &when, &vars, at, window, cx);
            body = body.child(field(
                &t,
                "Run only if",
                Some("Otherwise this step is skipped and the run carries on."),
                c,
            ));
        }
        if matches!(step.kind, StepKind::Action { .. }) {
            let this = cx.entity().downgrade();
            body = body.child(safety(
                &t,
                format!("{k}-keep-going"),
                "Keep going if it fails",
                "Otherwise a failure ends the run.".into(),
                step.continue_on_error,
                move |v, _, cx| {
                    let v = *v;
                    this.update(cx, |this, cx| {
                        this.edit(
                            |x| {
                                if let Some(s) = x.steps.get_mut(i) {
                                    s.continue_on_error = v;
                                }
                            },
                            cx,
                        )
                    })
                    .ok();
                },
            ));
        }
        body.into_any_element()
    }

    fn agent_fields(
        &mut self,
        step: &Step,
        i: usize,
        vars: &[(String, String)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let StepKind::Agent {
            prompt,
            repo,
            harness,
            model,
            family,
            placement,
            deadline_minutes,
        } = &step.kind
        else {
            return div().into_any_element();
        };
        let k = format!("s{i}");
        macro_rules! ag {
            ($x:ident, $body:expr) => {
                move |w: &mut Workflow, v| {
                    if let Some(StepKind::Agent { $x, .. }) =
                        w.steps.get_mut(i).map(|s| &mut s.kind)
                    {
                        #[allow(clippy::redundant_closure_call)]
                        ($body)($x, v);
                    }
                }
            };
        }
        let mut out = div().flex().flex_col().gap(px(12.));
        let p = self.var_input(
            &format!("{k}/prompt"),
            prompt,
            AREA,
            "Fix the failing check on {{item.url}} and push to the PR's branch.",
            vars,
            ag!(prompt, |p: &mut String, v: String| *p = v),
            window,
            cx,
        );
        out = out.child(field(
            &t,
            "Ask the agent",
            Some(
                "Templates like {{item.title}} and {{item.url}} are filled in when the step runs.",
            ),
            p,
        ));
        let r = self.var_input(
            &format!("{k}/repo"),
            repo,
            MONO,
            "{{item.metadata.repo}}",
            vars,
            ag!(repo, |r: &mut String, v: String| *r = v),
            window,
            cx,
        );
        // The model: a family, one harness and model, or the runner's default.
        let pinned = !harness.is_empty() || !model.is_empty();
        let current = if pinned {
            "A specific harness and model".to_string()
        } else if family.is_empty() {
            "The runner's default".into()
        } else {
            self.families.iter().find(|f| &f.id == family).map_or(
                format!("{family} (missing)"),
                |f| {
                    if f.name.is_empty() {
                        f.id.clone()
                    } else {
                        f.name.clone()
                    }
                },
            )
        };
        let set_model = move |h: String, m: String, f: String| {
            move |w: &mut Workflow| {
                if let Some(StepKind::Agent {
                    harness,
                    model,
                    family,
                    ..
                }) = w.steps.get_mut(i).map(|s| &mut s.kind)
                {
                    *harness = h.clone();
                    *model = m.clone();
                    *family = f.clone();
                }
            }
        };
        let mut choices = vec![Choice::new(
            "The runner's default",
            set_model(String::new(), String::new(), String::new()),
        )];
        for (j, f) in self.families.iter().enumerate() {
            let mut c = Choice::new(
                if f.name.is_empty() {
                    f.id.clone()
                } else {
                    f.name.clone()
                },
                set_model(String::new(), String::new(), f.id.clone()),
            );
            if j == 0 {
                c = c.group("Families");
            }
            choices.push(c);
        }
        let first = harnesses()
            .first()
            .map_or("claude_code".to_string(), |h| h.0.clone());
        choices.push(
            Choice::new(
                "A specific harness and model",
                set_model(first, String::new(), String::new()),
            )
            .group("Pinned"),
        );
        let m = self.select(format!("{k}/model"), current, choices, cx);
        out = out.child(row2(
            field(
                &t,
                "Repo",
                Some("owner/name. A computer uses its checkout; tab cloud clones it."),
                r,
            ),
            field(
                &t,
                "Model",
                Some("A family tries its models in order on the runner."),
                m,
            ),
        ));
        if pinned {
            let hs: Vec<Choice> = harnesses()
                .into_iter()
                .map(|(id, name)| {
                    Choice::new(name, move |w: &mut Workflow| {
                        if let Some(StepKind::Agent { harness, .. }) =
                            w.steps.get_mut(i).map(|s| &mut s.kind)
                        {
                            *harness = id.clone();
                        }
                    })
                })
                .collect();
            let h = self.select(
                format!("{k}/harness"),
                if harness.is_empty() {
                    "Pick a harness".into()
                } else {
                    harness_name(harness)
                },
                hs,
                cx,
            );
            let mm = self.input(
                &format!("{k}/model-name"),
                model,
                MONO,
                "The harness's default",
                Commit::Live,
                ag!(model, |m: &mut String, v: String| *m = v),
                window,
                cx,
            );
            out = out.child(row2(
                field(&t, "Harness", None, h),
                field(&t, "Model", Some("Empty uses the harness's default."), mm),
            ));
        } else if let Some(f) = self.families.iter().find(|f| &f.id == family) {
            let list = f
                .candidates
                .iter()
                .map(|c| {
                    if c.model.is_empty() {
                        harness_name(&c.harness)
                    } else {
                        format!("{} {}", harness_name(&c.harness), c.model)
                    }
                })
                .collect::<Vec<_>>()
                .join(", then ");
            out = out.child(note(
                &t,
                format!("Tries {list}. A runner skips harnesses it doesn't have, and moves on when a model is unavailable, rate limited or out of plan. Edit families in Settings → Agents."),
            ));
        }
        let cloud = self
            .runners
            .iter()
            .any(|(r, _, _)| r.kind == RunnerKind::Cloud);
        let place = |p: Placement| {
            move |w: &mut Workflow| {
                if let Some(StepKind::Agent { placement, .. }) =
                    w.steps.get_mut(i).map(|s| &mut s.kind)
                {
                    *placement = p;
                }
            }
        };
        let label = match placement {
            Placement::Computers => "Only my computers",
            Placement::PreferComputer { .. } => "A computer first, then tab cloud",
            Placement::Cloud => "Only tab cloud",
            Placement::Anywhere => "Whichever is free first",
        };
        let sel = self.select(
            format!("{k}/placement"),
            label.into(),
            vec![
                Choice::new("Only my computers", place(Placement::Computers)),
                Choice::new(
                    "A computer first, then tab cloud",
                    place(Placement::PreferComputer { minutes: 15 }),
                ),
                Choice::new("Only tab cloud", place(Placement::Cloud)),
                Choice::new("Whichever is free first", place(Placement::Anywhere)),
            ],
            cx,
        );
        out = out.child(field(
            &t,
            "Where it may run",
            (!cloud)
                .then_some("tab cloud runs agents on paid plans once your hub is in tab cloud."),
            sel,
        ));
        let deadline = self.number(
            &format!("{k}/deadline"),
            *deadline_minutes,
            0,
            10_080,
            ag!(deadline_minutes, |d: &mut u32, v: u32| *d = v),
            window,
            cx,
        );
        let deadline = field(
            &t,
            "Give up after (min)",
            Some("0 is a day. The run fails if no runner finishes by then."),
            deadline,
        );
        if let Placement::PreferComputer { minutes } = placement {
            let wait = self.number(
                &format!("{k}/wait"),
                *minutes,
                1,
                1440,
                move |w, v| {
                    if let Some(StepKind::Agent {
                        placement: Placement::PreferComputer { minutes },
                        ..
                    }) = w.steps.get_mut(i).map(|s| &mut s.kind)
                    {
                        *minutes = v;
                    }
                },
                window,
                cx,
            );
            out = out.child(row2(
                field(&t, "Wait for a computer (min)", None, wait),
                deadline,
            ));
        } else {
            out = out.child(deadline);
        }
        // Who could take the job right now.
        let allowed = |kind: RunnerKind| match placement {
            Placement::Anywhere | Placement::PreferComputer { .. } => true,
            Placement::Cloud => kind == RunnerKind::Cloud,
            Placement::Computers => kind == RunnerKind::Computer,
        };
        let shown: Vec<_> = self
            .runners
            .iter()
            .filter(|(r, _, _)| allowed(r.kind))
            .cloned()
            .collect();
        if shown.is_empty() {
            out = out.child(note(
                &t,
                format!(
                    "No runner can take it yet. {}",
                    if matches!(placement, Placement::Cloud) {
                        "Move your hub to tab cloud in Settings → Hub."
                    } else {
                        "Open tab on a computer."
                    }
                ),
            ));
        } else {
            out = out.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(5.))
                    .children(shown.into_iter().map(|(r, awake, me)| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .px(px(8.))
                            .py(px(3.))
                            .rounded_full()
                            .border_1()
                            .border_color(t.border)
                            .text_size(px(size::TEXT_SM))
                            .text_color(t.fg_muted)
                            .child(div().size(px(6.)).rounded_full().bg(if awake {
                                t.success.fg
                            } else {
                                t.border_strong
                            }))
                            .child(format!(
                                "{}{}",
                                if me {
                                    "This computer".to_string()
                                } else if r.name.is_empty() {
                                    r.id.clone()
                                } else {
                                    r.name.clone()
                                },
                                if awake { "" } else { " · asleep" }
                            ))
                    })),
            );
        }
        out = out.child(outputs(
            &t,
            ["summary", "branch", "pr_url"]
                .iter()
                .map(|o| format!("steps.{}.{o}", step.id))
                .collect(),
        ));
        out.into_any_element()
    }

    fn command_fields(
        &mut self,
        step: &Step,
        i: usize,
        vars: &[(String, String)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let StepKind::Command {
            program,
            args,
            folder,
            network,
            env,
            image,
            timeout_secs,
        } = &step.kind
        else {
            return div().into_any_element();
        };
        let k = format!("s{i}");
        macro_rules! cmd {
            ($x:ident, $body:expr) => {
                move |w: &mut Workflow, v| {
                    if let Some(StepKind::Command { $x, .. }) =
                        w.steps.get_mut(i).map(|s| &mut s.kind)
                    {
                        #[allow(clippy::redundant_closure_call)]
                        ($body)($x, v);
                    }
                }
            };
        }
        let lines = |v: String| -> Vec<String> { v.split('\n').map(str::to_string).collect() };
        let mut out = div().flex().flex_col().gap(px(12.));
        let p = self.input(
            &format!("{k}/program"),
            program,
            MONO,
            "cargo",
            Commit::Live,
            cmd!(program, |p: &mut String, v: String| *p = v),
            window,
            cx,
        );
        out = out.child(field(
            &t,
            "Program",
            Some("Run directly, never through a shell. A name on PATH, or a path."),
            p,
        ));
        let a = self.input(
            &format!("{k}/args"),
            &args.join("\n"),
            AREA_MONO,
            "test\n--workspace",
            Commit::Live,
            cmd!(args, move |a: &mut Vec<String>, v: String| *a = lines(v)),
            window,
            cx,
        );
        out = out.child(field(
            &t,
            "Arguments",
            Some("One per line. Templates like {{item.metadata.repo}} are passed as plain text."),
            a,
        ));
        let f = self.input(
            &format!("{k}/folder"),
            folder,
            MONO,
            "A fresh folder per run",
            Commit::Live,
            cmd!(folder, |f: &mut String, v: String| *f = v),
            window,
            cx,
        );
        out = out.child(field(
            &t,
            "Folder",
            Some("Where it runs and the only place it can change. Empty gives each run a fresh folder, removed afterwards."),
            f,
        ));
        let n = self.input(
            &format!("{k}/network"),
            &network.join("\n"),
            AREA_MONO,
            "crates.io\n*.crates.io",
            Commit::Live,
            cmd!(network, move |n: &mut Vec<String>, v: String| *n = lines(v)),
            window,
            cx,
        );
        out = out.child(field(
            &t,
            "Network",
            Some("Hosts it may reach, one per line. Empty means none; * means any."),
            n,
        ));
        let img = self.input(
            &format!("{k}/image"),
            image,
            MONO,
            "rust:1",
            Commit::Live,
            cmd!(image, |m: &mut String, v: String| *m = v.trim().to_string()),
            window,
            cx,
        );
        let secs = self.number(
            &format!("{k}/timeout"),
            *timeout_secs,
            0,
            3600,
            cmd!(timeout_secs, |s: &mut u32, v: u32| *s = v),
            window,
            cx,
        );
        out = out.child(row2(
            field(
                &t,
                "Container image",
                Some("Optional. Runs in Docker or Podman instead, with only the folder mounted."),
                img,
            ),
            field(
                &t,
                "Time limit (s)",
                Some("0 is 10 minutes; at most an hour."),
                secs,
            ),
        ));
        let at = lens(
            move |x: &mut Workflow| match x.steps.get_mut(i).map(|s| &mut s.kind) {
                Some(StepKind::Command { env, .. }) => Some(env),
                _ => None,
            },
        );
        let e = self.pairs(&format!("{k}/env"), env, vars, "Name", at, window, cx);
        out = out.child(field(
            &t,
            "Environment",
            Some("Only these, plus PATH and the like. None of tab's tokens are passed on."),
            e,
        ));
        out = out.child(outputs(
            &t,
            ["exit_code", "stdout", "stderr"]
                .iter()
                .map(|o| format!("steps.{}.{o}", step.id))
                .collect(),
        ));
        out.into_any_element()
    }
}

fn heading(t: &crate::theme::Theme, s: &str) -> AnyElement {
    div()
        .text_size(px(size::TEXT_LG))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(t.fg)
        .child(s.to_string())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chips_toggle_values() {
        let mut v = vec![1, 2];
        toggle(&mut v, &2);
        assert_eq!(v, vec![1]);
        toggle(&mut v, &3);
        assert_eq!(v, vec![1, 3]);
    }
}
