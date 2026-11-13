//! The Work tab: the ranked queue on the left, the open item on the right.

mod item;
pub mod model;
mod queue;

use gpui::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div, px,
};

use crate::theme::theme;
pub use item::ItemPane;
pub use queue::Queue;

pub struct WorkTab {
    queue: Entity<Queue>,
    item: Entity<ItemPane>,
}

impl WorkTab {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let item = cx.new(|cx| ItemPane::new(cx));
        let queue = cx.new(|cx| Queue::new(window, cx));
        // Picking a row opens it.
        cx.subscribe(&queue, {
            let item = item.clone();
            move |_, _, ev: &queue::Selected, cx| {
                item.update(cx, |pane, cx| pane.show(ev.0.clone(), cx));
            }
        })
        .detach();
        WorkTab { queue, item }
    }

    pub fn queue(&self) -> &Entity<Queue> {
        &self.queue
    }
}

impl Render for WorkTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        div()
            .flex()
            .size_full()
            .child(
                div()
                    .w(px(460.))
                    .flex_none()
                    .h_full()
                    .border_r_1()
                    .border_color(t.border)
                    .bg(t.bg_raised)
                    .child(self.queue.clone()),
            )
            .child(div().flex_1().h_full().min_w_0().child(self.item.clone()))
    }
}

pub fn bind_keys(cx: &mut gpui::App) {
    queue::bind_keys(cx);
}
