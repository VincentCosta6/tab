//! The ranked queue: Queue / Snoozed / Done, app filter chips, rows grouped
//! by urgency. j/k move, e marks done, s snoozes an hour, u reopens.

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, actions, div, prelude::*, px,
};
use tab_core::EngineEvent;
use tab_core::model::{ItemState, QueueItem};

use super::model::{self, APPS, Kinds};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::backend::backend;
use crate::components::{ago, icon, segmented, tab_mark};
use crate::theme::{size, theme};

actions!(
    queue,
    [
        SelectNext,
        SelectPrev,
        MarkDone,
        SnoozeHour,
        Reopen,
        FocusSearch,
        LeaveSearch
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("j", SelectNext, Some("Queue")),
        KeyBinding::new("down", SelectNext, Some("Queue")),
        KeyBinding::new("k", SelectPrev, Some("Queue")),
        KeyBinding::new("up", SelectPrev, Some("Queue")),
        KeyBinding::new("e", MarkDone, Some("Queue")),
        KeyBinding::new("s", SnoozeHour, Some("Queue")),
        KeyBinding::new("u", Reopen, Some("Queue")),
        KeyBinding::new("/", FocusSearch, Some("Queue")),
        KeyBinding::new("escape", LeaveSearch, Some("QueueSearch")),
        KeyBinding::new("down", SelectNext, Some("QueueSearch")),
        KeyBinding::new("up", SelectPrev, Some("QueueSearch")),
    ]);
}

/// The person picked a row (or the selection went away).
pub struct Selected(pub Option<QueueItem>);

pub struct Queue {
    view: ItemState,
    items: Vec<QueueItem>,
    app: Option<String>,
    selected: Option<String>,
    kinds: Kinds,
    loaded: bool,
    error: Option<String>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    search: Entity<InputState>,
    /// What's typed in search, and what it found (None until it answers).
    query: String,
    found: Option<Vec<QueueItem>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Selected> for Queue {}

impl Focusable for Queue {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Queue {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let events = crate::events::events(cx);
        let sub = cx.subscribe(&events, |this, _, ev: &Arc<EngineEvent>, cx| match &**ev {
            EngineEvent::ItemChanged { item, .. } => {
                let selected = this.selected.as_deref() == Some(item.id.as_str());
                this.reload(cx);
                if selected {
                    cx.emit(Selected(Some(item.clone())));
                }
            }
            EngineEvent::ConnectorChanged { .. } | EngineEvent::ConfigChanged => {
                this.load_kinds(cx)
            }
            _ => {}
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search everything  /"));
        let typed = cx.subscribe(&search, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.query = input.read(cx).value().trim().to_string();
                this.search(cx);
            }
        });
        let mut q = Queue {
            view: ItemState::Open,
            items: Vec::new(),
            app: None,
            selected: None,
            kinds: Kinds::default(),
            loaded: false,
            error: None,
            focus,
            scroll: ScrollHandle::new(),
            search,
            query: String::new(),
            found: None,
            _subs: vec![sub, typed],
        };
        q.reload(cx);
        q.load_kinds(cx);
        q
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let b = backend(cx);
        let states = vec![self.view];
        let task = crate::runtime::spawn(async move { b.store.list(&states) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok(items)) => {
                        this.items = items;
                        this.error = None;
                    }
                    Ok(Err(e)) => this.error = Some(e.to_string()),
                    Err(e) => this.error = Some(e),
                }
                this.loaded = true;
                if !this.query.is_empty() {
                    this.search(cx);
                }
                // The open item left this view (done, snoozed): select the next.
                if this
                    .selected
                    .as_ref()
                    .is_some_and(|id| !this.items.iter().any(|i| &i.id == id))
                {
                    this.selected = None;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Keyword matches first, then things that mean the same once the
    /// embedding model is ready (as the Tauri app's `search_items`).
    fn search(&mut self, cx: &mut Context<Self>) {
        if self.query.is_empty() {
            self.found = None;
            cx.notify();
            return;
        }
        let (b, query, states) = (backend(cx), self.query.clone(), vec![self.view]);
        let task = crate::runtime::spawn(async move {
            let qv = b.store.embed_query(&query);
            b.store
                .search_hybrid(&query, qv.as_deref(), &states, &Default::default(), 200)
        });
        let asked = self.query.clone();
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                // Only the answer to what's typed now.
                if this.query != asked {
                    return;
                }
                match res {
                    Ok(Ok(items)) => this.found = Some(items),
                    Ok(Err(e)) => this.error = Some(e.to_string()),
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    fn leave_search(&mut self, _: &LeaveSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self.query.clear();
        self.found = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The items shown: search results while searching, else the view.
    fn current(&self) -> &[QueueItem] {
        match &self.found {
            Some(f) if !self.query.is_empty() => f,
            _ => &self.items,
        }
    }

    /// Grouped by urgency in the queue; flat for search and the other views.
    fn grouping(&self) -> bool {
        self.view == ItemState::Open && self.query.is_empty()
    }

    fn load_kinds(&mut self, cx: &mut Context<Self>) {
        let b = backend(cx);
        let task =
            crate::runtime::spawn(async move { b.with_engine(async |e| Ok(e.connectors())).await });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(cs)) = task.await {
                this.update(cx, |this, cx| {
                    this.kinds = Kinds::from_connectors(&cs);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn set_view(&mut self, view: ItemState, cx: &mut Context<Self>) {
        if self.view != view {
            self.view = view;
            self.selected = None;
            cx.emit(Selected(None));
            self.reload(cx);
        }
    }

    /// The rows as drawn, top to bottom.
    fn rows(&self) -> Vec<&QueueItem> {
        let visible: Vec<QueueItem> = self.filtered().cloned().collect();
        let ids: Vec<String> = if self.grouping() {
            model::grouped(&visible)
                .into_iter()
                .flat_map(|(_, r)| r.into_iter().map(|i| i.id.clone()))
                .collect()
        } else {
            visible.iter().map(|i| i.id.clone()).collect()
        };
        ids.iter()
            .filter_map(|id| self.current().iter().find(|i| &i.id == id))
            .collect()
    }

    fn filtered(&self) -> impl Iterator<Item = &QueueItem> {
        self.current().iter().filter(|i| {
            self.app
                .as_deref()
                .is_none_or(|a| model::app_of(&i.event.kind) == a)
        })
    }

    pub fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.selected = id.clone();
        let item = id.and_then(|id| self.current().iter().find(|i| i.id == id).cloned());
        cx.emit(Selected(item));
        cx.notify();
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let rows: Vec<String> = self.rows().iter().map(|i| i.id.clone()).collect();
        if rows.is_empty() {
            return;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|s| rows.iter().position(|r| r == s));
        let next = match at {
            None => 0,
            Some(i) => (i as isize + by).clamp(0, rows.len() as isize - 1) as usize,
        };
        self.select(Some(rows[next].clone()), cx);
        // The list's children are rows with a heading before each group.
        let index = if self.grouping() {
            let visible: Vec<QueueItem> = self.filtered().cloned().collect();
            let mut i = 0;
            let mut found = next;
            for (_, group) in model::grouped(&visible) {
                i += 1;
                if found < group.len() {
                    i += found;
                    break;
                }
                found -= group.len();
                i += group.len();
            }
            i
        } else {
            next
        };
        self.scroll.scroll_to_item(index);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.step(1, cx);
    }
    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, cx);
    }

    /// Done, snooze or reopen the selected row. The row leaves the list at
    /// once and the next one is selected; the engine's event confirms it.
    fn act(&mut self, act: Act, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let rows: Vec<String> = self.rows().iter().map(|i| i.id.clone()).collect();
        let at = rows.iter().position(|r| *r == id).unwrap_or(0);
        self.items.retain(|i| i.id != id);
        if let Some(f) = &mut self.found {
            f.retain(|i| i.id != id);
        }
        let next = self
            .rows()
            .get(at.min(rows.len().saturating_sub(2)))
            .map(|i| i.id.clone());
        self.select(next, cx);
        let b = backend(cx);
        let task = crate::runtime::spawn(async move {
            b.with_engine(async |e| match act {
                Act::Done => e.mark_done(&id).await,
                Act::Snooze(m) => e.snooze(&id, chrono::Utc::now() + chrono::Duration::minutes(m)),
                Act::Reopen => e.reopen(&id),
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                if let Err(e) | Ok(Err(e)) = res.map(|r| r.map(|_| ())) {
                    this.error = Some(e);
                }
                this.reload(cx);
            })
            .ok();
        })
        .detach();
    }

    fn mark_done(&mut self, _: &MarkDone, _: &mut Window, cx: &mut Context<Self>) {
        if self.view != ItemState::Done {
            self.act(Act::Done, cx);
        }
    }
    fn snooze_hour(&mut self, _: &SnoozeHour, _: &mut Window, cx: &mut Context<Self>) {
        if self.view != ItemState::Done {
            self.act(Act::Snooze(60), cx);
        }
    }
    fn reopen(&mut self, _: &Reopen, _: &mut Window, cx: &mut Context<Self>) {
        if self.view != ItemState::Open {
            self.act(Act::Reopen, cx);
        }
    }

    fn render_chips(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let counts = model::counts(self.current());
        let chip = |id: SharedString,
                    label: SharedString,
                    glyph: Option<&'static str>,
                    count: usize,
                    active: bool,
                    cx: &mut Context<Self>| {
            let app = if id.is_empty() {
                None
            } else {
                Some(id.to_string())
            };
            div()
                .id(SharedString::from(format!("chip-{id}")))
                .flex()
                .items_center()
                .gap(px(5.))
                .px(px(8.))
                .py(px(2.))
                .rounded_full()
                .border_1()
                .text_size(px(size::TEXT_SM))
                .cursor_pointer()
                .when(active, |el| {
                    el.bg(t.fg).border_color(t.fg).text_color(t.bg_raised)
                })
                .when(!active, |el| {
                    el.border_color(t.border_strong)
                        .text_color(t.fg)
                        .hover(|s| s.bg(t.bg_hover))
                })
                .when_some(glyph, |el, g| {
                    el.child(if g == "tab" {
                        tab_mark(12.).into_any_element()
                    } else {
                        icon(g, 12., if active { t.bg_raised } else { t.fg_muted })
                            .into_any_element()
                    })
                })
                .child(label)
                .child(
                    div()
                        .text_color(if active {
                            t.bg_raised.opacity(0.7)
                        } else {
                            t.fg_subtle
                        })
                        .child(count.to_string()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.app = if this.app == app { None } else { app.clone() };
                    cx.notify();
                }))
        };
        let mut chips = vec![
            chip(
                "".into(),
                "All".into(),
                None,
                self.current().len(),
                self.app.is_none(),
                cx,
            )
            .into_any_element(),
        ];
        for (id, label, glyph) in APPS {
            if let Some(n) = counts.get(*id) {
                let active = self.app.as_deref() == Some(*id);
                chips.push(
                    chip((*id).into(), (*label).into(), Some(glyph), *n, active, cx)
                        .into_any_element(),
                );
            }
        }
        div().flex().flex_wrap().gap(px(6.)).children(chips)
    }

    fn render_row(&self, item: &QueueItem, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let k = self.kinds.get(&item.event.kind);
        let tone = t.tone(k.tone);
        let selected = self.selected.as_deref() == Some(item.id.as_str());
        let id = item.id.clone();
        let score = item
            .ranking
            .as_ref()
            .map(|r| (r.score, t.urgency(r.urgency)));
        let glyph = if model::app_of(&item.event.kind) == "tab" {
            tab_mark(18.).into_any_element()
        } else {
            icon(&k.icon, 16., tone.fg).into_any_element()
        };
        div()
            .id(SharedString::from(format!("row-{}", item.id)))
            .flex()
            .gap(px(12.))
            .px(px(16.))
            .py(px(10.))
            .border_l_2()
            .cursor_pointer()
            .when(selected, |el| el.bg(t.bg_selected).border_color(t.focus))
            .when(!selected, |el| {
                el.border_color(gpui::transparent_black())
                    .hover(|s| s.bg(t.bg_hover))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.focus, cx);
                this.select(Some(id.clone()), cx);
            }))
            .child(
                div()
                    .size(px(28.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(size::RADIUS_MD))
                    .bg(tone.bg)
                    .child(glyph),
            )
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
                            .text_color(t.fg)
                            .truncate()
                            .child(item.event.title.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .text_size(px(size::TEXT_SM))
                            .text_color(t.fg_subtle)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(model::byline(item)),
                            )
                            .when_some(
                                item.snoozed_until
                                    .filter(|_| item.state == ItemState::Snoozed),
                                |el, until| {
                                    el.child(format!(
                                        "until {}",
                                        until.with_timezone(&chrono::Local).format("%a %H:%M")
                                    ))
                                },
                            )
                            .child(ago(item.event.occurred_at, chrono::Utc::now())),
                    )
                    .when_some(
                        item.ranking
                            .as_ref()
                            .map(|r| r.reason.clone())
                            .filter(|r| !r.is_empty()),
                        |el, reason| {
                            el.child(
                                div()
                                    .text_size(px(size::TEXT_SM))
                                    .text_color(t.fg_muted)
                                    .truncate()
                                    .child(reason),
                            )
                        },
                    ),
            )
            .when_some(score, |el, (score, color)| {
                el.child(
                    div()
                        .flex_none()
                        .h(px(18.))
                        .px(px(6.))
                        .rounded_full()
                        .bg(color)
                        .text_color(t.bg_raised)
                        .text_size(px(size::TEXT_XS))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(score.to_string()),
                )
            })
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        if !self.query.is_empty() {
            return div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.))
                .py(px(48.))
                .child(icon("search", 22., t.fg_subtle))
                .child(
                    div()
                        .text_size(px(size::TEXT_LG))
                        .text_color(t.fg)
                        .child("No matches"),
                )
                .child(
                    div()
                        .text_size(px(size::TEXT_MD))
                        .text_color(t.fg_muted)
                        .child("Search covers titles, bodies, people and places."),
                );
        }
        let (title, body) = match self.view {
            ItemState::Open => (
                "Queue is clear",
                "New work from your connections lands here, ranked.",
            ),
            ItemState::Snoozed => ("Nothing snoozed", ""),
            ItemState::Done => ("Nothing done", ""),
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.))
            .py(px(48.))
            .child(icon("check", 22., t.fg_subtle))
            .child(
                div()
                    .text_size(px(size::TEXT_LG))
                    .text_color(t.fg)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(size::TEXT_MD))
                    .text_color(t.fg_muted)
                    .child(body),
            )
    }
}

#[derive(Clone, Copy)]
enum Act {
    Done,
    Snooze(i64),
    Reopen,
}

impl Render for Queue {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let visible: Vec<QueueItem> = self.filtered().cloned().collect();
        let groups: Vec<(Option<tab_core::model::Urgency>, Vec<QueueItem>)> = if self.grouping() {
            model::grouped(&visible)
                .into_iter()
                .map(|(u, r)| (Some(u), r.into_iter().cloned().collect()))
                .collect()
        } else {
            vec![(None, visible.clone())]
        };
        let mut list = div()
            .id("queue-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll);
        if self.loaded && visible.is_empty() {
            list = list.child(self.render_empty(cx));
        }
        for (u, rows) in groups {
            if let Some(u) = u {
                list = list.child(
                    div()
                        .px(px(16.))
                        .pt(px(14.))
                        .pb(px(6.))
                        .text_size(px(size::TEXT_XS))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(t.urgency(u))
                        .child(model::urgency_heading(u).to_uppercase()),
                );
            }
            for item in &rows {
                list = list.child(self.render_row(item, cx));
            }
        }
        let view = self.view;
        div()
            .key_context("Queue")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::mark_done))
            .on_action(cx.listener(Self::snooze_hour))
            .on_action(cx.listener(Self::reopen))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::leave_search))
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .p(px(16.))
                    .border_b_1()
                    .border_color(t.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(size::TEXT_XL))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(t.fg)
                                    .child("Work"),
                            )
                            .child(segmented(
                                "queue-view",
                                &[
                                    (ItemState::Open, "Queue"),
                                    (ItemState::Snoozed, "Snoozed"),
                                    (ItemState::Done, "Done"),
                                ],
                                view,
                                cx,
                                {
                                    let this = cx.entity().downgrade();
                                    move |v, _, cx| {
                                        this.update(cx, |q, cx| q.set_view(v, cx)).ok();
                                    }
                                },
                            )),
                    )
                    .child(
                        div().key_context("QueueSearch").child(
                            Input::new(&self.search).prefix(icon("search", 14., t.fg_subtle)),
                        ),
                    )
                    .child(self.render_chips(cx))
                    .when_some(self.error.clone(), |el, e| {
                        el.child(
                            div()
                                .text_size(px(size::TEXT_SM))
                                .text_color(t.danger.fg)
                                .child(e),
                        )
                    }),
            )
            .child(list)
    }
}

impl Queue {
    pub fn focus_handle_of(&self) -> FocusHandle {
        self.focus.clone()
    }
}
