//! Connections: what's installed, the marketplace of built-in connectors,
//! and one connector's settings. The counterpart of
//! `src/modules/connections/ConnectionsTab.tsx`.
//!
//! Remote MCP servers, the GitHub and email sign-in cards, API usage and
//! running a connection in tab cloud are still only in the Tauri app.

use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::switch::Switch;
use serde_json::Value;
use tab_core::EngineEvent;
use tab_core::model::{ConnectorInfo, ConnectorStatus, SettingKind};

use crate::backend::backend;
use crate::components::{Button, ButtonKind, ago, badge, icon};
use crate::theme::{Theme, Tone, size, theme};

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Installed,
    Marketplace,
}

const SHELVES: [(&str, &str); 4] = [
    ("code", "Code"),
    ("communication", "Communication"),
    ("work", "Planning"),
    ("built_in", "Built in"),
];

/// The status line under a connector's name, and its tone.
pub fn status_text(
    s: &ConnectorStatus,
    available: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> (&'static str, String) {
    if !available {
        return ("neutral", "Coming soon".into());
    }
    match s {
        ConnectorStatus::Disabled => ("neutral", "Not installed".into()),
        ConnectorStatus::Starting => ("info", "Starting…".into()),
        ConnectorStatus::Running { last_sync } => (
            "success",
            match last_sync {
                Some(t) => match ago(*t, now) {
                    a if a == "now" => "Synced just now".into(),
                    a => format!("Synced {a} ago"),
                },
                None => "Connected".into(),
            },
        ),
        ConnectorStatus::RateLimited { until } => (
            "warning",
            format!("Rate limited, resumes in {}", ago(now, *until)),
        ),
        ConnectorStatus::Error { message } => ("danger", message.clone()),
        ConnectorStatus::Stopped => ("neutral", "Stopped".into()),
        ConnectorStatus::InCloud => ("accent", "Runs in tab cloud".into()),
    }
}

fn tone_of(t: &Theme, name: &str) -> Tone {
    match name {
        "success" => t.success,
        "info" => t.info,
        "warning" => t.warning,
        "danger" => t.danger,
        "accent" => t.accent,
        _ => t.neutral,
    }
}

/// "per hour", "per 15 min".
pub fn per(seconds: u32) -> String {
    if seconds >= 3600 {
        format!("per {} hour", seconds / 3600)
    } else if seconds >= 60 {
        format!("per {} min", seconds / 60)
    } else {
        format!("per {seconds}s")
    }
}

/// A connector's settings, being edited.
struct Draft {
    id: String,
    settings: serde_json::Map<String, Value>,
    inputs: Vec<(String, Entity<InputState>)>,
}

pub struct ConnectionsTab {
    view: View,
    connectors: Vec<ConnectorInfo>,
    open: Option<String>,
    draft: Option<Draft>,
    busy: bool,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

impl ConnectionsTab {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let events = crate::events::events(cx);
        let live = cx.subscribe(&events, |this, _, ev: &Arc<EngineEvent>, cx| {
            if matches!(
                **ev,
                EngineEvent::ConnectorChanged { .. } | EngineEvent::ConfigChanged
            ) {
                this.reload(cx);
            }
        });
        let mut this = ConnectionsTab {
            view: View::Installed,
            connectors: Vec::new(),
            open: None,
            draft: None,
            busy: false,
            error: None,
            _subs: vec![live],
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let b = backend(cx);
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
    }

    fn open(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.open = id.clone();
        self.error = None;
        self.draft = None;
        if let Some(c) = id.and_then(|id| self.connectors.iter().find(|c| c.descriptor.id == id)) {
            let c = c.clone();
            let b = backend(cx);
            let cfg = crate::runtime::handle().block_on(b.config_masked());
            let settings = cfg
                .connectors
                .get(&c.descriptor.id)
                .map(|c| c.settings.clone())
                .unwrap_or_default();
            let mut inputs = Vec::new();
            for f in &c.descriptor.settings {
                if f.kind == SettingKind::Toggle || hidden(&c.descriptor.id, &f.key) {
                    continue;
                }
                let value = match settings.get(&f.key) {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => String::new(),
                };
                let secret = f.kind == SettingKind::Secret;
                let state = cx.new(|cx| {
                    InputState::new(window, cx)
                        .masked(secret)
                        .default_value(value)
                });
                let key = f.key.clone();
                let number = f.kind == SettingKind::Number;
                self._subs
                    .push(cx.subscribe(&state, move |this, s, ev: &InputEvent, cx| {
                        if let InputEvent::Change = ev
                            && let Some(d) = this.draft.as_mut()
                        {
                            let v = s.read(cx).value().to_string();
                            let v = match (number, v.trim().parse::<f64>()) {
                                (true, Ok(n)) => serde_json::json!(n),
                                _ => Value::String(v),
                            };
                            d.settings.insert(key.clone(), v);
                            cx.notify();
                        }
                    }));
                inputs.push((f.key.clone(), state));
            }
            self.draft = Some(Draft {
                id: c.descriptor.id.clone(),
                settings,
                inputs,
            });
        }
        cx.notify();
    }

    /// Saves the draft and turns the connector on or off.
    fn write(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let Some(d) = &self.draft else { return };
        let (id, settings) = (d.id.clone(), d.settings.clone());
        self.busy = true;
        self.error = None;
        let b = backend(cx);
        let task = crate::runtime::spawn(async move {
            let mut cfg = b.config_masked().await;
            let c = cfg.connectors.entry(id).or_default();
            c.enabled = enabled;
            c.settings = settings;
            b.config_save(cfg).await
        });
        cx.spawn(async move |this, cx| {
            let r = task.await.and_then(|r| r);
            this.update(cx, |this, cx| {
                this.busy = false;
                if let Err(e) = r {
                    this.error = Some(e);
                }
                this.reload(cx);
            })
            .ok();
        })
        .detach();
    }

    fn mark(&self, c: &ConnectorInfo, px_size: f32, cx: &App) -> impl IntoElement {
        let t = theme(cx);
        let name = if crate::assets::has_icon(&c.descriptor.icon) {
            c.descriptor.icon.clone()
        } else {
            crate::work::model::APPS
                .iter()
                .find(|(id, _, _)| *id == c.descriptor.id)
                .map_or("plug", |a| a.2)
                .to_string()
        };
        div()
            .size(px(px_size))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(size::RADIUS_MD))
            .border_1()
            .border_color(t.border)
            .bg(t.bg_raised)
            .child(if c.descriptor.id == "tab" {
                crate::components::tab_mark(px_size * 0.6).into_any_element()
            } else {
                icon(&name, px_size * 0.55, t.fg).into_any_element()
            })
    }

    fn status(&self, c: &ConnectorInfo, cx: &App) -> impl IntoElement {
        let t = theme(cx);
        let (tone, text) = status_text(&c.status, c.descriptor.available, chrono::Utc::now());
        let tone = tone_of(t, tone);
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(size::TEXT_SM))
            .text_color(t.fg_muted)
            .child(div().size(px(7.)).flex_none().rounded_full().bg(tone.fg))
            .child(div().min_w_0().truncate().child(text))
    }

    fn render_list(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme(cx).clone();
        let installed: Vec<_> = self.connectors.iter().filter(|c| c.enabled).collect();
        let market = self.connectors.iter().filter(|c| !c.enabled).count();
        let mut tabs = div()
            .flex()
            .gap(px(16.))
            .border_b_1()
            .border_color(t.border);
        for (view, label, count) in [
            (View::Installed, "Installed", installed.len()),
            (View::Marketplace, "Marketplace", market),
        ] {
            let active = self.view == view;
            tabs = tabs.child(
                div()
                    .id(label)
                    .flex()
                    .gap(px(6.))
                    .py(px(10.))
                    .cursor_pointer()
                    .text_size(px(size::TEXT_MD))
                    .border_b_2()
                    .border_color(if active {
                        t.accent.fg
                    } else {
                        gpui::transparent_black()
                    })
                    .text_color(if active { t.fg } else { t.fg_muted })
                    .child(label)
                    .child(div().text_color(t.fg_subtle).child(count.to_string()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.view = view;
                        cx.notify();
                    })),
            );
        }
        let body = match self.view {
            View::Installed => {
                let mut list = div()
                    .flex()
                    .flex_col()
                    .rounded(px(size::RADIUS_LG))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.bg_raised);
                for (n, c) in installed.into_iter().enumerate() {
                    let id = c.descriptor.id.clone();
                    list = list.child(
                        div()
                            .id(SharedString::from(format!("conn-{id}")))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .px(px(16.))
                            .py(px(12.))
                            .cursor_pointer()
                            .hover(|s| s.bg(t.bg_sunken))
                            .when(n > 0, |el| el.border_t_1().border_color(t.border))
                            .child(self.mark(c, 34., cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .flex()
                                            .gap(px(8.))
                                            .text_size(px(size::TEXT_MD))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .child(c.descriptor.name.clone())
                                            .when(c.descriptor.builtin, |el| {
                                                el.child(badge("Built in", t.fg_muted, t.bg_sunken))
                                            }),
                                    )
                                    .child(self.status(c, cx)),
                            )
                            .child(div().flex().gap(px(6.)).children(
                                c.descriptor.kinds.iter().take(3).map(|k| {
                                    let tone = t.tone(k.tone);
                                    badge(k.label.clone(), tone.fg, tone.bg)
                                }),
                            ))
                            .child(icon("chevron-right", 16., t.fg_subtle))
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.open(Some(id.clone()), w, cx)
                            })),
                    );
                }
                list.into_any_element()
            }
            View::Marketplace => {
                let mut shelves = div().flex().flex_col().gap(px(24.));
                for (shelf, label) in SHELVES {
                    let items: Vec<_> = self
                        .connectors
                        .iter()
                        .filter(|c| {
                            !c.enabled
                                && (if c.descriptor.category.is_empty() {
                                    "built_in"
                                } else {
                                    &c.descriptor.category
                                }) == shelf
                        })
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    let mut grid = div().flex().flex_wrap().gap(px(12.));
                    for c in items {
                        grid = grid.child(self.market_card(c, cx));
                    }
                    shelves = shelves.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(
                                div()
                                    .text_size(px(size::TEXT_LG))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(label),
                            )
                            .child(grid),
                    );
                }
                shelves.into_any_element()
            }
        };
        div()
            .id("connections")
            .size_full()
            .overflow_y_scroll()
            .bg(t.bg)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .max_w(px(960.))
                    .px(px(40.))
                    .py(px(28.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_size(px(size::TEXT_2XL))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Connections"),
                            )
                            .child(
                                div()
                                    .text_size(px(size::TEXT_MD))
                                    .text_color(t.fg_muted)
                                    .child("Each connection can bring work into your queue, give agents tools to act there, or both."),
                            ),
                    )
                    .child(tabs)
                    .child(body),
            )
            .into_any_element()
    }

    fn market_card(&self, c: &ConnectorInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let d = &c.descriptor;
        let how = if !d.available {
            "Coming soon"
        } else if d.id == "github" {
            "Sign in with GitHub"
        } else if d.id == "email" {
            "Sign in with IMAP"
        } else if d.settings.iter().any(|s| s.required) {
            "Needs a token"
        } else {
            "One click"
        };
        let id = d.id.clone();
        div()
            .w(px(280.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(14.))
            .rounded(px(size::RADIUS_LG))
            .border_1()
            .border_color(t.border)
            .bg(t.bg_raised)
            .when(!d.available, |el| el.opacity(0.6))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(self.mark(c, 34., cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(size::TEXT_MD))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(d.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(size::TEXT_SM))
                                    .text_color(t.fg_subtle)
                                    .child(how),
                            ),
                    )
                    .when(d.available, |el| {
                        el.child(
                            Button::new(SharedString::from(format!("install-{id}")))
                                .kind(ButtonKind::Primary)
                                .label("Install")
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    this.open(Some(id.clone()), w, cx)
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_muted)
                    .child(d.description.clone()),
            )
    }

    fn render_detail(&self, c: &ConnectorInfo, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme(cx).clone();
        let d = &c.descriptor;
        let draft = self.draft.as_ref();
        let missing = d.settings.iter().any(|f| {
            f.required
                && !hidden(&d.id, &f.key)
                && draft
                    .and_then(|dr| dr.settings.get(&f.key))
                    .map(|v| match v {
                        Value::String(s) => s.trim().is_empty(),
                        Value::Null => true,
                        _ => false,
                    })
                    .unwrap_or(true)
        });
        let section = |title: &'static str, desc: &'static str, body: gpui::AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(size::TEXT_LG))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .when(!desc.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(size::TEXT_MD))
                            .text_color(t.fg_muted)
                            .child(desc),
                    )
                })
                .child(body)
        };
        let mut page = div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .max_w(px(760.))
            .px(px(40.))
            .py(px(24.))
            .child(
                div().child(
                    Button::new("back")
                        .kind(ButtonKind::Ghost)
                        .icon("arrow-left")
                        .label("Connections")
                        .on_click(cx.listener(|this, _, w, cx| this.open(None, w, cx))),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(self.mark(c, 48., cx))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_size(px(size::TEXT_2XL))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(d.name.clone()),
                            )
                            .child(self.status(c, cx)),
                    )
                    .when(d.available && !d.builtin, |el| {
                        el.child(if c.enabled {
                            Button::new("uninstall")
                                .label("Uninstall")
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, _, cx| this.write(false, cx)))
                        } else {
                            Button::new("install")
                                .kind(ButtonKind::Primary)
                                .label(if self.busy {
                                    "Installing…"
                                } else {
                                    "Install"
                                })
                                .disabled(self.busy || missing)
                                .on_click(cx.listener(|this, _, _, cx| this.write(true, cx)))
                        })
                    }),
            )
            .child(
                div()
                    .text_size(px(size::TEXT_MD))
                    .text_color(t.fg_muted)
                    .child(d.description.clone()),
            )
            .when_some(self.error.clone(), |el, e| {
                el.child(div().text_color(t.danger.fg).child(e))
            });
        if !d.features.is_empty() {
            page = page.child(section(
                "What it adds",
                "",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .children(d.features.iter().map(|f| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(size::TEXT_MD))
                            .child(icon("check", 13., t.success.fg))
                            .child(f.clone())
                    }))
                    .into_any_element(),
            ));
        }
        let fields: Vec<_> = d
            .settings
            .iter()
            .filter(|f| !hidden(&d.id, &f.key))
            .cloned()
            .collect();
        if !fields.is_empty()
            && let Some(draft) = draft
        {
            let mut card = div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .p(px(16.))
                .rounded(px(size::RADIUS_LG))
                .border_1()
                .border_color(t.border)
                .bg(t.bg_raised);
            for f in fields {
                let control = if f.kind == SettingKind::Toggle {
                    let key = f.key.clone();
                    let on = draft.settings.get(&f.key).and_then(Value::as_bool) == Some(true);
                    let this = cx.entity().downgrade();
                    Switch::new(SharedString::from(format!("t-{key}")))
                        .checked(on)
                        .on_click(move |v, _, cx| {
                            let v = *v;
                            this.update(cx, |this, cx| {
                                if let Some(d) = this.draft.as_mut() {
                                    d.settings.insert(key.clone(), Value::Bool(v));
                                }
                                cx.notify();
                            })
                            .ok();
                        })
                        .into_any_element()
                } else {
                    match draft.inputs.iter().find(|(k, _)| *k == f.key) {
                        Some((_, state)) => Input::new(state).into_any_element(),
                        None => div().into_any_element(),
                    }
                };
                card = card.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(px(size::TEXT_MD))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(f.label.clone()),
                        )
                        .child(control)
                        .when_some(f.help.clone(), |el, h| {
                            el.child(
                                div()
                                    .text_size(px(size::TEXT_SM))
                                    .text_color(t.fg_subtle)
                                    .child(h),
                            )
                        }),
                );
            }
            card = card.child(
                div().flex().child(
                    Button::new("save-conn")
                        .kind(ButtonKind::Primary)
                        .label(if c.enabled {
                            "Save"
                        } else {
                            "Save and install"
                        })
                        .disabled(self.busy || missing)
                        .on_click(cx.listener(|this, _, _, cx| this.write(true, cx))),
                ),
            );
            page = page.child(section("Settings", "", card.into_any_element()));
        }
        if !d.kinds.is_empty() {
            page = page.child(section(
                "Kinds of work",
                "What lands in your queue from this connection. Scores are set in Settings → Ranking.",
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .children(d.kinds.iter().map(|k| {
                        let tone = t.tone(k.tone);
                        badge(k.label.clone(), tone.fg, tone.bg)
                    }))
                    .into_any_element(),
            ));
        }
        if !d.rate_limits.is_empty() {
            page = page.child(section(
                "Rate budget",
                "Declared limits. tab budgets against these and backs off background refreshes when headroom is low.",
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(10.))
                    .children(d.rate_limits.iter().map(|l| {
                        div()
                            .w(px(220.))
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .p(px(12.))
                            .rounded(px(size::RADIUS_LG))
                            .border_1()
                            .border_color(t.border)
                            .bg(t.bg_raised)
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .text_size(px(size::TEXT_MD))
                                    .child(
                                        div()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .child(l.resource.clone()),
                                    )
                                    .child(l.requests.to_string()),
                            )
                            .child(
                                div()
                                    .text_size(px(size::TEXT_SM))
                                    .text_color(t.fg_subtle)
                                    .child(format!(
                                        "{} · burst over {} in 10s cools down",
                                        per(l.per_seconds),
                                        l.burst
                                    )),
                            )
                    }))
                    .into_any_element(),
            ));
        }
        div()
            .id("connection")
            .size_full()
            .overflow_y_scroll()
            .bg(t.bg)
            .child(page)
            .into_any_element()
    }
}

/// Fields that have their own sign-in card in the Tauri app: GitHub's
/// token. Shown here as a plain field until that card is ported.
fn hidden(_connector: &str, _key: &str) -> bool {
    false
}

use gpui::App;

impl Render for ConnectionsTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self
            .open
            .as_ref()
            .and_then(|id| self.connectors.iter().find(|c| c.descriptor.id == *id))
            .cloned();
        match open {
            Some(c) => self.render_detail(&c, cx),
            None => self.render_list(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_read_naturally() {
        assert_eq!(per(3600), "per 1 hour");
        assert_eq!(per(900), "per 15 min");
        assert_eq!(per(10), "per 10s");
    }

    #[test]
    fn unavailable_reads_coming_soon() {
        let (tone, text) = status_text(&ConnectorStatus::Disabled, false, chrono::Utc::now());
        assert_eq!((tone, text.as_str()), ("neutral", "Coming soon"));
    }
}
