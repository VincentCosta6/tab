//! An item's chat with its agent: the transcript, the reply as it streams,
//! approval prompts, and the composer. The counterpart of `ChatView` in
//! `src/modules/work/Chat.tsx`.

pub mod model;

use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::text::TextView;
use tab_app::chat::Chat;
use tab_core::EngineEvent;
use tab_core::model::{Approval, Project, QueueItem};

use self::model::{Row, rows, status_label, suggestions};
use crate::backend::backend;
use crate::components::{Button, ButtonKind, icon};
use crate::theme::{FONT_MONO, size, theme};

/// Whose chat this is.
#[derive(Clone)]
pub enum Target {
    /// A task's chat.
    Item(QueueItem),
    /// A project's coordinator.
    Coordinator(Project),
    /// The workflow builder's assistant, with the workflow as it's open
    /// (unsaved edits included).
    Workflow(Box<tab_core::workflow::Workflow>),
}

pub struct ChatView {
    target: Target,
    chat: Option<Chat>,
    /// The agent's reply so far, before it's stored as a message.
    streaming: String,
    error: Option<String>,
    composer: Entity<TextareaState>,
    scroll: ScrollHandle,
    /// Tool rows the person opened, by their place in the transcript.
    open_tools: std::collections::HashSet<usize>,
    _subs: Vec<Subscription>,
}

impl ChatView {
    pub fn new(item: QueueItem, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::of(Target::Item(item), window, cx)
    }

    pub fn of(target: Target, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = match &target {
            Target::Item(_) => "Ask about this, or tell the agent what to do…",
            Target::Coordinator(_) => "Ask about the project, or tell the coordinator what to do…",
            Target::Workflow(w) if !w.steps.is_empty() => "Ask for a change to this workflow…",
            Target::Workflow(_) => "Describe a workflow…",
        };
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .submit_on_enter(true)
                .placeholder(placeholder)
        });
        let sent = cx.subscribe_in(
            &composer,
            window,
            |this, input, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    let text = input.read(cx).value().to_string();
                    if !text.trim().is_empty() {
                        input.update(cx, |i, cx| i.set_value("", window, cx));
                        this.send(text, cx);
                    }
                }
            },
        );
        let events = crate::events::events(cx);
        let live = cx.subscribe(&events, |this, _, ev: &Arc<EngineEvent>, cx| {
            this.on_event(ev, cx)
        });
        let mut v = ChatView {
            target,
            chat: None,
            streaming: String::new(),
            error: None,
            composer,
            scroll: ScrollHandle::new(),
            open_tools: Default::default(),
            _subs: vec![sent, live],
        };
        v.load(cx);
        v
    }

    fn suggestions(&self) -> Vec<String> {
        match &self.target {
            Target::Item(item) => suggestions(item).into_iter().map(String::from).collect(),
            Target::Coordinator(p) => vec![
                "What needs me in this project?".into(),
                "What changed this week?".into(),
                format!("Summarize where {} stands", p.name),
            ],
            Target::Workflow(w) if !w.steps.is_empty() => vec![
                "Dry-run this on a real item and tell me what it would do".into(),
                "Make this only run for PRs in repos I own".into(),
                "Add a step that labels the PR too".into(),
            ],
            Target::Workflow(_) => vec![
                "When my review is requested on a small PR, add an urgent task".into(),
                "Snooze Dependabot PRs until Friday morning".into(),
                "When CI fails on my PR, comment with the failing job and make a task".into(),
            ],
        }
    }

    /// Points the chat at a newer copy of what it's about, such as the
    /// workflow with the person's latest edits, without reloading it.
    pub fn retarget(&mut self, target: Target) {
        self.target = target;
    }

    fn session_id(&self) -> Option<&str> {
        self.chat.as_ref()?.session.as_ref().map(|s| s.id.as_str())
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let (b, target) = (backend(cx), self.target.clone());
        let task = crate::runtime::spawn(async move {
            match target {
                Target::Item(i) => b.chat_open(&i.id),
                Target::Coordinator(p) => b.coordinator_open(&p.id),
                Target::Workflow(w) => b.workflow_chat_open(&w.id),
            }
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok(chat)) => {
                        this.chat = Some(chat);
                        this.scroll.scroll_to_bottom();
                    }
                    Ok(Err(e)) | Err(e) => this.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_event(&mut self, ev: &EngineEvent, cx: &mut Context<Self>) {
        let mine = |sid: &str| self.session_id() == Some(sid);
        match ev {
            EngineEvent::SessionMessage { message } if mine(&message.session_id) => {
                if message.kind == "text" && message.role != "user" {
                    self.streaming.clear();
                }
                if let Some(chat) = &mut self.chat
                    && !chat.messages.iter().any(|m| m.id == message.id)
                {
                    // The stored copy replaces the one shown while sending.
                    chat.messages.retain(|m| m.id > 0 || message.role != "user");
                    chat.messages.push(message.clone());
                }
                self.scroll.scroll_to_bottom();
                cx.notify();
            }
            EngineEvent::SessionDelta { session_id, text } if mine(session_id) => {
                self.streaming.push_str(text);
                self.scroll.scroll_to_bottom();
                cx.notify();
            }
            EngineEvent::SessionChanged { session }
                if match &self.target {
                    Target::Item(i) => session.item_id.as_deref() == Some(&i.id),
                    Target::Coordinator(_) | Target::Workflow(_) => mine(&session.id),
                } =>
            {
                // A new session (first message, /clear, /resume) reloads.
                if self.session_id() != Some(&session.id) {
                    self.load(cx);
                } else if let Some(chat) = &mut self.chat {
                    chat.running =
                        matches!(session.status, tab_core::model::SessionStatus::Working);
                    chat.session = Some(session.clone());
                    if !chat.running {
                        self.streaming.clear();
                    }
                    cx.notify();
                }
            }
            EngineEvent::ApprovalRequested { approval } if mine(&approval.session_id) => {
                if let Some(chat) = &mut self.chat {
                    chat.approvals.retain(|a| a.id != approval.id);
                    chat.approvals.push(approval.clone());
                }
                self.scroll.scroll_to_bottom();
                cx.notify();
            }
            EngineEvent::ApprovalResolved { session_id, id } if mine(session_id) => {
                if let Some(chat) = &mut self.chat {
                    chat.approvals.retain(|a| &a.id != id);
                }
                cx.notify();
            }
            _ => {}
        }
    }

    fn send(&mut self, text: String, cx: &mut Context<Self>) {
        self.error = None;
        // Show it at once; the stored copy replaces it.
        if let Some(chat) = &mut self.chat {
            chat.running = true;
            chat.messages.push(tab_core::model::SessionMessage {
                id: -chrono::Utc::now().timestamp_millis(),
                session_id: String::new(),
                role: "user".into(),
                kind: "text".into(),
                content: text.clone(),
                meta: serde_json::Value::Null,
                created_at: chrono::Utc::now(),
            });
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
        let (b, target) = (backend(cx), self.target.clone());
        let task = crate::runtime::spawn(async move {
            match target {
                Target::Item(i) => b
                    .chat_send(Some(i.id), &text, None, Vec::new())
                    .await
                    .map(|s| s.session),
                Target::Coordinator(p) => b.coordinator_send(&p.id, &text, &[]),
                Target::Workflow(w) => {
                    let open = (!w.steps.is_empty() || !w.id.is_empty()).then_some(&*w);
                    b.workflow_chat_send(&text, open)
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok(session)) => {
                        let new = this.session_id() != Some(&session.id);
                        if let Some(chat) = &mut this.chat {
                            chat.session = Some(session);
                        }
                        if new {
                            this.load(cx);
                        }
                    }
                    Ok(Err(e)) | Err(e) => {
                        this.error = Some(e);
                        this.load(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(sid) = self.session_id().map(str::to_string) else {
            return;
        };
        let b = backend(cx);
        drop(crate::runtime::spawn(
            async move { b.chat_stop(&sid).await },
        ));
    }

    fn answer(&mut self, approval: &Approval, decision: &'static str, cx: &mut Context<Self>) {
        if let Some(chat) = &mut self.chat {
            chat.approvals.retain(|a| a.id != approval.id);
        }
        cx.notify();
        let (b, id) = (backend(cx), approval.id.clone());
        let task = crate::runtime::spawn(async move {
            b.approval_resolve(id, decision.into(), None, None).await
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Err(e)) | Err(e) = task.await.map(|r| r.map(|_| ())) {
                this.update(cx, |this, cx| {
                    this.error = Some(e);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn render_row(&self, i: usize, row: &Row, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme(cx).clone();
        match row {
            Row::User { text } => div()
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w(px(560.))
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(size::RADIUS_LG))
                        .bg(t.bg_sunken)
                        .text_size(px(size::TEXT_MD))
                        .text_color(t.fg)
                        .child(text.clone()),
                )
                .into_any_element(),
            Row::Assistant { text } => div()
                .text_size(px(size::TEXT_MD))
                .text_color(t.fg)
                .child(
                    TextView::markdown(("msg", i), SharedString::from(text.clone()))
                        .selectable(true),
                )
                .into_any_element(),
            Row::Tool {
                line,
                output,
                failed,
                tab,
            } => {
                let open = self.open_tools.contains(&i);
                let color = if *failed { t.danger.fg } else { t.fg_muted };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .id(("tool", i))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(size::TEXT_SM))
                            .text_color(color)
                            .cursor_pointer()
                            .hover(|s| s.text_color(t.fg))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.open_tools.remove(&i) {
                                    this.open_tools.insert(i);
                                }
                                cx.notify();
                            }))
                            .child(icon(if *tab { "graph" } else { "terminal" }, 12., color))
                            .child(div().truncate().child(line.clone()))
                            .when(*failed, |el| {
                                el.child(div().text_color(t.danger.fg).child("failed"))
                            }),
                    )
                    .when(open, |el| {
                        el.child(
                            div()
                                .id(("tool-out", i))
                                .max_h(px(240.))
                                .overflow_y_scroll()
                                .p(px(8.))
                                .rounded(px(size::RADIUS_MD))
                                .bg(t.bg_sunken)
                                .font_family(FONT_MONO)
                                .text_size(px(size::TEXT_XS))
                                .text_color(t.fg_muted)
                                .child(output.clone().unwrap_or_else(|| "No output".into())),
                        )
                    })
                    .into_any_element()
            }
            Row::Note { text } => div()
                .text_size(px(size::TEXT_XS))
                .text_color(t.fg_subtle)
                .child(text.clone())
                .into_any_element(),
            Row::Error { text } => div()
                .flex()
                .gap(px(6.))
                .p(px(8.))
                .rounded(px(size::RADIUS_MD))
                .bg(t.danger.bg)
                .text_color(t.danger.fg)
                .text_size(px(size::TEXT_SM))
                .child(icon("x-circle", 14., t.danger.fg))
                .child(text.clone())
                .into_any_element(),
        }
    }

    fn render_approval(&self, a: &Approval, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let (allow, always, deny) = (a.clone(), a.clone(), a.clone());
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(size::RADIUS_LG))
            .border_1()
            .border_color(t.warning.fg)
            .bg(t.warning.bg)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(size::TEXT_MD))
                    .text_color(t.warning.fg)
                    .child(icon("shield", 14., t.warning.fg))
                    .child("The agent wants to run"),
            )
            .child(
                div()
                    .p(px(8.))
                    .rounded(px(size::RADIUS_MD))
                    .bg(t.bg_raised)
                    .font_family(FONT_MONO)
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg)
                    .child(a.summary.clone()),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        Button::new(SharedString::from(format!("allow-{}", a.id)))
                            .kind(ButtonKind::Primary)
                            .icon("check")
                            .label("Allow")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.answer(&allow, "allow", cx)),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("always-{}", a.id)))
                            .label("Always allow")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.answer(&always, "always", cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("deny-{}", a.id)))
                            .kind(ButtonKind::Ghost)
                            .label("Deny")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.answer(&deny, "deny", cx)),
                            ),
                    ),
            )
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let messages = self
            .chat
            .as_ref()
            .map(|c| c.messages.clone())
            .unwrap_or_default();
        let approvals = self
            .chat
            .as_ref()
            .map(|c| c.approvals.clone())
            .unwrap_or_default();
        let running = self.chat.as_ref().is_some_and(|c| c.running);
        let session = self.chat.as_ref().and_then(|c| c.session.clone());
        let transcript = rows(&messages);

        let mut list = div().flex().flex_col().gap(px(14.)).px(px(24.)).py(px(16.));
        if transcript.is_empty() && self.chat.is_some() {
            let this = cx.entity().downgrade();
            list = list.child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(8.))
                    .pt(px(24.))
                    .children(
                        self.suggestions()
                            .into_iter()
                            .enumerate()
                            .map(move |(i, s)| {
                                let this = this.clone();
                                let label = s.clone();
                                div()
                                    .id(("suggest", i))
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .px(px(12.))
                                    .py(px(6.))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(t.border_strong)
                                    .text_size(px(size::TEXT_MD))
                                    .text_color(t.fg_muted)
                                    .cursor_pointer()
                                    .hover(|st| st.bg(t.bg_hover).text_color(t.fg))
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |v, cx| v.send(s.to_string(), cx)).ok();
                                    })
                                    .child(icon("sparkles", 13., t.fg_subtle))
                                    .child(label)
                            }),
                    ),
            );
        }
        for (i, row) in transcript.iter().enumerate() {
            list = list.child(self.render_row(i, row, cx));
        }
        if !self.streaming.is_empty() {
            list = list.child(div().text_size(px(size::TEXT_MD)).text_color(t.fg).child(
                TextView::markdown("streaming", SharedString::from(self.streaming.clone())),
            ));
        } else if running && approvals.is_empty() {
            list = list.child(
                div()
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_subtle)
                    .child("Working…"),
            );
        }
        for a in &approvals {
            list = list.child(self.render_approval(a, cx));
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .when_some(session, |el, s| {
                let label = status_label(s.status, !approvals.is_empty());
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px(px(24.))
                        .py(px(8.))
                        .border_b_1()
                        .border_color(t.border)
                        .text_size(px(size::TEXT_SM))
                        .text_color(t.fg_muted)
                        .child(format!("{} · {label}", agent_name(s.agent)))
                        .when(running, |el| {
                            el.child(
                                Button::new("stop")
                                    .kind(ButtonKind::Ghost)
                                    .icon("stop")
                                    .label("Stop")
                                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                            )
                        }),
                )
            })
            .child(
                div()
                    .id("transcript")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(list),
            )
            .when_some(self.error.clone(), |el, e| {
                el.child(
                    div()
                        .mx(px(24.))
                        .mb(px(8.))
                        .text_size(px(size::TEXT_SM))
                        .text_color(t.danger.fg)
                        .child(e),
                )
            })
            .child(
                div()
                    .px(px(24.))
                    .pb(px(16.))
                    .child(Textarea::new(&self.composer)),
            )
    }
}

pub fn agent_name(kind: tab_core::model::AgentKind) -> &'static str {
    use tab_core::model::AgentKind::*;
    match kind {
        ClaudeCode => "Claude Code",
        OpenCode => "OpenCode",
        Codex => "Codex",
        Pi => "Pi",
    }
}
