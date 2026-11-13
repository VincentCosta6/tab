//! Runs: one workflow's in its panel, and every workflow's on the All runs
//! page, each opening to its steps' inputs, outputs and errors, what's
//! waiting on the person, and the agents it handed work to. The
//! counterparts of `RunsPanel`, `RunDetail`, `AllRuns` and `RunExtras`.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};
use gpui_component::Sizable;
use gpui_component::button::Button as GButton;
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use tab_core::workflow::{Issue, Run, RunStatus, Workflow};

use super::format::{plural, run_label, run_tone, step_label, step_tone};
use super::{OpenItem, Panel, RunFilter, WorkflowsTab, issue_line};
use crate::components::{Button, ButtonKind, ago, badge, segmented};
use crate::theme::{FONT_MONO, Theme, size, theme};

fn json_block(t: &Theme, label: &str, v: &serde_json::Value) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .text_size(px(size::TEXT_XS))
                .text_color(t.fg_subtle)
                .child(label.to_string()),
        )
        .child(
            div()
                .p(px(8.))
                .rounded(px(size::RADIUS_SM))
                .bg(t.bg_sunken)
                .font_family(FONT_MONO)
                .text_size(px(size::TEXT_XS))
                .text_color(t.fg_muted)
                .child(serde_json::to_string_pretty(v).unwrap_or_default()),
        )
        .into_any_element()
}

/// Whether a run passes the All runs filter.
fn shown(r: &Run, filter: RunFilter, workflow: Option<&str>) -> bool {
    if workflow.is_some_and(|w| r.workflow_id != w) {
        return false;
    }
    match filter {
        RunFilter::Dry => r.dry_run,
        _ if r.dry_run => false,
        RunFilter::Problems => !matches!(
            r.status,
            RunStatus::Succeeded | RunStatus::Stopped | RunStatus::Running
        ),
        RunFilter::Succeeded => r.status == RunStatus::Succeeded,
        RunFilter::All => true,
    }
}

impl WorkflowsTab {
    fn run_row(&mut self, r: &Run, all: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let open = self.open_run.as_deref() == Some(&r.id);
        let tone = run_tone(&t, r.status);
        let now = chrono::Utc::now();
        let id = r.id.clone();
        let head = div()
            .id(SharedString::from(format!("wf-run-{}", r.id)))
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .cursor_pointer()
            .hover(|s| s.bg(t.bg_hover))
            .child(badge(
                if r.dry_run && all {
                    "Dry run"
                } else {
                    run_label(r.status)
                },
                tone.fg,
                tone.bg,
            ))
            .when(all, |el| {
                el.child(
                    div()
                        .flex_none()
                        .max_w(px(220.))
                        .truncate()
                        .text_size(px(size::TEXT_MD))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(r.workflow_name.clone()),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_muted)
                    .child(r.trigger.clone()),
            )
            .when(all, |el| {
                el.child(
                    div()
                        .text_size(px(size::TEXT_SM))
                        .text_color(t.fg_subtle)
                        .child(plural(r.steps.len() as u32, "step")),
                )
            })
            .child(
                div()
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_subtle)
                    .child(ago(r.started_at, now)),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_run = if this.open_run.as_deref() == Some(&id) {
                    None
                } else {
                    Some(id.clone())
                };
                cx.notify();
            }));
        let detail = open.then(|| self.run_detail(r, all, cx));
        div()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(t.border)
            .when(open, |el| el.bg(t.bg_raised))
            .child(head)
            .children(detail)
            .into_any_element()
    }

    fn run_detail(&mut self, r: &Run, all: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        if !r.dry_run
            && !self.run_views.contains_key(&r.id)
            && let Ok(Some(v)) = crate::backend::backend(cx).workflow_run_view(&r.id)
        {
            self.run_views.insert(r.id.clone(), v);
        }
        let view = self.run_views.get(&r.id).cloned();
        let mut body = div().flex().flex_col().gap(px(8.)).px(px(12.)).pb(px(12.));
        if let Some(v) = &view {
            if let Some(a) = &v.approval {
                let (id1, id2) = (r.id.clone(), r.id.clone());
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(10.))
                        .rounded(px(size::RADIUS_MD))
                        .bg(t.warning.bg)
                        .text_color(t.warning.fg)
                        .child(
                            div()
                                .text_size(px(size::TEXT_MD))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(format!("Needs your OK: {}", a.label)),
                        )
                        .when_some(a.reason.clone(), |el, why| {
                            el.child(div().text_size(px(size::TEXT_SM)).child(why))
                        })
                        .when(!a.input.is_null(), |el| {
                            el.child(json_block(&t, "It would send", &a.input))
                        })
                        .child(
                            div()
                                .flex()
                                .gap(px(6.))
                                .child(
                                    Button::new(SharedString::from(format!("wf-approve-{}", r.id)))
                                        .kind(ButtonKind::Primary)
                                        .label("Approve")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.answer(id1.clone(), true, cx)
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("wf-decline-{}", r.id)))
                                        .label("Decline")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.answer(id2.clone(), false, cx)
                                        })),
                                ),
                        ),
                );
            }
            if let Some(s) = &v.started_by {
                body = body.child(line(&t, format!("Started from the chat \"{}\"", s.title)));
            }
            if let Some(c) = &v.caused_by {
                body = body.child(line(&t, format!("Started by a run of {}", c.workflow_name)));
            }
            for a in &v.agents {
                let status = match (a.needs_you, a.status) {
                    (true, _) => "needs you".to_string(),
                    (_, Some(s)) => format!("{s:?}").to_lowercase(),
                    _ => "waiting for a runner".into(),
                };
                body = body.child(line(&t, format!("Agent on {}: {status}", a.step_id)));
            }
        }
        if let Some(e) = &r.error {
            body = body.child(issue_line(
                &Issue {
                    at: None,
                    error: r.status == RunStatus::Failed,
                    message: e.clone(),
                },
                &t,
            ));
        }
        if !r.chain.is_empty() {
            body = body.child(line(
                &t,
                format!(
                    "Started by changes from {} earlier workflow{}.",
                    r.chain.len(),
                    if r.chain.len() == 1 { "" } else { "s" }
                ),
            ));
        }
        for s in &r.steps {
            let tone = step_tone(&t, s.status);
            let has_out = s.output.as_object().is_some_and(|o| !o.is_empty())
                || (!s.output.is_null() && !s.output.is_object());
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(8.))
                    .rounded(px(size::RADIUS_MD))
                    .border_1()
                    .border_color(t.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(badge(step_label(s.status), tone.fg, tone.bg))
                            .child(
                                div()
                                    .flex_1()
                                    .font_family(FONT_MONO)
                                    .text_size(px(size::TEXT_SM))
                                    .child(s.id.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(size::TEXT_SM))
                                    .text_color(t.fg_subtle)
                                    .child(format!("{} ms", s.ms)),
                            ),
                    )
                    .when_some(s.error.clone(), |el, e| {
                        el.child(
                            div()
                                .text_size(px(size::TEXT_SM))
                                .text_color(t.danger.fg)
                                .child(e),
                        )
                    })
                    .when(!s.input.is_null(), |el| {
                        el.child(json_block(&t, "Input", &s.input))
                    })
                    .when(has_out, |el| el.child(json_block(&t, "Output", &s.output))),
            );
        }
        let mut actions = div().flex().gap(px(6.));
        if let Some(item) = r.item_id.clone() {
            actions = actions.child(
                Button::new(SharedString::from(format!("wf-open-item-{}", r.id)))
                    .icon("external")
                    .label("Open the item and its timeline")
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(OpenItem(item.clone())))),
            );
        }
        if all
            && let Some(w) = self
                .workflows
                .iter()
                .find(|w| w.id == r.workflow_id)
                .cloned()
        {
            actions = actions.child(
                Button::new(SharedString::from(format!("wf-open-wf-{}", r.id)))
                    .icon("workflow")
                    .label("Open the workflow")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open(w.clone(), Panel::Runs, window, cx)
                    })),
            );
        }
        body.child(actions).into_any_element()
    }

    pub(super) fn runs_panel(&mut self, w: &Workflow, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        if w.id.is_empty() {
            return pad(
                &t,
                "Save the workflow to start keeping its runs. Dry runs show on the canvas.",
            );
        }
        if self.runs.is_empty() {
            return pad(
                &t,
                "No runs yet. Each run is kept with every step's inputs and outputs for 30 days.",
            );
        }
        let runs = self.runs.clone();
        div()
            .flex()
            .flex_col()
            .children(
                runs.iter()
                    .map(|r| self.run_row(r, false, cx))
                    .collect::<Vec<_>>(),
            )
            .into_any_element()
    }

    pub(super) fn all_runs_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let this = cx.entity().downgrade();
        let filter = segmented(
            "wf-runs-filter",
            &[
                (RunFilter::All, "All"),
                (RunFilter::Problems, "Problems"),
                (RunFilter::Succeeded, "Succeeded"),
                (RunFilter::Dry, "Dry runs"),
            ],
            self.run_filter,
            cx,
            move |f, _, cx| {
                this.update(cx, |v, cx| {
                    v.run_filter = f;
                    cx.notify();
                })
                .ok();
            },
        );
        let picked = self
            .run_workflow
            .as_ref()
            .and_then(|id| self.workflows.iter().find(|w| &w.id == id))
            .map_or("Every workflow".to_string(), |w| w.name.clone());
        let names: Vec<(String, String)> = self
            .workflows
            .iter()
            .map(|w| (w.id.clone(), w.name.clone()))
            .collect();
        let this = cx.entity().downgrade();
        let pick = GButton::new("wf-runs-workflow")
            .outline()
            .small()
            .label(picked)
            .dropdown_menu(move |mut menu, _, _| {
                let every = this.clone();
                menu = menu.item(
                    PopupMenuItem::new("Every workflow").on_click(move |_, _, cx| {
                        every
                            .update(cx, |v, cx| {
                                v.run_workflow = None;
                                cx.notify();
                            })
                            .ok();
                    }),
                );
                for (id, name) in &names {
                    let (this, id) = (this.clone(), id.clone());
                    menu = menu.item(PopupMenuItem::new(name.clone()).on_click(move |_, _, cx| {
                        let id = id.clone();
                        this.update(cx, |v, cx| {
                            v.run_workflow = Some(id);
                            cx.notify();
                        })
                        .ok();
                    }));
                }
                menu
            });
        let runs: Vec<Run> = self
            .all_runs
            .iter()
            .filter(|r| shown(r, self.run_filter, self.run_workflow.as_deref()))
            .cloned()
            .collect();
        let empty = if self.all_runs.is_empty() {
            "No runs yet. When a workflow runs, each run shows here with every step's inputs and outputs, kept for 30 days."
        } else {
            "No runs match."
        };
        let rows: Vec<AnyElement> = runs.iter().map(|r| self.run_row(r, true, cx)).collect();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(16.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(t.border)
                    .bg(t.bg_raised)
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(size::TEXT_LG))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("All runs"),
                    )
                    .child(filter)
                    .child(pick),
            )
            .child(
                div()
                    .id("wf-allruns-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(rows.is_empty(), |el| el.child(pad(&t, empty)))
                    .children(rows),
            )
            .into_any_element()
    }
}

fn line(t: &Theme, s: String) -> AnyElement {
    div()
        .text_size(px(size::TEXT_SM))
        .text_color(t.fg_muted)
        .child(s)
        .into_any_element()
}

fn pad(t: &Theme, s: &str) -> AnyElement {
    div()
        .p(px(16.))
        .text_size(px(size::TEXT_MD))
        .text_color(t.fg_subtle)
        .child(s.to_string())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(status: RunStatus, dry: bool) -> Run {
        Run {
            id: "r".into(),
            workflow_id: "w".into(),
            workflow_name: "W".into(),
            version: 1,
            status,
            dry_run: dry,
            trigger: "by hand".into(),
            trigger_key: None,
            item_id: None,
            chain: Vec::new(),
            steps: Vec::new(),
            error: None,
            started_at: chrono::Utc::now(),
            finished_at: None,
            runner: String::new(),
            resume: None,
            rerun_of: None,
            started_by: None,
            caused_by: None,
            approved: Vec::new(),
        }
    }

    #[test]
    fn the_filters_pick_the_right_runs() {
        let failed = run(RunStatus::Failed, false);
        let ok = run(RunStatus::Succeeded, false);
        let dry = run(RunStatus::Succeeded, true);
        assert!(shown(&failed, RunFilter::Problems, None));
        assert!(!shown(&ok, RunFilter::Problems, None));
        assert!(shown(&ok, RunFilter::Succeeded, None));
        assert!(!shown(&dry, RunFilter::All, None));
        assert!(shown(&dry, RunFilter::Dry, None));
        assert!(!shown(&ok, RunFilter::All, Some("other")));
    }
}
