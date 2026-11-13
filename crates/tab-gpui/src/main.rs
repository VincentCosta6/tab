use gpui::{App, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px, size};

struct Root;

impl Render for Root {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().items_center().justify_center().bg(gpui::white()).text_color(gpui::black()).text_xl().font_family("Inter").child("tab")
    }
}

fn main() {
    env_logger::init();
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, |_, cx| cx.new(|_| Root))
            .unwrap();
    });
}
