//! Projects: each with a coordinator (one long-running chat that knows the
//! project), its threads, notes, repos and instructions. The counterpart of
//! `src/modules/projects/ProjectsTab.tsx`.
//!
//! Resources and the knowledge view, the vault, compaction history and the
//! coordinator's watch and brief settings are still only in the Tauri app.

use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, Hsla, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::*, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use tab_core::EngineEvent;
use tab_core::model::{ItemState, Project, ProjectNote, ProjectRepo, QueueItem};

use crate::backend::backend;
use crate::chat::{ChatView, Target};
use crate::components::{Button, ButtonKind, ago, icon};
use crate::theme::{FONT_MONO, size, theme};
use crate::work::item::ItemPane;

/// The palette new projects take colors from, in order (as `projectColors`).
const COLORS: [u32; 10] = [
    0x7c5cff, 0x3b82f6, 0x06b6d4, 0x10b981, 0x84cc16, 0xf59e0b, 0xf97316, 0xe5484d, 0xec4899,
    0x8b8d98,
];

/// `#rrggbb` as a color, grey if it isn't one.
pub fn color_of(hex: &str) -> Hsla {
    let n = hex
        .strip_prefix('#')
        .and_then(|h| u32::from_str_radix(h, 16).ok())
        .unwrap_or(0x8b8d98);
    rgb(n).into()
}

/// A project's icon on a tint of its color.
pub fn mark(p: &Project, size_px: f32) -> impl IntoElement {
    let c = color_of(&p.color);
    div()
        .size(px(size_px))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(size::RADIUS_MD))
        .bg(c.opacity(0.18))
        .child(icon(
            if p.icon.is_empty() { "folder" } else { &p.icon },
            size_px * 0.58,
            c,
        ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Home,
    Notes,
    Settings,
}

#[derive(Clone, PartialEq, Eq)]
enum Focus {
    Coordinator,
    Item(String),
}

pub struct ProjectsTab {
    projects: Vec<Project>,
    selected: Option<String>,
    view: View,
    focus: Focus,
    items: Vec<QueueItem>,
    notes: Vec<ProjectNote>,
    repos: Vec<ProjectRepo>,
    show_done: bool,
    coordinator: Option<Entity<ChatView>>,
    item: Entity<ItemPane>,
    creating: bool,
    error: Option<String>,
    new_name: Entity<InputState>,
    note_topic: Entity<InputState>,
    note_text: Entity<TextareaState>,
    editing_note: Option<String>,
    about: Entity<TextareaState>,
    rules: Entity<TextareaState>,
    repo_name: Entity<InputState>,
    repo_note: Entity<InputState>,
    repo_path: Entity<InputState>,
    _subs: Vec<Subscription>,
}

impl ProjectsTab {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let line = |p: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(p))
        };
        let area = |rows: usize, p: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(rows, 14)
                    .placeholder(p)
            })
        };
        let new_name = line("Project name", window, cx);
        let created = cx.subscribe_in(&new_name, window, |this, _, ev: &InputEvent, w, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.create(w, cx);
            }
        });
        let events = crate::events::events(cx);
        let live =
            cx.subscribe_in(
                &events,
                window,
                |this, _, ev: &Arc<EngineEvent>, w, cx| match &**ev {
                    EngineEvent::ProjectChanged { project_id }
                        if this.selected.as_deref() == Some(project_id) =>
                    {
                        this.reload(w, cx)
                    }
                    EngineEvent::ItemChanged { item, .. }
                        if item.project_id.is_some() && item.project_id == this.selected =>
                    {
                        this.load_project(cx)
                    }
                    _ => {}
                },
            );
        let mut this = ProjectsTab {
            projects: Vec::new(),
            selected: None,
            view: View::Home,
            focus: Focus::Coordinator,
            items: Vec::new(),
            notes: Vec::new(),
            repos: Vec::new(),
            show_done: false,
            coordinator: None,
            item: cx.new(ItemPane::new),
            creating: false,
            error: None,
            new_name,
            note_topic: line("Topic, e.g. Deploys", window, cx),
            note_text: area(3, "What agents should know", window, cx),
            editing_note: None,
            about: area(3, "What is this project, and what does it do?", window, cx),
            rules: area(
                5,
                "How should agents work here? One per line, e.g.\n- Reply to customers short and technical\n- Refunds over $100 need me",
                window,
                cx,
            ),
            repo_name: line("owner/name or URL", window, cx),
            repo_note: line("What it's for, e.g. the API", window, cx),
            repo_path: line("Local checkout (optional), e.g. ~/src/api", window, cx),
            _subs: vec![created, live],
        };
        this.reload(window, cx);
        this
    }

    fn current(&self) -> Option<&Project> {
        let id = self.selected.as_ref()?;
        self.projects.iter().find(|p| &p.id == id)
    }

    /// Reloads the project list, then the selected project.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let b = backend(cx);
        match b.projects() {
            Ok(ps) => self.projects = ps,
            Err(e) => self.error = Some(e),
        }
        if self.current().is_none() {
            self.selected = self.projects.first().map(|p| p.id.clone());
            self.coordinator = None;
            self.focus = Focus::Coordinator;
        }
        if let Some(p) = self.current().cloned() {
            self.about
                .update(cx, |s, cx| s.set_value(p.about.clone(), window, cx));
            self.rules
                .update(cx, |s, cx| s.set_value(p.rules.clone(), window, cx));
            if self.coordinator.is_none() {
                self.coordinator =
                    Some(cx.new(|cx| ChatView::of(Target::Coordinator(p), window, cx)));
            }
        }
        self.load_project(cx);
        cx.notify();
    }

    fn load_project(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            self.items.clear();
            self.notes.clear();
            self.repos.clear();
            return;
        };
        let b = backend(cx);
        self.items = b.project_items(&id).unwrap_or_default();
        self.notes = b.project_notes(&id).unwrap_or_default();
        self.repos = b.project_repos(&id).unwrap_or_default();
        cx.notify();
    }

    fn select(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&id) {
            return;
        }
        self.selected = Some(id);
        self.coordinator = None;
        self.focus = Focus::Coordinator;
        self.editing_note = None;
        self.reload(window, cx);
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.new_name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        let color = format!("#{:06x}", COLORS[self.projects.len() % COLORS.len()]);
        match backend(cx).project_save(None, &name, &color, "folder") {
            Ok(p) => {
                self.creating = false;
                self.new_name
                    .update(cx, |s, cx| s.set_value("", window, cx));
                self.selected = Some(p.id);
                self.coordinator = None;
                self.reload(window, cx);
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn open_item(&mut self, item: QueueItem, cx: &mut Context<Self>) {
        self.focus = Focus::Item(item.id.clone());
        self.view = View::Home;
        self.item.update(cx, |p, cx| p.show(Some(item), cx));
        cx.notify();
    }

    fn save_profile(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let about = self.about.read(cx).value().to_string();
        let rules = self.rules.read(cx).value().to_string();
        if let Err(e) = backend(cx).project_set_profile(&id, Some(&about), Some(&rules)) {
            self.error = Some(e);
        }
        cx.notify();
    }

    fn save_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pid) = self.selected.clone() else {
            return;
        };
        let topic = self.note_topic.read(cx).value().trim().to_string();
        let text = self.note_text.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let topic = if topic.is_empty() {
            "General".to_string()
        } else {
            topic
        };
        match backend(cx).project_note_save(&pid, self.editing_note.as_deref(), &topic, &text) {
            Ok(()) => {
                self.editing_note = None;
                self.note_topic
                    .update(cx, |s, cx| s.set_value("", window, cx));
                self.note_text
                    .update(cx, |s, cx| s.set_value("", window, cx));
                self.load_project(cx);
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn edit_note(&mut self, n: &ProjectNote, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_note = Some(n.id.clone());
        let (topic, text) = (n.topic.clone(), n.text.clone());
        self.note_topic
            .update(cx, |s, cx| s.set_value(topic, window, cx));
        self.note_text
            .update(cx, |s, cx| s.set_value(text, window, cx));
        cx.notify();
    }

    fn delete_note(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Err(e) = backend(cx).project_note_delete(id) {
            self.error = Some(e);
        }
        self.load_project(cx);
    }

    fn add_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pid) = self.selected.clone() else {
            return;
        };
        let repo = self.repo_name.read(cx).value().trim().to_string();
        if repo.is_empty() {
            return;
        }
        let note = self.repo_note.read(cx).value().trim().to_string();
        let path = self.repo_path.read(cx).value().trim().to_string();
        let path = (!path.is_empty()).then_some(path);
        match backend(cx).project_repo_save(&pid, &repo, path.as_deref(), Some(&note)) {
            Ok(_) => {
                for s in [&self.repo_name, &self.repo_note, &self.repo_path] {
                    s.update(cx, |s, cx| s.set_value("", window, cx));
                }
                self.load_project(cx);
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn delete_repo(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(pid) = self.selected.clone() else {
            return;
        };
        if let Err(e) = backend(cx).project_repo_delete(&pid, id) {
            self.error = Some(e);
        }
        self.load_project(cx);
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let mut list = div().flex().flex_col().gap(px(2.));
        for p in &self.projects {
            let active = self.selected.as_ref() == Some(&p.id);
            let id = p.id.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("pj-{}", p.id)))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(size::RADIUS_MD))
                    .cursor_pointer()
                    .text_size(px(size::TEXT_MD))
                    .when(active, |el| el.bg(t.bg_raised).text_color(t.fg))
                    .when(!active, |el| {
                        el.text_color(t.fg_muted).hover(|s| s.text_color(t.fg))
                    })
                    .child(mark(p, 22.))
                    .child(div().flex_1().min_w_0().truncate().child(p.name.clone()))
                    .on_click(cx.listener(move |this, _, w, cx| this.select(id.clone(), w, cx))),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .w(px(220.))
            .flex_none()
            .h_full()
            .px(px(12.))
            .py(px(20.))
            .border_r_1()
            .border_color(t.border)
            .child(
                div()
                    .px(px(8.))
                    .pb(px(8.))
                    .text_size(px(size::TEXT_XL))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Projects"),
            )
            .child(list)
            .child(if self.creating {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(Input::new(&self.new_name))
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(
                                Button::new("create")
                                    .kind(ButtonKind::Primary)
                                    .label("Create")
                                    .on_click(cx.listener(|this, _, w, cx| this.create(w, cx))),
                            )
                            .child(
                                Button::new("cancel-create")
                                    .kind(ButtonKind::Ghost)
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.creating = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .into_any_element()
            } else {
                div()
                    .child(
                        Button::new("new-project")
                            .kind(ButtonKind::Ghost)
                            .icon("plus")
                            .label("New project")
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.creating = true;
                                this.new_name.update(cx, |s, cx| s.focus(w, cx));
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            })
    }

    fn render_threads(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let (done, open): (Vec<_>, Vec<_>) =
            self.items.iter().partition(|i| i.state == ItemState::Done);
        let selected = match &self.focus {
            Focus::Item(id) => Some(id.clone()),
            Focus::Coordinator => None,
        };
        let row = |i: &QueueItem, cx: &mut Context<Self>| {
            let active = selected.as_ref() == Some(&i.id);
            let item = i.clone();
            div()
                .id(SharedString::from(format!("th-{}", i.id)))
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(12.))
                .py(px(8.))
                .cursor_pointer()
                .border_l_2()
                .border_color(if active {
                    t.accent.fg
                } else {
                    gpui::transparent_black()
                })
                .when(active, |el| el.bg(t.bg_selected))
                .hover(|s| s.bg(t.bg_hover))
                .child(
                    div()
                        .text_size(px(size::TEXT_MD))
                        .truncate()
                        .child(i.event.title.clone()),
                )
                .child(
                    div()
                        .text_size(px(size::TEXT_SM))
                        .text_color(t.fg_subtle)
                        .child(format!(
                            "{} · {}",
                            crate::work::model::byline(i),
                            ago(i.event.occurred_at, chrono::Utc::now())
                        )),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_item(item.clone(), cx)))
        };
        let head = |label: String| {
            div()
                .px(px(12.))
                .pt(px(12.))
                .pb(px(4.))
                .text_size(px(size::TEXT_XS))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(t.fg_subtle)
                .child(label)
        };
        let mut col = div().flex().flex_col();
        col = col.child(
            div()
                .id("coord-row")
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .py(px(10.))
                .cursor_pointer()
                .border_b_1()
                .border_color(t.border)
                .when(self.focus == Focus::Coordinator, |el| el.bg(t.bg_selected))
                .hover(|s| s.bg(t.bg_hover))
                .child(icon("sparkles", 14., t.accent.fg))
                .child(
                    div()
                        .text_size(px(size::TEXT_MD))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child("Coordinator"),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.focus = Focus::Coordinator;
                    cx.notify();
                })),
        );
        if self.items.is_empty() {
            col = col.child(
                div()
                    .p(px(12.))
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_subtle)
                    .child("Nothing here yet. Ask the coordinator to start threads, or pick this project when starting a chat in Work."),
            );
        }
        if !open.is_empty() {
            col = col.child(head(format!("OPEN · {}", open.len())));
            for i in open {
                col = col.child(row(i, cx));
            }
        }
        if !done.is_empty() {
            col = col.child(
                div()
                    .id("done-head")
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .cursor_pointer()
                    .child(head(format!("DONE · {}", done.len())))
                    .child(icon(
                        if self.show_done {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        },
                        11.,
                        t.fg_subtle,
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_done = !this.show_done;
                        cx.notify();
                    })),
            );
            if self.show_done {
                for i in done {
                    col = col.child(row(i, cx));
                }
            }
        }
        div()
            .id("threads")
            .w(px(300.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(t.border)
            .child(col)
    }

    fn render_notes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .max_w(px(760.))
            .px(px(40.))
            .py(px(24.))
            .child(
                div()
                    .text_size(px(size::TEXT_MD))
                    .text_color(t.fg_muted)
                    .child("What agents learned about this project, and what you told them. Every chat here reads these."),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(14.))
                    .rounded(px(size::RADIUS_LG))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.bg_raised)
                    .child(Input::new(&self.note_topic))
                    .child(Textarea::new(&self.note_text))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                Button::new("save-note")
                                    .kind(ButtonKind::Primary)
                                    .label(if self.editing_note.is_some() {
                                        "Save note"
                                    } else {
                                        "Add note"
                                    })
                                    .on_click(cx.listener(|this, _, w, cx| this.save_note(w, cx))),
                            )
                            .when(self.editing_note.is_some(), |el| {
                                el.child(
                                    Button::new("cancel-note")
                                        .kind(ButtonKind::Ghost)
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _, w, cx| {
                                            this.editing_note = None;
                                            this.note_topic
                                                .update(cx, |s, cx| s.set_value("", w, cx));
                                            this.note_text
                                                .update(cx, |s, cx| s.set_value("", w, cx));
                                            cx.notify();
                                        })),
                                )
                            }),
                    ),
            );
        if self.notes.is_empty() {
            col = col.child(
                div()
                    .text_size(px(size::TEXT_MD))
                    .text_color(t.fg_subtle)
                    .child("No notes yet."),
            );
        }
        for n in &self.notes {
            let (edit, del) = (n.clone(), n.id.clone());
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .p(px(14.))
                    .rounded(px(size::RADIUS_LG))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.bg_raised)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(size::TEXT_MD))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(n.topic.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(size::TEXT_SM))
                                    .text_color(t.fg_subtle)
                                    .child(format!(
                                        "{} · {}",
                                        if n.added_by == "user" { "you" } else { "agent" },
                                        ago(n.updated_at, chrono::Utc::now())
                                    )),
                            )
                            .child(
                                Button::new(SharedString::from(format!("edit-{}", n.id)))
                                    .kind(ButtonKind::Ghost)
                                    .icon("edit")
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.edit_note(&edit, w, cx)
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("del-{}", n.id)))
                                    .kind(ButtonKind::Ghost)
                                    .icon("x")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete_note(&del, cx)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(size::TEXT_MD))
                            .text_color(t.fg)
                            .child(n.text.clone()),
                    ),
            );
        }
        div().id("notes").size_full().overflow_y_scroll().child(col)
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let label = |s: &'static str| {
            div()
                .text_size(px(size::TEXT_SM))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(t.fg_muted)
                .child(s)
        };
        let mut repos = div().flex().flex_col().gap(px(6.));
        if self.repos.is_empty() {
            repos = repos.child(
                div()
                    .text_size(px(size::TEXT_MD))
                    .text_color(t.fg_subtle)
                    .child("No repos yet. Agents add the ones they work in for this project."),
            );
        }
        for r in &self.repos {
            let id = r.id.clone();
            repos = repos.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .p(px(10.))
                    .rounded(px(size::RADIUS_MD))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.bg_raised)
                    .child(icon("git-branch", 14., t.fg_subtle))
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
                                    .child(r.repo.clone())
                                    .child(div().text_color(t.fg_subtle).child(format!(
                                        "{} · {}",
                                        if r.added_by == "agent" {
                                            "agent"
                                        } else {
                                            "you"
                                        },
                                        ago(r.updated_at, chrono::Utc::now())
                                    ))),
                            )
                            .when(!r.note.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_size(px(size::TEXT_SM))
                                        .text_color(t.fg_muted)
                                        .child(r.note.clone()),
                                )
                            })
                            .when_some(r.path.clone(), |el, p| {
                                el.child(
                                    div()
                                        .font_family(FONT_MONO)
                                        .text_size(px(size::TEXT_SM))
                                        .text_color(t.fg_subtle)
                                        .child(p),
                                )
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("rm-{}", r.id)))
                            .kind(ButtonKind::Ghost)
                            .icon("x")
                            .on_click(cx.listener(move |this, _, _, cx| this.delete_repo(&id, cx))),
                    ),
            );
        }
        div()
            .id("pj-settings")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .max_w(px(760.))
                    .px(px(40.))
                    .py(px(24.))
                    .child(
                        div()
                            .text_size(px(size::TEXT_MD))
                            .text_color(t.fg_muted)
                            .child("Every chat in this project reads these before it starts, and agents keep them up to date."),
                    )
                    .child(label("ABOUT"))
                    .child(Textarea::new(&self.about))
                    .child(label("INSTRUCTIONS"))
                    .child(Textarea::new(&self.rules))
                    .child(
                        div().flex().child(
                            Button::new("save-profile")
                                .kind(ButtonKind::Primary)
                                .label("Save")
                                .on_click(cx.listener(|this, _, _, cx| this.save_profile(cx))),
                        ),
                    )
                    .child(label("REPOS · WHERE AGENTS WORK"))
                    .child(repos)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(Input::new(&self.repo_name))
                            .child(Input::new(&self.repo_note))
                            .child(Input::new(&self.repo_path))
                            .child(
                                div().flex().child(
                                    Button::new("add-repo")
                                        .icon("plus")
                                        .label("Add repo")
                                        .on_click(cx.listener(|this, _, w, cx| this.add_repo(w, cx))),
                                ),
                            ),
                    ),
            )
    }
}

impl Render for ProjectsTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let sidebar = self.render_sidebar(cx).into_any_element();
        let Some(p) = self.current().cloned() else {
            return div()
                .size_full()
                .flex()
                .bg(t.bg)
                .child(sidebar)
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(8.))
                        .child(icon("folder", 28., t.fg_subtle))
                        .child(
                            div()
                                .text_size(px(size::TEXT_LG))
                                .child("No projects yet"),
                        )
                        .child(
                            div()
                                .max_w(px(420.))
                                .text_size(px(size::TEXT_MD))
                                .text_color(t.fg_muted)
                                .child("Run a piece of work, or a whole business, as a project: a coordinator that watches it, its threads, and everything it knows."),
                        ),
                )
                .into_any_element();
        };
        let open = self
            .items
            .iter()
            .filter(|i| i.state != ItemState::Done)
            .count();
        let mut tabs = div().flex().gap(px(16.));
        for (view, label, count) in [
            (View::Home, "Home", open),
            (View::Notes, "Notes", self.notes.len()),
            (View::Settings, "Settings", 0),
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
                    .when(count > 0, |el| {
                        el.child(div().text_color(t.fg_subtle).child(count.to_string()))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.view = view;
                        cx.notify();
                    })),
            );
        }
        let body = match self.view {
            View::Home => {
                let center = match &self.focus {
                    Focus::Coordinator => match &self.coordinator {
                        Some(c) => c.clone().into_any_element(),
                        None => div().into_any_element(),
                    },
                    Focus::Item(_) => self.item.clone().into_any_element(),
                };
                div()
                    .size_full()
                    .flex()
                    .child(self.render_threads(cx).into_any_element())
                    .child(div().flex_1().min_w_0().h_full().child(center))
                    .into_any_element()
            }
            View::Notes => self.render_notes(cx).into_any_element(),
            View::Settings => self.render_settings(cx).into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .bg(t.bg)
            .child(sidebar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .px(px(24.))
                            .pt(px(16.))
                            .border_b_1()
                            .border_color(t.border)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .child(mark(&p, 30.))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .text_size(px(size::TEXT_XL))
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child(p.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(size::TEXT_SM))
                                                    .text_color(t.fg_muted)
                                                    .truncate()
                                                    .child(if p.about.is_empty() {
                                                        format!("{open} open")
                                                    } else {
                                                        format!("{open} open · {}", p.about)
                                                    }),
                                            ),
                                    ),
                            )
                            .child(tabs),
                    )
                    .when_some(self.error.clone(), |el, e| {
                        el.child(
                            div()
                                .px(px(24.))
                                .py(px(6.))
                                .text_size(px(size::TEXT_SM))
                                .text_color(t.danger.fg)
                                .child(e),
                        )
                    })
                    .child(div().flex_1().min_h_0().child(body)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_color_reads_grey() {
        assert_eq!(color_of("nope"), color_of("#8b8d98"));
        assert_ne!(color_of("#7c5cff"), color_of("#8b8d98"));
    }
}
