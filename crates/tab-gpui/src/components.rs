//! Small building blocks shared by every screen: icons, buttons, badges,
//! key hints. The counterparts of `src/design/components.tsx`.

use gpui::{
    AnyElement, App, ClickEvent, ElementId, Hsla, IntoElement, ParentElement, RenderOnce,
    SharedString, Styled, Window, div, img, prelude::*, px, svg,
};

use crate::assets::icon_path;
use crate::theme::{FONT_MONO, size, theme};

/// An icon from the design system, in the current text color unless given.
pub fn icon(name: &str, px_size: f32, color: Hsla) -> impl IntoElement {
    svg()
        .path(icon_path(name))
        .size(px(px_size))
        .flex_none()
        .text_color(color)
}

/// The tab mark.
pub fn tab_mark(px_size: f32) -> impl IntoElement {
    img("brands/tab.svg").size(px(px_size)).flex_none()
}

/// A pill of text in a tone.
pub fn badge(text: impl Into<SharedString>, fg: Hsla, bg: Hsla) -> impl IntoElement {
    div()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(size::RADIUS_SM))
        .bg(bg)
        .text_color(fg)
        .text_size(px(size::TEXT_SM))
        .child(text.into())
}

/// A keyboard key, shown in buttons and menus.
pub fn kbd(key: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let t = theme(cx);
    div()
        .px(px(4.))
        .rounded(px(3.))
        .border_1()
        .border_color(t.border_strong)
        .text_color(t.fg_subtle)
        .text_size(px(10.))
        .font_family(FONT_MONO)
        .child(key.into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// Filled with the foreground color: the one main action.
    Primary,
    /// Raised with a border.
    Secondary,
    /// Text only until hovered.
    Ghost,
}

type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<&'static str>,
    key: Option<SharedString>,
    kind: ButtonKind,
    disabled: bool,
    on_click: Option<OnClick>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Button {
            id: id.into(),
            label: None,
            icon: None,
            key: None,
            kind: ButtonKind::Secondary,
            disabled: false,
            on_click: None,
        }
    }
    pub fn label(mut self, l: impl Into<SharedString>) -> Self {
        self.label = Some(l.into());
        self
    }
    pub fn icon(mut self, name: &'static str) -> Self {
        self.icon = Some(name);
        self
    }
    /// The shortcut shown at the end of the button.
    pub fn key(mut self, k: impl Into<SharedString>) -> Self {
        self.key = Some(k.into());
        self
    }
    pub fn kind(mut self, k: ButtonKind) -> Self {
        self.kind = k;
        self
    }
    pub fn disabled(mut self, d: bool) -> Self {
        self.disabled = d;
        self
    }
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let (fg, bg, border, hover) = match self.kind {
            ButtonKind::Primary => (t.bg_raised, t.fg, t.fg, t.fg.opacity(0.85)),
            ButtonKind::Secondary => (t.fg, t.bg_raised, t.border_strong, t.bg_hover),
            ButtonKind::Ghost => (
                t.fg_muted,
                gpui::transparent_black(),
                gpui::transparent_black(),
                t.bg_hover,
            ),
        };
        let key = self.key.map(|k| {
            let (kfg, kborder) = if self.kind == ButtonKind::Primary {
                (t.bg_raised.opacity(0.8), t.bg_raised.opacity(0.5))
            } else {
                (t.fg_subtle, t.border_strong)
            };
            div()
                .ml(px(4.))
                .px(px(4.))
                .rounded(px(3.))
                .border_1()
                .border_color(kborder)
                .text_color(kfg)
                .text_size(px(10.))
                .font_family(FONT_MONO)
                .child(k)
        });
        let mut el = div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(28.))
            .px(px(10.))
            .rounded(px(size::RADIUS_MD))
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(fg)
            .text_size(px(size::TEXT_MD))
            .when_some(self.icon, |el, name| el.child(icon(name, 15., fg)))
            .when_some(self.label, |el, l| el.child(l))
            .children(key);
        if self.disabled {
            el = el.opacity(0.5);
        } else {
            el = el.cursor_pointer().hover(move |s| s.bg(hover));
            if let Some(f) = self.on_click {
                el = el.on_click(f);
            }
        }
        el
    }
}

/// A segmented control: one of a few options, like Queue / Snoozed / Done.
pub fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &[(T, &'static str)],
    current: T,
    cx: &App,
    on_pick: impl Fn(T, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let t = theme(cx).clone();
    div()
        .flex()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(size::RADIUS_MD))
        .bg(t.bg_sunken)
        .children(options.iter().enumerate().map(|(i, (value, label))| {
            let selected = *value == current;
            let value = *value;
            let on_pick = on_pick.clone();
            div()
                .id((id, i))
                .px(px(10.))
                .py(px(3.))
                .rounded(px(size::RADIUS_SM))
                .text_size(px(size::TEXT_MD))
                .cursor_pointer()
                .when(selected, |el| {
                    el.bg(t.bg_raised).text_color(t.fg).shadow_sm()
                })
                .when(!selected, |el| {
                    el.text_color(t.fg_muted).hover(|s| s.text_color(t.fg))
                })
                .on_click(move |_, w, cx| on_pick(value, w, cx))
                .child(*label)
        }))
        .into_any_element()
}

/// "4m", "3h", "2d": how long ago, as the queue shows it.
pub fn ago(when: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    let s = (now - when).num_seconds().max(0);
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        86_400..604_800 => format!("{}d", s / 86_400),
        _ => format!("{}w", s / 604_800),
    }
}

#[cfg(test)]
mod tests {
    use super::ago;
    use chrono::{Duration, Utc};

    #[test]
    fn ages() {
        let now = Utc::now();
        assert_eq!(ago(now, now), "now");
        assert_eq!(ago(now - Duration::minutes(12), now), "12m");
        assert_eq!(ago(now - Duration::hours(5), now), "5h");
        assert_eq!(ago(now - Duration::days(3), now), "3d");
        assert_eq!(ago(now - Duration::days(15), now), "2w");
        // A clock that's a little behind the item's never shows a negative age.
        assert_eq!(ago(now + Duration::seconds(30), now), "now");
    }
}
