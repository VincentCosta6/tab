//! tab's desktop app on GPUI.
//!
//! Boots the backend (`tab-app`) on a tokio runtime, then opens the window.
//! `TAB_IDENTIFIER` picks the data folder and ports; it defaults to the
//! GPUI dev build's own, so this never opens the installed app's data.

mod assets;
mod backend;
mod blocks;
mod chat;
mod components;
mod connections;
mod events;
mod runtime;
mod settings;
mod shell;
mod theme;
mod work;

use gpui::{App, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};
use gpui_component::Root;

fn main() -> anyhow::Result<()> {
    tab_app::logging::init();
    let identifier =
        std::env::var("TAB_IDENTIFIER").unwrap_or_else(|_| tab_app::paths::GPUI_DEV.into());
    let paths = tab_app::Paths::for_identifier(&identifier)?;
    let rt = runtime::init();
    let backend = rt.block_on(tab_app::Backend::boot(paths))?;

    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            theme::apply(theme::appearance(cx.window_appearance()), cx);
            cx.set_global(backend::Backend(backend.clone()));
            events::start(&backend.events, cx);
            shell::bind_keys(cx);
            work::bind_keys(cx);

            let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    app_id: Some(identifier.clone()),
                    ..Default::default()
                },
                |window, cx| {
                    // Follow the system's light or dark appearance.
                    window
                        .observe_window_appearance(|window, cx| {
                            theme::apply(theme::appearance(window.appearance()), cx);
                            cx.refresh_windows();
                        })
                        .detach();
                    let shell = cx.new(|cx| shell::Shell::new(window, cx));
                    cx.new(|cx| Root::new(shell, window, cx))
                },
            )
            .expect("open the window");
            cx.activate(true);
        });
    Ok(())
}
