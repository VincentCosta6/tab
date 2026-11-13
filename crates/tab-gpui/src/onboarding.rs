//! First launch: what tab is, picking an agent this computer has and where
//! sessions start, then go. The counterpart of
//! `src/modules/onboarding/Onboarding.tsx`.
//!
//! The Tauri app's hub step (tab cloud sign-in), installing an agent and the
//! GitHub sign-in card aren't here yet: the hub stays on this computer, and
//! GitHub is set up in Connections.

use gpui::{
    AppContext, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use tab_core::TabConfig;
use tab_core::agents::AgentProbe;
use tab_core::model::AgentKind;

use crate::backend::backend;
use crate::chat::agent_name;
use crate::components::{Button, ButtonKind, icon, tab_mark};
use crate::theme::{size, theme};

const STEPS: [&str; 3] = ["Welcome", "Agent", "Ready"];

/// Emitted once the person finishes.
pub struct Finished;

pub struct Onboarding {
    step: usize,
    draft: TabConfig,
    probes: Option<Vec<AgentProbe>>,
    workdir: Entity<InputState>,
    error: Option<String>,
    busy: bool,
    _sub: gpui::Subscription,
}

impl EventEmitter<Finished> for Onboarding {}

/// Whether this config still needs onboarding.
pub fn needed(cfg: &TabConfig) -> bool {
    !cfg.onboarded
}

impl Onboarding {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let b = backend(cx);
        let draft = crate::runtime::handle().block_on(b.config_masked());
        let value = draft.agent.workdir.clone().unwrap_or_default();
        let workdir = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("~/code")
                .default_value(value)
        });
        let sub = cx.subscribe(&workdir, |this: &mut Self, s, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                let v = s.read(cx).value().trim().to_string();
                this.draft.agent.workdir = (!v.is_empty()).then_some(v);
                cx.notify();
            }
        });
        let mut this = Onboarding {
            step: 0,
            draft,
            probes: None,
            workdir,
            error: None,
            busy: false,
            _sub: sub,
        };
        this.detect(cx);
        this
    }

    /// Looks for installed agents, and picks the first found if none is set.
    fn detect(&mut self, cx: &mut Context<Self>) {
        let b = backend(cx);
        let task = crate::runtime::spawn(async move { b.agents.detect().await });
        cx.spawn(async move |this, cx| {
            if let Ok(probes) = task.await {
                this.update(cx, |this, cx| {
                    if this.draft.agent.kind.is_none()
                        && let Some(p) = probes.iter().find(|p| p.found)
                    {
                        this.choose(p.kind);
                    }
                    this.probes = Some(probes);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn choose(&mut self, kind: AgentKind) {
        self.draft.agent.kind = Some(kind);
        self.draft.agent.model = self.draft.agent.models.get(kind.as_str()).cloned();
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.error = None;
        let mut cfg = self.draft.clone();
        cfg.onboarded = true;
        let b = backend(cx);
        let task = crate::runtime::spawn(async move { b.config_save(cfg).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.and_then(|r| r);
            this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(_) => cx.emit(Finished),
                    Err(e) => this.error = Some(format!("Couldn't save: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_agents(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let mut col = div().flex().flex_col().gap(px(8.));
        let Some(probes) = &self.probes else {
            return col.child(
                div()
                    .text_size(px(size::TEXT_MD))
                    .text_color(t.fg_muted)
                    .child("Looking for agents on this computer…"),
            );
        };
        for p in probes {
            let chosen = self.draft.agent.kind == Some(p.kind);
            let kind = p.kind;
            let detail = match (&p.found, &p.version, &p.error) {
                (true, Some(v), _) => format!("Found · {v}"),
                (true, None, _) => "Found".to_string(),
                (false, _, Some(e)) => e.clone(),
                (false, _, None) => "Not found on this computer".to_string(),
            };
            col = col.child(
                div()
                    .id(SharedString::from(format!("agent-{}", kind.as_str())))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(px(size::RADIUS_LG))
                    .border_1()
                    .border_color(if chosen { t.accent.fg } else { t.border })
                    .bg(if chosen { t.accent.bg } else { t.bg_raised })
                    .cursor_pointer()
                    .child(icon("bot", 18., if p.found { t.fg } else { t.fg_subtle }))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(size::TEXT_MD))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(agent_name(kind)),
                            )
                            .child(
                                div()
                                    .text_size(px(size::TEXT_SM))
                                    .text_color(if p.found { t.success.fg } else { t.fg_subtle })
                                    .child(detail),
                            ),
                    )
                    .when(chosen, |el| el.child(icon("check", 16., t.accent.fg)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose(kind);
                        cx.notify();
                    })),
            );
        }
        col.child(
            div().flex().child(
                Button::new("look-again")
                    .kind(ButtonKind::Ghost)
                    .icon("refresh")
                    .label("Look again")
                    .on_click(cx.listener(|this, _, _, cx| this.detect(cx))),
            ),
        )
    }
}

impl Render for Onboarding {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let mut steps = div().flex().gap(px(16.));
        for (i, s) in STEPS.iter().enumerate() {
            let state = if i < self.step {
                t.success.fg
            } else if i == self.step {
                t.fg
            } else {
                t.fg_subtle
            };
            steps = steps.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(size::TEXT_SM))
                    .text_color(state)
                    .child(if i < self.step {
                        icon("check", 12., state).into_any_element()
                    } else {
                        div().child((i + 1).to_string()).into_any_element()
                    })
                    .child(*s),
            );
        }
        let body = match self.step {
            0 => div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .py(px(24.))
                .child(tab_mark(56.))
                .child(
                    div()
                        .text_size(px(28.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("tab"),
                )
                .child(
                    div()
                        .text_size(px(size::TEXT_LG))
                        .child("TAB Annihilates Backlogs."),
                )
                .child(
                    div()
                        .max_w(px(460.))
                        .text_size(px(size::TEXT_MD))
                        .text_color(t.fg_muted)
                        .child("One queue for everything that needs you, ranked. Open anything and hand it to an agent: it works in a chat right next to the item, with the context rolled up for it."),
                )
                .into_any_element(),
            1 => div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(
                    div()
                        .text_size(px(size::TEXT_XL))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Set up your agent"),
                )
                .child(
                    div()
                        .text_size(px(size::TEXT_MD))
                        .text_color(t.fg_muted)
                        .child("tab runs a coding agent on this computer and streams it into the app. Pick one; you can switch later in Settings."),
                )
                .child(self.render_agents(cx))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(size::TEXT_MD))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child("Work folder"),
                        )
                        .child(
                            div()
                                .text_size(px(size::TEXT_SM))
                                .text_color(t.fg_subtle)
                                .child("Sessions start here, usually where your repos live."),
                        )
                        .child(Input::new(&self.workdir))
                        .when_some(self.draft.agent.workdir_problem(), |el, p| {
                            el.child(
                                div()
                                    .text_size(px(size::TEXT_SM))
                                    .text_color(t.danger.fg)
                                    .child(p),
                            )
                        }),
                )
                .into_any_element(),
            _ => div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .text_size(px(size::TEXT_XL))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("You're set"),
                )
                .child(
                    div()
                        .text_size(px(size::TEXT_MD))
                        .text_color(t.fg_muted)
                        .child(match self.draft.agent.kind {
                            Some(k) => format!(
                                "Chats run on {}. Connect GitHub, Slack or email in Connections to fill your queue; the sample data shows how it looks meanwhile.",
                                agent_name(k)
                            ),
                            None => "No agent yet: the queue works without one, and you can pick one in Settings → Agents.".into(),
                        }),
                )
                .into_any_element(),
        };
        let last = self.step == STEPS.len() - 1;
        let blocked = self.step == 1 && self.draft.agent.workdir_problem().is_some();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(t.bg_sunken)
            .font_family(crate::theme::FONT_SANS)
            .text_color(t.fg)
            .child(
                div()
                    .w(px(620.))
                    .flex()
                    .flex_col()
                    .gap(px(20.))
                    .p(px(28.))
                    .rounded(px(size::RADIUS_LG))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.bg_raised)
                    .shadow_lg()
                    .child(steps)
                    .child(body)
                    .when_some(self.error.clone(), |el, e| {
                        el.child(div().text_color(t.danger.fg).child(e))
                    })
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(if self.step > 0 {
                                Button::new("back")
                                    .kind(ButtonKind::Ghost)
                                    .label("Back")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.step -= 1;
                                        cx.notify();
                                    }))
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            })
                            .child(
                                Button::new("next")
                                    .kind(ButtonKind::Primary)
                                    .label(if last {
                                        if self.busy { "Saving…" } else { "Start" }
                                    } else {
                                        "Continue"
                                    })
                                    .disabled(self.busy || blocked)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if last {
                                            this.finish(cx);
                                        } else {
                                            this.step += 1;
                                            cx.notify();
                                        }
                                    })),
                            ),
                    ),
            )
    }
}
