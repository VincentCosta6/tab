//! Assets GPUI loads by path: `icons/<name>.svg` from the design system's
//! icon set, and the brand marks.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

include!(concat!(env!("OUT_DIR"), "/icons.rs"));

/// The tab mark (`src/design/brands.tsx`).
const TAB_MARK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024"><rect width="1024" height="1024" rx="230" fill="#1c1c20"/><g fill="none" stroke-linecap="round" stroke-width="84"><path d="M220 300 C 420 300, 440 512, 600 512" stroke="#7dd3fc"/><path d="M220 724 C 420 724, 440 512, 600 512" stroke="#f9a8d4"/><path d="M220 512 H 600" stroke="#fcd34d"/></g><rect x="580" y="452" width="230" height="120" rx="60" fill="#fff"/></svg>"##;

pub struct Assets;

pub fn icon_path(name: &str) -> SharedString {
    let name = if ICONS.iter().any(|(n, _)| *n == name) {
        name
    } else {
        "dot"
    };
    format!("icons/{name}.svg").into()
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if path == "brands/tab.svg" {
            return Ok(Some(Cow::Borrowed(TAB_MARK.as_bytes())));
        }
        let name = path
            .strip_prefix("icons/")
            .and_then(|p| p.strip_suffix(".svg"));
        Ok(name
            .and_then(|n| ICONS.iter().find(|(i, _)| *i == n))
            .map(|(_, svg)| Cow::Borrowed(svg.as_bytes())))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(if path.starts_with("icons") {
            ICONS
                .iter()
                .map(|(n, _)| format!("icons/{n}.svg").into())
                .collect()
        } else {
            Vec::new()
        })
    }
}

/// Whether the design system has an icon by this name.
pub fn has_icon(name: &str) -> bool {
    ICONS.iter().any(|(n, _)| *n == name)
}
