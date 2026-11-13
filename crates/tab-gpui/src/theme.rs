//! tab's design tokens (`src/design/tokens.css`) for GPUI. Views read
//! colors from [`Theme`] only; never raw colors.

use gpui::{App, Global, Hsla, Rgba, WindowAppearance, rgb};

#[derive(Clone, Copy, Debug)]
pub struct Tone {
    pub fg: Hsla,
    pub bg: Hsla,
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub dark: bool,
    pub bg: Hsla,
    pub bg_raised: Hsla,
    pub bg_sunken: Hsla,
    pub bg_hover: Hsla,
    pub bg_selected: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub fg: Hsla,
    pub fg_muted: Hsla,
    pub fg_subtle: Hsla,
    pub focus: Hsla,
    pub neutral: Tone,
    pub accent: Tone,
    pub merged: Tone,
    pub info: Tone,
    pub success: Tone,
    pub warning: Tone,
    pub danger: Tone,
}

/// Font sizes, spacing and radii from tokens.css, in pixels.
pub mod size {
    pub const TEXT_XS: f32 = 11.;
    pub const TEXT_SM: f32 = 12.;
    pub const TEXT_MD: f32 = 13.;
    pub const TEXT_LG: f32 = 15.;
    pub const TEXT_XL: f32 = 18.;
    pub const TEXT_2XL: f32 = 22.;
    pub const RADIUS_SM: f32 = 4.;
    pub const RADIUS_MD: f32 = 6.;
    pub const RADIUS_LG: f32 = 10.;
}

pub const FONT_SANS: &str = "Inter";
pub const FONT_MONO: &str = "JetBrains Mono";

fn c(hex: u32) -> Hsla {
    let c: Rgba = rgb(hex);
    c.into()
}

fn tone(fg: u32, bg: u32) -> Tone {
    Tone {
        fg: c(fg),
        bg: c(bg),
    }
}

impl Theme {
    pub fn light() -> Self {
        Theme {
            dark: false,
            bg: c(0xf7f7f8),
            bg_raised: c(0xffffff),
            bg_sunken: c(0xefeff1),
            bg_hover: c(0xececef),
            bg_selected: c(0xe6e8f5),
            border: c(0xe3e3e8),
            border_strong: c(0xcfcfd6),
            fg: c(0x18181b),
            fg_muted: c(0x5c5c66),
            fg_subtle: c(0x8b8b95),
            focus: c(0x4f5bd5),
            neutral: tone(0x52525b, 0xefeff1),
            accent: tone(0x4f46e5, 0xecebfd),
            merged: tone(0x8250df, 0xfbefff),
            info: tone(0x0369a1, 0xe3f2fb),
            success: tone(0x15803d, 0xe5f6ea),
            warning: tone(0xb45309, 0xfdf1df),
            danger: tone(0xc2261d, 0xfde9e7),
        }
    }

    pub fn dark() -> Self {
        Theme {
            dark: true,
            bg: c(0x111113),
            bg_raised: c(0x18181b),
            bg_sunken: c(0x0c0c0e),
            bg_hover: c(0x222226),
            bg_selected: c(0x23253a),
            border: c(0x2a2a2f),
            border_strong: c(0x3a3a41),
            fg: c(0xececef),
            fg_muted: c(0xa1a1aa),
            fg_subtle: c(0x71717a),
            focus: c(0x818cf8),
            neutral: tone(0xa1a1aa, 0x232327),
            accent: tone(0xa5b4fc, 0x25264a),
            merged: tone(0xab7df8, 0x2a1d42),
            info: tone(0x7dd3fc, 0x0f2a3a),
            success: tone(0x86efac, 0x10291a),
            warning: tone(0xfcd34d, 0x32250c),
            danger: tone(0xfca5a5, 0x3a1513),
        }
    }

    pub fn for_appearance(a: WindowAppearance) -> Self {
        match a {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            _ => Self::light(),
        }
    }

    /// A connector's semantic tone.
    pub fn tone(&self, t: tab_core::model::Tone) -> Tone {
        use tab_core::model::Tone as T;
        match t {
            T::Neutral => self.neutral,
            T::Accent => self.accent,
            T::Info => self.info,
            T::Success => self.success,
            T::Warning => self.warning,
            T::Danger => self.danger,
        }
    }

    /// The color of an urgency heading and score badge.
    pub fn urgency(&self, u: tab_core::model::Urgency) -> Hsla {
        use tab_core::model::Urgency as U;
        match u {
            U::Now => self.danger.fg,
            U::Today => self.warning.fg,
            U::ThisWeek => self.info.fg,
            U::Whenever => self.fg_subtle,
        }
    }
}

impl Global for Theme {}

/// The theme views draw with.
pub fn theme(cx: &App) -> &Theme {
    cx.global::<Theme>()
}

/// Sets tab's theme for this appearance, and dresses gpui-component's
/// controls (inputs, menus, popovers, lists) in the same tokens.
pub fn apply(appearance: WindowAppearance, cx: &mut App) {
    let t = Theme::for_appearance(appearance);
    gpui_component::Theme::change(appearance, None, cx);
    let tc = t.clone();
    gpui_component::Theme::update(cx, move |c| {
        let t = tc;
        let k = &mut c.colors;
        k.background = t.bg;
        k.foreground = t.fg;
        k.border = t.border;
        k.input = t.border_strong;
        k.ring = t.focus;
        k.caret = t.fg;
        k.selection = t.focus.opacity(0.25);
        k.muted = t.bg_sunken;
        k.muted_foreground = t.fg_muted;
        k.accent = t.bg_hover;
        k.accent_foreground = t.fg;
        k.primary = t.fg;
        k.primary_hover = t.fg.opacity(0.85);
        k.primary_active = t.fg.opacity(0.75);
        k.primary_foreground = t.bg_raised;
        k.secondary = t.bg_raised;
        k.secondary_hover = t.bg_hover;
        k.secondary_active = t.bg_sunken;
        k.secondary_foreground = t.fg;
        k.popover = t.bg_raised;
        k.popover_foreground = t.fg;
        k.list = t.bg_raised;
        k.list_hover = t.bg_hover;
        k.list_active = t.bg_selected;
        k.list_active_border = t.focus;
        k.link = t.accent.fg;
        k.danger = t.danger.fg;
        k.success = t.success.fg;
        k.warning = t.warning.fg;
        k.info = t.info.fg;
        c.font_family = FONT_SANS.into();
        c.mono_font_family = FONT_MONO.into();
        c.font_size = gpui::px(size::TEXT_MD);
        c.radius = gpui::px(size::RADIUS_MD);
        c.radius_lg = gpui::px(size::RADIUS_LG);
    });
    cx.set_global(t);
}

/// The system's appearance, unless `TAB_APPEARANCE` (`light` or `dark`)
/// pins one, as screenshots do.
pub fn appearance(system: WindowAppearance) -> WindowAppearance {
    match std::env::var("TAB_APPEARANCE").as_deref() {
        Ok("dark") => WindowAppearance::Dark,
        Ok("light") => WindowAppearance::Light,
        _ => system,
    }
}
