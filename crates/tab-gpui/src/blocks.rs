//! Rich content blocks (`tab_core::model::Block`), one element per
//! variant, like `src/design/blocks.tsx`.

use gpui::{App, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};
use tab_core::model::{Block, Field};

use crate::components::badge;
use crate::theme::{FONT_MONO, size, theme};

pub fn blocks(items: &[Block], cx: &App) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .children(items.iter().enumerate().map(|(i, b)| block(i, b, cx)))
}

fn block(i: usize, b: &Block, cx: &App) -> gpui::AnyElement {
    let t = theme(cx).clone();
    let text = |s: &str| {
        div()
            .text_size(px(size::TEXT_MD))
            .text_color(t.fg)
            .line_height(px(20.))
            .child(SharedString::from(s.to_string()))
    };
    match b {
        // TODO(gpui): render markdown (headings, lists, links, code).
        Block::Text { text: s, .. } => text(s).into_any_element(),
        Block::Quote { text: s, author } => div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(16.))
            .py(px(10.))
            .border_l_2()
            .border_color(t.border_strong)
            .bg(t.bg_sunken)
            .rounded_r(px(size::RADIUS_MD))
            .when_some(author.clone(), |el, a| {
                el.child(
                    div()
                        .text_size(px(size::TEXT_SM))
                        .text_color(t.fg_muted)
                        .child(a),
                )
            })
            .child(text(s))
            .into_any_element(),
        Block::Fields { items } => div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .children(items.iter().map(|f| field_row(f, cx)))
            .into_any_element(),
        Block::Stats { items } => div()
            .flex()
            .gap(px(20.))
            .children(items.iter().map(|f| {
                let tone = t.tone(f.tone);
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(size::TEXT_XL))
                            .text_color(tone.fg)
                            .child(f.value.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(size::TEXT_SM))
                            .text_color(t.fg_muted)
                            .child(f.label.clone()),
                    )
            }))
            .into_any_element(),
        Block::Badges { items } => div()
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .children(items.iter().map(|f| {
                let tone = t.tone(f.tone);
                let label = if f.value.is_empty() {
                    f.label.clone()
                } else {
                    format!("{} {}", f.label, f.value)
                };
                badge(label, tone.fg, tone.bg)
            }))
            .into_any_element(),
        Block::Links { items } => div()
            .flex()
            .flex_wrap()
            .gap(px(10.))
            .children(items.iter().enumerate().map(|(j, l)| {
                let url = l.url.clone();
                div()
                    .id(("link", i * 1000 + j))
                    .text_size(px(size::TEXT_MD))
                    .text_color(t.accent.fg)
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .child(l.label.clone())
            }))
            .into_any_element(),
        Block::Code { text: s, .. } => div()
            .p(px(12.))
            .rounded(px(size::RADIUS_MD))
            .bg(t.bg_sunken)
            .font_family(FONT_MONO)
            .text_size(px(size::TEXT_SM))
            .text_color(t.fg)
            .child(s.clone())
            .into_any_element(),
        // No webview here: show the email as text. Remote content never loads.
        Block::Html { html } => {
            let md = tab_core::knowledge::extract::html_to_markdown(html).unwrap_or_default();
            text(&md).into_any_element()
        }
        Block::Choices { prompt, options } => div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(size::RADIUS_LG))
            .border_1()
            .border_dashed()
            .border_color(t.warning.fg)
            .child(
                div()
                    .text_size(px(size::TEXT_MD))
                    .text_color(t.fg)
                    .child(prompt.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .children(options.iter().map(|o| {
                        div()
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(px(size::RADIUS_SM))
                            .border_1()
                            .border_color(t.border)
                            .text_size(px(size::TEXT_SM))
                            .text_color(t.fg_muted)
                            .child(o.clone())
                    })),
            )
            .into_any_element(),
    }
}

fn field_row(f: &Field, cx: &App) -> impl IntoElement {
    let t = theme(cx);
    div()
        .flex()
        .gap(px(12.))
        .text_size(px(size::TEXT_MD))
        .child(
            div()
                .w(px(120.))
                .flex_none()
                .text_color(t.fg_muted)
                .child(f.label.clone()),
        )
        .child(
            div()
                .flex_1()
                .text_color(t.tone(f.tone).fg)
                .child(f.value.clone()),
        )
}
