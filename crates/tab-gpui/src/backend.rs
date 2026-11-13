//! The backend as a GPUI global, so any view can reach it.

use std::sync::Arc;

use gpui::{App, Global};

pub struct Backend(pub Arc<tab_app::Backend>);

impl Global for Backend {}

pub fn backend(cx: &App) -> Arc<tab_app::Backend> {
    cx.global::<Backend>().0.clone()
}
