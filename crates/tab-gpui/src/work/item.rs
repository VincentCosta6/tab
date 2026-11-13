//! The open item: what it is, why it ranks where it does, its content, and
//! Done / Snooze / Open.

use gpui::{
    Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, prelude::*, px,
};
use tab_core::model::{Block, ItemState, Preview, QueueItem};

use gpui_component::Sizable;
use gpui_component::button::Button as MenuButton;
use gpui_component::menu::{DropdownMenu, PopupMenuItem};

use super::model::{Kinds, byline, snooze_options, urgency_heading};
use crate::backend::backend;
use crate::blocks::blocks;
use crate::chat::ChatView;
use crate::components::{Button, ButtonKind, ago, badge, icon};
use crate::theme::{size, theme};
use gpui::Entity;

pub struct ItemPane {
    item: Option<QueueItem>,
    preview: Option<Preview>,
    kinds: Kinds,
    pane: Pane,
    chat: Option<Entity<ChatView>>,
}

/// Which side of the item is showing. Chat is the default, except for email,
/// which opens on the message itself (as the React app does).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Details,
    Chat,
}

impl ItemPane {
    pub fn new(cx: &mut Context<Self>) -> Self {
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
        ItemPane {
            item: None,
            preview: None,
            kinds: Kinds::default(),
            pane: Pane::Chat,
            chat: None,
        }
    }

    pub fn show(&mut self, item: Option<QueueItem>, cx: &mut Context<Self>) {
        let same = item.as_ref().map(|i| &i.id) == self.item.as_ref().map(|i| &i.id);
        self.item = item;
        if !same {
            self.chat = None;
            self.pane = match &self.item {
                Some(i) if i.event.kind.starts_with("email.") => Pane::Details,
                _ => Pane::Chat,
            };
            self.preview = None;
            self.load_preview(cx);
        }
        cx.notify();
    }

    fn load_preview(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.item.as_ref().map(|i| i.id.clone()) else {
            return;
        };
        let b = backend(cx);
        let task =
            crate::runtime::spawn(
                async move { b.with_engine(async |e| e.preview(&id).await).await },
            );
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(p)) = task.await {
                this.update(cx, |this, cx| {
                    if this.item.as_ref().is_some_and(|i| i.id == p.item_id) {
                        this.preview = Some(p);
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn act(&mut self, act: Act, cx: &mut Context<Self>) {
        let Some(id) = self.item.as_ref().map(|i| i.id.clone()) else {
            return;
        };
        let b = backend(cx);
        let _ = crate::runtime::spawn(async move {
            let r = b
                .with_engine(async |e| match act {
                    Act::Done => e.mark_done(&id).await.map(|_| ()),
                    Act::Reopen => e.reopen(&id).map(|_| ()),
                    Act::Snooze(m) => e
                        .snooze(&id, chrono::Utc::now() + chrono::Duration::minutes(m))
                        .map(|_| ()),
                })
                .await;
            if let Err(e) = r {
                tracing::warn!(error = %e, "item action failed");
            }
        });
    }
}

#[derive(Clone, Copy)]
enum Act {
    Done,
    Snooze(i64),
    Reopen,
}

impl Render for ItemPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(item) = self.item.clone() else {
            return self.details(cx);
        };
        let t = theme(cx).clone();
        if self.pane == Pane::Chat && self.chat.is_none() {
            self.chat = Some(cx.new(|cx| ChatView::new(item.clone(), window, cx)));
        }
        let body = match (self.pane, self.chat.clone()) {
            (Pane::Chat, Some(chat)) => chat.into_any_element(),
            _ => self.details(cx),
        };
        let details = if item.event.kind.starts_with("email.") {
            "Email"
        } else {
            "Details"
        };
        let mut tabs = div()
            .flex()
            .gap(px(16.))
            .px(px(32.))
            .border_b_1()
            .border_color(t.border);
        for (id, label, pane) in [
            ("pane-chat", "Chat", Pane::Chat),
            ("pane-details", details, Pane::Details),
        ] {
            let active = self.pane == pane;
            tabs = tabs.child(
                div()
                    .id(id)
                    .py(px(10.))
                    .text_size(px(size::TEXT_MD))
                    .cursor_pointer()
                    .border_b_2()
                    .border_color(if active {
                        t.accent.fg
                    } else {
                        gpui::transparent_black()
                    })
                    .text_color(if active { t.fg } else { t.fg_muted })
                    .font_weight(if active {
                        gpui::FontWeight::MEDIUM
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pane = pane;
                        cx.notify();
                    })),
            );
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .child(tabs)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

impl ItemPane {
    fn details(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme(cx).clone();
        let Some(item) = self.item.clone() else {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .bg(t.bg)
                .child(icon("inbox", 28., t.fg_subtle))
                .child(
                    div()
                        .text_size(px(size::TEXT_LG))
                        .text_color(t.fg_muted)
                        .child("Pick something from the queue"),
                )
                .child(
                    div()
                        .text_size(px(size::TEXT_SM))
                        .text_color(t.fg_subtle)
                        .child("j / k to move, e for done, s to snooze"),
                )
                .into_any_element();
        };
        let k = self.kinds.get(&item.event.kind);
        let tone = t.tone(k.tone);
        let ranking = item.ranking.clone();
        let content: Vec<Block> = match &self.preview {
            Some(p) if !p.blocks.is_empty() => p.blocks.clone(),
            _ => {
                let mut b = item.event.blocks.clone();
                if b.is_empty()
                    && let Some(body) = item.event.body.clone()
                {
                    b.push(Block::Text {
                        text: body,
                        markdown: false,
                    });
                }
                b
            }
        };
        let open = item.state != ItemState::Open;
        let url = item.event.url.clone();
        div()
            .id("item-pane")
            .size_full()
            .overflow_y_scroll()
            .bg(t.bg)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .max_w(px(760.))
                    .px(px(32.))
                    .py(px(24.))
                    // Kind, urgency.
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .size(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(size::RADIUS_SM))
                                    .bg(tone.bg)
                                    .child(icon(&k.icon, 14., tone.fg)),
                            )
                            .child(badge(k.label.clone(), tone.fg, tone.bg))
                            .when_some(ranking.as_ref(), |el, r| {
                                el.child(
                                    div()
                                        .text_size(px(size::TEXT_MD))
                                        .text_color(t.urgency(r.urgency))
                                        .child(urgency_heading(r.urgency)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(size::TEXT_2XL))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(t.fg)
                            .line_height(px(30.))
                            .child(item.event.title.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .text_size(px(size::TEXT_MD))
                            .text_color(t.fg_muted)
                            .child(byline(&item))
                            .child(
                                div()
                                    .text_color(t.fg_subtle)
                                    .child(ago(item.event.occurred_at, chrono::Utc::now())),
                            ),
                    )
                    // Actions.
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .when(!open, |el| {
                                el.child(
                                    Button::new("done")
                                        .kind(ButtonKind::Primary)
                                        .icon("check")
                                        .label("Done")
                                        .key("E")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.act(Act::Done, cx)),
                                        ),
                                )
                                .child({
                                    let pane = cx.entity().downgrade();
                                    MenuButton::new("snooze")
                                        .outline()
                                        .small()
                                        .icon(
                                            gpui_component::Icon::default()
                                                .path(crate::assets::icon_path("clock")),
                                        )
                                        .label("Snooze")
                                        .dropdown_menu(move |mut menu, _, _| {
                                            for (label, _key, minutes) in
                                                snooze_options(chrono::Local::now())
                                            {
                                                let pane = pane.clone();
                                                menu =
                                                    menu.item(PopupMenuItem::new(label).on_click(
                                                        move |_, _, cx| {
                                                            pane.update(cx, |p, cx| {
                                                                p.act(Act::Snooze(minutes), cx)
                                                            })
                                                            .ok();
                                                        },
                                                    ));
                                            }
                                            menu
                                        })
                                })
                            })
                            .when(open, |el| {
                                el.child(
                                    Button::new("reopen")
                                        .icon("undo")
                                        .label("Reopen")
                                        .key("U")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.act(Act::Reopen, cx)),
                                        ),
                                )
                            })
                            .when_some(url, |el, url| {
                                el.child(
                                    Button::new("open")
                                        .icon("external")
                                        .label("Open")
                                        .on_click(move |_, _, cx| cx.open_url(&url)),
                                )
                            }),
                    )
                    // Why it ranks here.
                    .when_some(ranking, |el, r| {
                        el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .p(px(16.))
                                .rounded(px(size::RADIUS_LG))
                                .border_1()
                                .border_color(t.border)
                                .bg(t.bg_raised)
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(10.))
                                        .child(
                                            div()
                                                .px(px(6.))
                                                .rounded_full()
                                                .bg(t.urgency(r.urgency))
                                                .text_color(t.bg_raised)
                                                .text_size(px(size::TEXT_XS))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child(r.score.to_string()),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_size(px(size::TEXT_MD))
                                                .text_color(t.fg)
                                                .child(r.reason.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(px(size::TEXT_SM))
                                        .text_color(t.fg_subtle)
                                        .child(SharedString::from(match r.ai_adjustment {
                                            Some(a) => format!(
                                                "Rules {} · AI {:+} · via {}",
                                                r.base_score, a, r.ranked_by
                                            ),
                                            None => format!(
                                                "Rules {} · via {}",
                                                r.base_score, r.ranked_by
                                            ),
                                        })),
                                ),
                        )
                    })
                    .child(blocks(&content, cx))
                    .when(!item.event.entities.is_empty(), |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .text_size(px(size::TEXT_XS))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(t.fg_subtle)
                                        .child("IN THE GRAPH"),
                                )
                                .child(
                                    div().flex().flex_wrap().gap(px(6.)).children(
                                        item.event.entities.iter().map(|e| {
                                            badge(e.label.clone(), t.fg_muted, t.bg_sunken)
                                        }),
                                    ),
                                ),
                        )
                    }),
            )
            .into_any_element()
    }
}
