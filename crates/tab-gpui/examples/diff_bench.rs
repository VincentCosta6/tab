//! Shows one patch in gpui-component's diff viewer, for comparing scrolling
//! a large PR against the Tauri app's Monaco review.
//!
//! ```sh
//! git -C ~/src/tab show -p <big commit> > /tmp/big.patch
//! cargo run --release --example diff_bench -- /tmp/big.patch
//! ```
//!
//! `DIFF_MODE=split` shows the sides next to each other (Monaco's split
//! mode); the default is unified, like Monaco's inline mode. Both keep three
//! unchanged lines around each change, as tab's review does. Timings go to
//! stderr: parsing and the first frame with the diff in it. Syntax colors
//! are prepared in the background, as in the viewer itself.

use std::time::Instant;

use gpui::{
    App, Bounds, Context, Entity, Focusable, IntoElement, ParentElement, Render, Styled, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use gpui_component::{
    ActiveTheme, Root, Theme, ThemeMode,
    diff::{Diff, DiffFile, DiffMode, DiffState},
    highlighter::{HighlightTheme, SyntaxHighlighter},
};

fn language(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "json" => "json",
        _ => "text",
    }
}

struct Bench {
    state: Entity<DiffState>,
    started: Instant,
    painted: bool,
}

impl Render for Bench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.painted {
            self.painted = true;
            let started = self.started;
            window.on_next_frame(move |_, _| {
                eprintln!(
                    "first_frame_ms {:.1}",
                    started.elapsed().as_secs_f64() * 1e3
                );
            });
        }
        div()
            .size_full()
            .bg(cx.theme().background)
            .child(Diff::new(&self.state).size_full())
    }
}

fn main() -> anyhow::Result<()> {
    let started = Instant::now();
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: diff_bench <patch file>"))?;
    let patch = std::fs::read_to_string(&path)?;
    let split = std::env::var("DIFF_MODE").is_ok_and(|m| m == "split");

    let parse = Instant::now();
    let files: Vec<DiffFile> = DiffFile::parse(&patch)?
        .into_iter()
        .map(|f| {
            let lang = language(f.path());
            f.with_language(lang)
        })
        .collect();
    let lines: usize = patch.lines().count();
    eprintln!(
        "parsed {} files, {} patch lines in {:.1} ms",
        files.len(),
        lines,
        parse.elapsed().as_secs_f64() * 1e3
    );

    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        let theme = HighlightTheme::default_dark();

        let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let state = cx.new(|cx| {
                    DiffState::new(files, cx)
                        .with_mode(if split {
                            DiffMode::Split
                        } else {
                            DiffMode::Unified
                        })
                        .with_context_lines(Some(3))
                        .with_syntax_highlighter(move |lang, text| {
                            let mut h = SyntaxHighlighter::new(lang);
                            let rope = ropey::Rope::from_str(text);
                            h.update(None, &rope, None);
                            h.styles(&(0..text.len()), &*theme)
                        })
                });
                let focus = state.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
                let bench = cx.new(|_| Bench {
                    state,
                    started,
                    painted: false,
                });
                cx.new(|cx| Root::new(bench, window, cx))
            },
        )
        .expect("open the window");
        cx.activate(true);
    });
    Ok(())
}
