//! The window's frame: the nav down the left and the active tab. The
//! counterpart of `src/App.tsx`'s `Shell`.

use gpui::{
    AnyView, App, Context, Entity, InteractiveElement, IntoElement, KeyBinding, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Window, actions, div, prelude::*, px,
};

use crate::components::{icon, tab_mark};
use crate::theme::{FONT_SANS, size, theme};
use crate::work::WorkTab;

actions!(
    shell,
    [
        ShowWork,
        ShowProjects,
        ShowWorkflows,
        ShowConnections,
        ShowSettings
    ]
);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Work,
    Projects,
    Workflows,
    Connections,
    Settings,
}

impl Tab {
    const ALL: [Tab; 5] = [
        Tab::Work,
        Tab::Projects,
        Tab::Workflows,
        Tab::Connections,
        Tab::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Work => "Work",
            Tab::Projects => "Projects",
            Tab::Workflows => "Workflows",
            Tab::Connections => "Connections",
            Tab::Settings => "Settings",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Tab::Work => "inbox",
            Tab::Projects => "folder",
            Tab::Workflows => "workflow",
            Tab::Connections => "plug",
            Tab::Settings => "settings",
        }
    }

    /// In the bottom group of the nav.
    fn footer(self) -> bool {
        matches!(self, Tab::Connections | Tab::Settings)
    }
}

pub fn bind_keys(cx: &mut App) {
    // Cmd on macOS, Ctrl elsewhere.
    cx.bind_keys([
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-1"
            } else {
                "ctrl-1"
            },
            ShowWork,
            None,
        ),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-2"
            } else {
                "ctrl-2"
            },
            ShowProjects,
            None,
        ),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-4"
            } else {
                "ctrl-4"
            },
            ShowConnections,
            None,
        ),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-5"
            } else {
                "ctrl-5"
            },
            ShowWorkflows,
            None,
        ),
    ]);
}

pub struct Shell {
    tab: Tab,
    work: Entity<WorkTab>,
    settings: Option<Entity<crate::settings::SettingsTab>>,
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let work = cx.new(|cx| WorkTab::new(window, cx));
        Shell {
            tab: Tab::Work,
            work,
            settings: None,
        }
    }

    fn show(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == Tab::Settings && self.settings.is_none() {
            self.settings = Some(cx.new(|cx| crate::settings::SettingsTab::new(window, cx)));
        }
        if tab == Tab::Work {
            let q = self.work.read(cx).queue().clone();
            window.focus(&q.read(cx).focus_handle_of(), cx);
        }
        cx.notify();
    }

    fn view(&self, cx: &mut Context<Self>) -> AnyView {
        match self.tab {
            Tab::Work => self.work.clone().into(),
            Tab::Settings if self.settings.is_some() => self.settings.clone().unwrap().into(),
            other => cx.new(|_| NotPorted(other)).into(),
        }
    }

    fn nav_item(&self, tab: Tab, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let active = self.tab == tab;
        div()
            .id(SharedString::from(format!("nav-{}", tab.label())))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(3.))
            .w(px(76.))
            .py(px(8.))
            .rounded(px(size::RADIUS_LG))
            .cursor_pointer()
            .text_size(px(size::TEXT_SM))
            .when(active, |el| el.bg(t.bg_raised).text_color(t.fg).shadow_sm())
            .when(!active, |el| {
                el.text_color(t.fg_muted).hover(|s| s.text_color(t.fg))
            })
            .on_click(cx.listener(move |this, _, window, cx| this.show(tab, window, cx)))
            .child(if tab == Tab::Work {
                tab_mark(22.).into_any_element()
            } else {
                icon(tab.icon(), 18., if active { t.fg } else { t.fg_muted }).into_any_element()
            })
            .child(tab.label())
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let mut top = Vec::new();
        let mut bottom = Vec::new();
        for tab in Tab::ALL {
            let el = self.nav_item(tab, cx).into_any_element();
            if tab.footer() {
                bottom.push(el)
            } else {
                top.push(el)
            }
        }
        let view = self.view(cx);
        div()
            .on_action(cx.listener(|this, _: &ShowWork, w, cx| this.show(Tab::Work, w, cx)))
            .on_action(cx.listener(|this, _: &ShowProjects, w, cx| this.show(Tab::Projects, w, cx)))
            .on_action(
                cx.listener(|this, _: &ShowWorkflows, w, cx| this.show(Tab::Workflows, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ShowConnections, w, cx| this.show(Tab::Connections, w, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowSettings, w, cx| this.show(Tab::Settings, w, cx)))
            .flex()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .font_family(FONT_SANS)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .items_center()
                    .w(px(92.))
                    .flex_none()
                    .h_full()
                    .py(px(12.))
                    .border_r_1()
                    .border_color(t.border)
                    .bg(t.bg_sunken)
                    .child(div().flex().flex_col().gap(px(4.)).children(top))
                    .child(div().flex().flex_col().gap(px(4.)).children(bottom)),
            )
            .child(div().flex_1().min_w_0().h_full().child(view))
    }
}

/// A tab the GPUI app doesn't have yet.
struct NotPorted(Tab);

impl Render for NotPorted {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .child(icon(self.0.icon(), 28., t.fg_subtle))
            .child(
                div()
                    .text_size(px(size::TEXT_LG))
                    .text_color(t.fg)
                    .child(self.0.label()),
            )
            .child(
                div()
                    .text_size(px(size::TEXT_MD))
                    .text_color(t.fg_muted)
                    .child("Not in the GPUI app yet."),
            )
    }
}
