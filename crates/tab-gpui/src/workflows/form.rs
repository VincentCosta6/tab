//! The builder's form pieces: labeled fields, text boxes that take
//! `{{variables}}`, selects, chips, conditions and key/value pairs. The
//! counterparts of `Field`, `VarInput`, `InputField`, `Conditions` and
//! `Pairs` in `WorkflowsTab.tsx`.
//!
//! Text boxes are kept between renders in [`Fields`], keyed by where they
//! are in the form, so typing keeps its cursor. Each writes into the draft
//! through a setter as the person types. When the draft's shape changes
//! (`restructure`), the boxes are rebuilt from it.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, AppContext, Context, Entity, IntoElement, ParentElement, SharedString, Styled,
    Subscription, Window, div, prelude::*, px,
};
use gpui_component::Sizable;
use gpui_component::button::{Button as GButton, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::switch::Switch;
use serde_json::{Map, Value};
use tab_core::workflow::{Condition, Op, Workflow};

use super::WorkflowsTab;
use super::format::{OPS, op_takes_value, text};
use crate::components::icon;
use crate::theme::{FONT_MONO, Theme, size, theme};

/// A way into the draft: the part a field edits, if it's still there.
pub type Lens<T> = Rc<dyn Fn(&mut Workflow) -> Option<&mut T>>;

/// Makes a [`Lens`] from a closure, so its lifetimes come out right.
pub fn lens<T: 'static, F>(f: F) -> Lens<T>
where
    F: for<'a> Fn(&'a mut Workflow) -> Option<&'a mut T> + 'static,
{
    Rc::new(f)
}

type Setter = Rc<dyn Fn(&mut Workflow, String)>;
type Edit = dyn Fn(&mut Workflow);

#[derive(Clone)]
enum Boxed {
    Line(Entity<InputState>),
    Area(Entity<TextareaState>),
}

impl Boxed {
    fn insert(&self, s: String, window: &mut Window, cx: &mut gpui::App) {
        match self {
            Boxed::Line(e) => e.update(cx, |st, cx| st.insert(s, window, cx)),
            Boxed::Area(e) => e.update(cx, |st, cx| st.insert(s, window, cx)),
        }
    }
}

/// The form's text boxes, by key.
#[derive(Default)]
pub struct Fields {
    map: HashMap<String, (Boxed, Subscription)>,
}

impl Fields {
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

/// How a text box writes back.
#[derive(Clone, Copy, PartialEq)]
pub enum Commit {
    /// On every change.
    Live,
    /// When the person leaves it or presses Enter, rebuilding the form:
    /// for names other fields are keyed by.
    OnLeave,
}

/// What a text box looks like.
#[derive(Clone, Copy, Default)]
pub struct Look {
    pub multiline: bool,
    pub mono: bool,
}

pub const LINE: Look = Look {
    multiline: false,
    mono: false,
};
pub const MONO: Look = Look {
    multiline: false,
    mono: true,
};
pub const AREA: Look = Look {
    multiline: true,
    mono: false,
};
pub const AREA_MONO: Look = Look {
    multiline: true,
    mono: true,
};

/// A label over a control, with help under it.
pub fn field(
    t: &Theme,
    label: impl Into<SharedString>,
    help: Option<&str>,
    control: impl IntoElement,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .min_w_0()
        .child(
            div()
                .text_size(px(size::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(t.fg_muted)
                .child(label.into()),
        )
        .child(control)
        .when_some(help.map(str::to_string), |el, h| {
            el.child(
                div()
                    .text_size(px(size::TEXT_XS))
                    .text_color(t.fg_subtle)
                    .child(h),
            )
        })
        .into_any_element()
}

/// Two fields side by side.
pub fn row2(a: AnyElement, b: AnyElement) -> AnyElement {
    div()
        .flex()
        .gap(px(10.))
        .child(div().flex_1().min_w_0().child(a))
        .child(div().flex_1().min_w_0().child(b))
        .into_any_element()
}

/// A section heading in the inspector.
pub fn section(t: &Theme, s: &str) -> AnyElement {
    div()
        .mt(px(10.))
        .pt(px(12.))
        .border_t_1()
        .border_color(t.border)
        .text_size(px(size::TEXT_MD))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(t.fg)
        .child(s.to_string())
        .into_any_element()
}

/// Quiet words under a form.
pub fn note(t: &Theme, s: impl Into<SharedString>) -> AnyElement {
    div()
        .text_size(px(size::TEXT_SM))
        .text_color(t.fg_subtle)
        .child(s.into())
        .into_any_element()
}

/// `{{steps.x.y}}` paths in a row, for "Later steps can use".
pub fn outputs(t: &Theme, paths: Vec<String>) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(4.))
        .text_size(px(size::TEXT_SM))
        .text_color(t.fg_subtle)
        .child("Later steps can use")
        .children(paths.into_iter().map(|p| {
            div()
                .px(px(5.))
                .py(px(1.))
                .rounded(px(size::RADIUS_SM))
                .bg(t.bg_sunken)
                .font_family(FONT_MONO)
                .text_size(px(size::TEXT_XS))
                .text_color(t.fg_muted)
                .child(format!("{{{{{p}}}}}"))
        }))
        .into_any_element()
}

/// A setting with words on the left and a switch on the right.
pub fn safety(
    t: &Theme,
    id: impl Into<SharedString>,
    title: &str,
    body: String,
    on: bool,
    toggle: impl Fn(&bool, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(px(10.))
        .p(px(10.))
        .rounded(px(size::RADIUS_MD))
        .border_1()
        .border_color(if on { t.warning.fg } else { t.border })
        .when(on, |el| el.bg(t.warning.bg))
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
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(px(size::TEXT_SM))
                        .text_color(t.fg_muted)
                        .child(body),
                ),
        )
        .child(Switch::new(id.into()).checked(on).small().on_click(toggle))
        .into_any_element()
}

/// A toggle chip: on is filled, off is outlined and faded.
pub fn toggle_chip(
    t: &Theme,
    id: impl Into<SharedString>,
    label: String,
    glyph: Option<&str>,
    on: bool,
    cx: &mut Context<WorkflowsTab>,
    toggle: impl Fn(&mut Workflow) + 'static,
) -> AnyElement {
    let id: SharedString = id.into();
    div()
        .id(gpui::ElementId::Name(id))
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(8.))
        .py(px(3.))
        .rounded_full()
        .border_1()
        .cursor_pointer()
        .text_size(px(size::TEXT_SM))
        .when(on, |el| {
            el.bg(t.accent.bg)
                .border_color(t.accent.fg)
                .text_color(t.accent.fg)
        })
        .when(!on, |el| {
            el.border_color(t.border)
                .text_color(t.fg_subtle)
                .hover(|s| s.text_color(t.fg).border_color(t.border_strong))
        })
        .when_some(glyph.map(str::to_string), |el, g| {
            el.child(icon(&g, 11., if on { t.accent.fg } else { t.fg_subtle }))
        })
        .child(label)
        .on_click(cx.listener(move |this, _, _, cx| this.restructure(&toggle, cx)))
        .into_any_element()
}

/// A plain chip that sets something when clicked.
pub fn action_chip(
    t: &Theme,
    id: impl Into<SharedString>,
    label: String,
    cx: &mut Context<WorkflowsTab>,
    set: impl Fn(&mut Workflow) + 'static,
) -> AnyElement {
    let id: SharedString = id.into();
    div()
        .id(gpui::ElementId::Name(id))
        .px(px(8.))
        .py(px(3.))
        .rounded_full()
        .border_1()
        .border_color(t.border)
        .text_size(px(size::TEXT_SM))
        .text_color(t.fg_muted)
        .cursor_pointer()
        .hover(|s| s.bg(t.bg_hover).text_color(t.fg))
        .child(label)
        .on_click(cx.listener(move |this, _, _, cx| this.restructure(&set, cx)))
        .into_any_element()
}

/// One choice in a [`WorkflowsTab::select`].
pub struct Choice {
    pub label: String,
    /// A heading before it, starting a group.
    pub group: Option<String>,
    pub set: Rc<Edit>,
}

impl Choice {
    pub fn new(label: impl Into<String>, set: impl Fn(&mut Workflow) + 'static) -> Self {
        Self {
            label: label.into(),
            group: None,
            set: Rc::new(set),
        }
    }
    pub fn group(mut self, g: impl Into<String>) -> Self {
        self.group = Some(g.into());
        self
    }
}

impl WorkflowsTab {
    /// A text box kept across renders. `set` writes its text into the draft.
    #[allow(clippy::too_many_arguments)]
    fn text_box(
        &mut self,
        key: &str,
        value: &str,
        look: Look,
        placeholder: &str,
        commit: Commit,
        set: Setter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Boxed {
        if let Some((b, _)) = self.fields.map.get(key) {
            return b.clone();
        }
        let placeholder = placeholder.to_string();
        let value = value.to_string();
        let on_event = move |this: &mut Self,
                             v: String,
                             ev: &InputEvent,
                             cx: &mut Context<Self>| match (commit, ev) {
            (Commit::Live, InputEvent::Change) => {
                let set = set.clone();
                this.edit(move |w| set(w, v), cx);
            }
            (Commit::OnLeave, InputEvent::Blur | InputEvent::PressEnter { .. }) => {
                let set = set.clone();
                this.restructure(&move |w: &mut Workflow| set(w, v.clone()), cx);
            }
            _ => {}
        };
        let (boxed, sub) = if look.multiline {
            let e = cx.new(|cx| {
                let mut s = TextareaState::new(window, cx)
                    .auto_grow(3, 16)
                    .placeholder(placeholder);
                s.set_value(value, window, cx);
                s
            });
            let sub = cx.subscribe(&e, move |this, s, ev: &InputEvent, cx| {
                let v = s.read(cx).value().to_string();
                on_event(this, v, ev, cx)
            });
            (Boxed::Area(e), sub)
        } else {
            let e = cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder);
                s.set_value(value, window, cx);
                s
            });
            let sub = cx.subscribe(&e, move |this, s, ev: &InputEvent, cx| {
                let v = s.read(cx).value().to_string();
                on_event(this, v, ev, cx)
            });
            (Boxed::Line(e), sub)
        };
        self.fields
            .map
            .insert(key.to_string(), (boxed.clone(), sub));
        boxed
    }

    fn render_box(&self, b: &Boxed, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let el = match b {
            Boxed::Line(e) => Input::new(e).small().into_any_element(),
            Boxed::Area(e) => Textarea::new(e).into_any_element(),
        };
        div()
            .flex_1()
            .min_w_0()
            .text_size(px(size::TEXT_MD))
            .text_color(t.fg)
            .when(look.mono, |el| el.font_family(FONT_MONO))
            .child(el)
            .into_any_element()
    }

    /// A text box.
    #[allow(clippy::too_many_arguments)]
    pub fn input(
        &mut self,
        key: &str,
        value: &str,
        look: Look,
        placeholder: &str,
        commit: Commit,
        set: impl Fn(&mut Workflow, String) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let b = self.text_box(
            key,
            value,
            look,
            placeholder,
            commit,
            Rc::new(set),
            window,
            cx,
        );
        self.render_box(&b, look, cx)
    }

    /// A number box: anything that isn't a number is ignored.
    #[allow(clippy::too_many_arguments)]
    pub fn number(
        &mut self,
        key: &str,
        value: u32,
        min: u32,
        max: u32,
        set: impl Fn(&mut Workflow, u32) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.input(
            key,
            &value.to_string(),
            LINE,
            "",
            Commit::Live,
            move |w, s| {
                if let Ok(n) = s.trim().parse::<u32>() {
                    set(w, n.clamp(min, max));
                }
            },
            window,
            cx,
        )
    }

    /// A text box that takes `{{variables}}`, with a menu to insert one.
    #[allow(clippy::too_many_arguments)]
    pub fn var_input(
        &mut self,
        key: &str,
        value: &str,
        look: Look,
        placeholder: &str,
        vars: &[(String, String)],
        set: impl Fn(&mut Workflow, String) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let b = self.text_box(
            key,
            value,
            look,
            placeholder,
            Commit::Live,
            Rc::new(set),
            window,
            cx,
        );
        let vars = vars.to_vec();
        let target = b.clone();
        let menu = GButton::new(SharedString::from(format!("var-{key}")))
            .ghost()
            .xsmall()
            .label("{ }")
            .tooltip("Insert a value from the item or an earlier step")
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.max_h(px(320.)).scrollable(true);
                for (path, label) in &vars {
                    let (target, token) = (target.clone(), format!("{{{{{path}}}}}"));
                    menu = menu.item(PopupMenuItem::new(format!("{label}   {path}")).on_click(
                        move |_, window, cx| {
                            target.insert(token.clone(), window, cx);
                        },
                    ));
                }
                menu
            });
        div()
            .flex()
            .items_start()
            .gap(px(4.))
            .child(self.render_box(&b, look, cx))
            .child(menu)
            .into_any_element()
    }

    /// A dropdown of choices; picking one rebuilds the form.
    pub fn select(
        &self,
        id: impl Into<SharedString>,
        current: String,
        choices: Vec<Choice>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let this = cx.entity().downgrade();
        let choices: Vec<(Option<String>, String, Rc<Edit>)> = choices
            .into_iter()
            .map(|c| (c.group, c.label, c.set))
            .collect();
        GButton::new(id.into())
            .outline()
            .small()
            .w_full()
            .label(current)
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.max_h(px(360.)).scrollable(true);
                for (group, label, set) in &choices {
                    if let Some(g) = group {
                        menu = menu.label(g.clone());
                    }
                    let (this, set) = (this.clone(), set.clone());
                    menu =
                        menu.item(PopupMenuItem::new(label.clone()).on_click(move |_, _, cx| {
                            let set = set.clone();
                            this.update(cx, |v, cx| {
                                v.restructure(&move |w: &mut Workflow| set(w), cx)
                            })
                            .ok();
                        }));
                }
                menu
            })
            .into_any_element()
    }

    /// One action input, as its descriptor asks: a choice, yes/no, or text
    /// that takes variables. A templated value always shows as text.
    #[allow(clippy::too_many_arguments)]
    pub fn action_input(
        &mut self,
        key: &str,
        f: &tab_core::model::ActionField,
        value: &Value,
        vars: &[(String, String)],
        at: Lens<Map<String, Value>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use tab_core::model::FieldType;
        let t = theme(cx).clone();
        let v = text(value);
        let templated = v.contains("{{");
        let k = f.key.clone();
        let put = move |at: &Lens<Map<String, Value>>, w: &mut Workflow, nv: Value| {
            if let Some(m) = at(w) {
                if nv.is_null() || nv.as_str() == Some("") {
                    m.remove(&k);
                } else {
                    m.insert(k.clone(), nv);
                }
            }
        };
        let control = match f.ty {
            FieldType::Select if !templated => {
                let mut choices = Vec::new();
                if !f.required {
                    let (at, put) = (at.clone(), put.clone());
                    choices.push(Choice::new("Default", move |w| put(&at, w, Value::Null)));
                }
                for o in &f.options {
                    let (at, put, o2) = (at.clone(), put.clone(), o.clone());
                    choices.push(Choice::new(o.clone(), move |w| {
                        put(&at, w, Value::String(o2.clone()))
                    }));
                }
                self.select(
                    format!("sel-{key}"),
                    if v.is_empty() {
                        "Default".into()
                    } else {
                        v.clone()
                    },
                    choices,
                    cx,
                )
            }
            FieldType::Bool if !templated => {
                let mut choices = Vec::new();
                for (label, nv) in [
                    ("Default", Value::Null),
                    ("Yes", Value::Bool(true)),
                    ("No", Value::Bool(false)),
                ] {
                    let (at, put) = (at.clone(), put.clone());
                    choices.push(Choice::new(label, move |w| put(&at, w, nv.clone())));
                }
                let current = match value {
                    Value::Bool(true) => "Yes",
                    Value::Bool(false) => "No",
                    _ => "Default",
                };
                self.select(format!("sel-{key}"), current.into(), choices, cx)
            }
            _ => {
                let placeholder = if f.ty == FieldType::List {
                    "a, b, c".to_string()
                } else {
                    f.default.clone().unwrap_or_default()
                };
                let look = if f.ty == FieldType::LongText {
                    AREA
                } else {
                    LINE
                };
                let at = at.clone();
                self.var_input(
                    key,
                    &v,
                    look,
                    &placeholder,
                    vars,
                    move |w, s| put(&at, w, Value::String(s)),
                    window,
                    cx,
                )
            }
        };
        field(
            &t,
            format!("{}{}", f.label, if f.required { "" } else { " (optional)" }),
            f.help.as_deref(),
            control,
        )
    }

    /// Conditions that must all hold.
    pub fn conditions(
        &mut self,
        key: &str,
        conds: &[Condition],
        vars: &[(String, String)],
        at: Lens<Vec<Condition>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let mut out = div().flex().flex_col().gap(px(6.));
        for (j, c) in conds.iter().enumerate() {
            let a = at.clone();
            let field_box = self.var_input(
                &format!("{key}/c{j}/field"),
                &c.field,
                MONO,
                "item.title",
                vars,
                move |w, s| {
                    if let Some(c) = a(w).and_then(|v| v.get_mut(j)) {
                        // The menu inserts `{{path}}`; a condition wants the bare path.
                        c.field = s.replace("{{", "").replace("}}", "");
                    }
                },
                window,
                cx,
            );
            let ops: Vec<Choice> = OPS
                .iter()
                .map(|(op, label, _)| {
                    let (a, op) = (at.clone(), *op);
                    Choice::new(*label, move |w| {
                        if let Some(c) = a(w).and_then(|v| v.get_mut(j)) {
                            c.op = op;
                        }
                    })
                })
                .collect();
            let op = self.select(
                format!("{key}/c{j}/op"),
                super::format::op_label(c.op).to_string(),
                ops,
                cx,
            );
            let value = op_takes_value(c.op).then(|| {
                let a = at.clone();
                self.input(
                    &format!("{key}/c{j}/value"),
                    &text(&c.value),
                    LINE,
                    "value",
                    Commit::Live,
                    move |w, s| {
                        if let Some(c) = a(w).and_then(|v| v.get_mut(j)) {
                            c.value = Value::String(s);
                        }
                    },
                    window,
                    cx,
                )
            });
            let a = at.clone();
            out = out.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(4.))
                    .child(div().flex_1().min_w_0().child(field_box))
                    .child(div().w(px(110.)).flex_none().child(op))
                    .when_some(value, |el, v| el.child(div().flex_1().min_w_0().child(v)))
                    .child(remove_button(&t, format!("{key}/c{j}/x"), cx, move |w| {
                        if let Some(v) = a(w)
                            && j < v.len()
                        {
                            v.remove(j);
                        }
                    })),
            );
        }
        let first = vars
            .first()
            .map_or("item.title".to_string(), |v| v.0.clone());
        let a = at.clone();
        out = out.child(add_button(
            &t,
            format!("{key}/add"),
            "Add condition",
            cx,
            move |w| {
                if let Some(v) = a(w) {
                    v.push(Condition {
                        field: first.clone(),
                        op: Op::Contains,
                        value: Value::String(String::new()),
                    });
                }
            },
        ));
        if conds.len() > 1 {
            out = out.child(note(&t, "All of these must hold."));
        }
        out.into_any_element()
    }

    /// Names and values, such as a Set step's values or a signal's payload.
    #[allow(clippy::too_many_arguments)]
    pub fn pairs(
        &mut self,
        key: &str,
        map: &Map<String, Value>,
        vars: &[(String, String)],
        key_label: &str,
        at: Lens<Map<String, Value>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let mut out = div().flex().flex_col().gap(px(6.));
        for (j, (k, v)) in map.iter().enumerate() {
            let (a, old) = (at.clone(), k.clone());
            // Renaming re-sorts the pairs, so it lands when the box is left.
            let name = self.input(
                &format!("{key}/p{j}/k"),
                k,
                MONO,
                key_label,
                Commit::OnLeave,
                move |w, s| {
                    let new = sanitize_id(&s);
                    if let Some(m) = a(w)
                        && !new.is_empty()
                        && new != old
                        && !m.contains_key(&new)
                        && let Some(v) = m.remove(&old)
                    {
                        m.insert(new, v);
                    }
                },
                window,
                cx,
            );
            let (a, kk) = (at.clone(), k.clone());
            let value = self.var_input(
                &format!("{key}/p{j}/v"),
                &text(v),
                LINE,
                "",
                vars,
                move |w, s| {
                    if let Some(m) = a(w) {
                        m.insert(kk.clone(), Value::String(s));
                    }
                },
                window,
                cx,
            );
            let (a, kk) = (at.clone(), k.clone());
            out = out.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(4.))
                    .child(div().w(px(120.)).flex_none().child(name))
                    .child(div().flex_1().min_w_0().child(value))
                    .child(remove_button(&t, format!("{key}/p{j}/x"), cx, move |w| {
                        if let Some(m) = a(w) {
                            m.remove(&kk);
                        }
                    })),
            );
        }
        let a = at.clone();
        out = out.child(add_button(&t, format!("{key}/add"), "Add", cx, move |w| {
            if let Some(m) = a(w) {
                let mut n = m.len() + 1;
                while m.contains_key(&format!("value_{n}")) {
                    n += 1;
                }
                m.insert(format!("value_{n}"), Value::String(String::new()));
            }
        }));
        out.into_any_element()
    }
}

/// Letters, digits and `_`; anything else becomes `_`.
pub fn sanitize_id(s: &str) -> String {
    s.trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn remove_button(
    t: &Theme,
    id: String,
    cx: &mut Context<WorkflowsTab>,
    f: impl Fn(&mut Workflow) + 'static,
) -> AnyElement {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .flex_none()
        .mt(px(4.))
        .p(px(4.))
        .rounded(px(size::RADIUS_SM))
        .cursor_pointer()
        .hover(|s| s.bg(t.bg_hover))
        .child(icon("x", 12., t.fg_subtle))
        .on_click(cx.listener(move |this, _, _, cx| this.restructure(&f, cx)))
        .into_any_element()
}

fn add_button(
    t: &Theme,
    id: String,
    label: &str,
    cx: &mut Context<WorkflowsTab>,
    f: impl Fn(&mut Workflow) + 'static,
) -> AnyElement {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .flex()
        .items_center()
        .gap(px(4.))
        .text_size(px(size::TEXT_SM))
        .text_color(t.accent.fg)
        .cursor_pointer()
        .hover(|s| s.text_color(t.fg))
        .child(icon("plus", 12., t.accent.fg))
        .child(label.to_string())
        .on_click(cx.listener(move |this, _, _, cx| this.restructure(&f, cx)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_keep_to_letters_digits_and_underscores() {
        assert_eq!(sanitize_id(" pr-url "), "pr_url");
        assert_eq!(sanitize_id("ok_2"), "ok_2");
    }

    #[test]
    fn a_lens_reaches_into_the_draft() {
        let l = lens(|w: &mut Workflow| Some(&mut w.description));
        let mut w = Workflow::default();
        *l(&mut w).unwrap() = "hi".into();
        assert_eq!(w.description, "hi");
    }
}
