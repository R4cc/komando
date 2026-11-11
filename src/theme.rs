//! Colours: the App Theme (Komando's chrome), the Terminal Theme (terminal content), and the
//! Project color presets. The two themes are independent settings.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::{Hsla, Rgba, rgb, rgba};

/// KOMANDO brand orange (orange-600, for light themes). Only for Komando's own chrome,
/// never for Project colors.
pub const BRAND: u32 = 0xea580c;
/// The brand orange on dark surfaces (orange-500).
pub const BRAND_DARK: u32 = 0xf97316;

/// A Theme: the colours of the app and the terminal for one mode. Dark and light themes are
/// separate lists, picked separately, so each mode can have its own character.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub dark: bool,
    /// Buttons, switches, selections, links.
    pub accent: u32,
    /// The window background the islands float on, the islands, the sidebar and dialogs.
    pub canvas: u32,
    pub background: u32,
    pub sidebar: u32,
    pub surface: u32,
    pub text: u32,
    pub text_muted: u32,
    pub text_faint: u32,
    pub term_background: u32,
    pub term_foreground: u32,
}

const fn spec(
    id: &'static str,
    name: &'static str,
    dark: bool,
    accent: u32,
    [canvas, background, sidebar, surface]: [u32; 4],
    [text, text_muted, text_faint]: [u32; 3],
    [term_background, term_foreground]: [u32; 2],
) -> ThemeSpec {
    ThemeSpec { id, name, dark, accent, canvas, background, sidebar, surface, text, text_muted, text_faint, term_background, term_foreground }
}

/// Dark themes. Greys are tinted toward each theme's accent, never near-black: deep grey is
/// easier on the eyes over long sessions. The first is the default.
pub const DARK_THEMES: &[ThemeSpec] = &[
    spec("ember", "Ember", true, BRAND_DARK, [0x121215, 0x18181c, 0x1c1c20, 0x232328], [0xececef, 0xa1a1aa, 0x85858f], [0x1a1a1e, 0xd4d4d8]),
    spec("midnight", "Midnight", true, 0x60a5fa, [0x0e121a, 0x131924, 0x171e2a, 0x1e2633], [0xe6ebf3, 0x9ca8ba, 0x7f8ba0], [0x151b26, 0xd0d8e6]),
    spec("forest", "Forest", true, 0x4ade80, [0x0f1411, 0x141b17, 0x18201b, 0x1f2822], [0xe5eee8, 0x9db2a5, 0x7f9587], [0x161d19, 0xd0ddd4]),
    spec("dusk", "Dusk", true, 0xa78bfa, [0x13111a, 0x1a1623, 0x1e1a29, 0x262132], [0xece8f4, 0xaaa2bc, 0x8d85a0], [0x1c1826, 0xdbd5e7]),
    spec("rose", "Rosé", true, 0xf472b6, [0x161013, 0x1e161a, 0x231a1f, 0x2b2026], [0xf3e9ee, 0xb9a3ae, 0x9a8590], [0x20171b, 0xe7d7df]),
    spec("glacier", "Glacier", true, 0x88c0d0, [0x1a1e26, 0x21262f, 0x252b35, 0x2d343f], [0xeceff4, 0xaeb7c6, 0x939dae], [0x232832, 0xd8dee9]),
    spec("graphite", "Graphite", true, 0xe4e4e7, [0x0f0f10, 0x151516, 0x19191b, 0x212123], [0xededee, 0xa3a3a8, 0x88888e], [0x171718, 0xd6d6d9]),
];

/// Light themes, the counterparts of the dark ones. The first is the default.
pub const LIGHT_THEMES: &[ThemeSpec] = &[
    spec("ember", "Ember", false, BRAND, [0xe8e8eb, 0xffffff, 0xfafafa, 0xffffff], [0x18181b, 0x52525b, 0x71717a], [0xfcfcfb, 0x27272a]),
    spec("sky", "Sky", false, 0x2563eb, [0xe2e8f1, 0xfdfeff, 0xf5f8fc, 0xffffff], [0x141b2b, 0x4a5568, 0x647085], [0xfbfcff, 0x1f2937]),
    spec("sage", "Sage", false, 0x15803d, [0xe1e8e3, 0xfdfffd, 0xf4f8f5, 0xffffff], [0x15201a, 0x4a5a50, 0x627268], [0xfbfdfb, 0x1f2a23]),
    spec("lavender", "Lavender", false, 0x7c3aed, [0xe8e4f1, 0xfefdff, 0xf8f6fd, 0xffffff], [0x1d1729, 0x564d66, 0x726a83], [0xfcfbfe, 0x2a2338]),
    spec("blossom", "Blossom", false, 0xdb2777, [0xf1e3ea, 0xfffdfe, 0xfcf5f8, 0xffffff], [0x2a1820, 0x654b57, 0x836776], [0xfefbfc, 0x33222b]),
    spec("paper", "Paper", false, 0x0f766e, [0xe9e3d7, 0xfffdf8, 0xfaf6ee, 0xfffdf8], [0x2b2418, 0x5c5444, 0x766c5b], [0xfdfaf3, 0x2f2a20]),
    spec("graphite", "Graphite", false, 0x27272a, [0xe4e4e6, 0xffffff, 0xf7f7f8, 0xffffff], [0x18181b, 0x52525b, 0x71717a], [0xfcfcfc, 0x27272a]),
];

/// The dark or light theme with this id, or the default one.
pub fn theme(dark: bool, id: &str) -> &'static ThemeSpec {
    let list = if dark { DARK_THEMES } else { LIGHT_THEMES };
    list.iter().find(|t| t.id == id).unwrap_or(&list[0])
}

#[derive(Clone, Copy)]
pub struct AppPalette {
    pub dark: bool,
    /// The window background the islands float on.
    pub canvas: Hsla,
    pub background: Hsla,
    pub sidebar: Hsla,
    pub surface: Hsla,
    pub hover: Hsla,
    pub active: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_faint: Hsla,
    pub brand: Hsla,
    pub brand_soft: Hsla,
    /// Text and icons on a `brand` background: near-black or white, whichever reads better.
    pub on_brand: Hsla,
    pub danger: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
}

impl AppPalette {
    pub fn dark() -> Self {
        Self::from_theme(&DARK_THEMES[0])
    }

    pub fn light() -> Self {
        Self::from_theme(&LIGHT_THEMES[0])
    }

    pub fn from_theme(t: &ThemeSpec) -> Self {
        let (ink, glare) = if t.dark { (0xffffff, 1.) } else { (0x000000, 0.8) };
        Self {
            dark: t.dark,
            canvas: hex(t.canvas),
            background: hex(t.background),
            sidebar: hex(t.sidebar),
            surface: hex(t.surface),
            hover: alpha(ink, 0.05 * glare),
            active: alpha(ink, 0.08 * glare + if t.dark { 0. } else { 0.006 }),
            border: alpha(ink, if t.dark { 0.07 } else { 0.09 }),
            text: hex(t.text),
            text_muted: hex(t.text_muted),
            text_faint: hex(t.text_faint),
            brand: hex(t.accent),
            brand_soft: alpha(t.accent, if t.dark { 0.16 } else { 0.10 }),
            on_brand: hex(on_color(t.accent)),
            danger: hex(if t.dark { 0xf87171 } else { 0xdc2626 }),
            success: hex(if t.dark { 0x4ade80 } else { 0x16a34a }),
            // amber-700 in light themes: 5:1 on white, so warning text reads.
            warning: hex(if t.dark { 0xfbbf24 } else { 0xb45309 }),
        }
    }

    /// Let the window backdrop (Mica) show through. `t` is 0 (opaque) to 1. The canvas
    /// becomes almost clear, islands half-clear; dialogs (`surface`) stay opaque.
    pub fn translucent(mut self, t: f32) -> Self {
        let t = t.clamp(0., 1.);
        self.canvas.a *= 1. - 0.92 * t;
        self.sidebar.a *= 1. - 0.55 * t;
        self.background.a *= 1. - 0.55 * t;
        self
    }
}

/// Near-black or white, whichever has more contrast on `bg`.
fn on_color(bg: u32) -> u32 {
    const INK: u32 = 0x1c1917;
    if contrast(INK, bg) >= contrast(0xffffff, bg) { INK } else { 0xffffff }
}

/// WCAG relative luminance of `0xrrggbb`.
pub fn luminance(v: u32) -> f32 {
    let channel = |c: u32| {
        let c = c as f32 / 255.;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * channel((v >> 16) & 0xff) + 0.7152 * channel((v >> 8) & 0xff) + 0.0722 * channel(v & 0xff)
}

/// WCAG contrast ratio of two `0xrrggbb` colours.
pub fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[derive(Clone, PartialEq)]
pub struct TerminalPalette {
    pub dark: bool,
    pub background: Rgb,
    /// Opacity of the terminal's own background (below 1 when the backdrop shows through).
    pub background_alpha: f32,
    pub foreground: Rgb,
    pub cursor: Rgb,
    pub selection: Hsla,
    pub ansi: [Rgb; 16],
}

impl TerminalPalette {
    pub fn dark() -> Self {
        Self::from_theme(&DARK_THEMES[0])
    }

    pub fn light() -> Self {
        Self::from_theme(&LIGHT_THEMES[0])
    }

    /// The theme's terminal background and text with the shared ANSI colours of its mode,
    /// which are tuned to read on all of that mode's backgrounds.
    pub fn from_theme(t: &ThemeSpec) -> Self {
        Self {
            dark: t.dark,
            background: rgb8(t.term_background),
            background_alpha: 1.,
            foreground: rgb8(t.term_foreground),
            cursor: rgb8(t.text),
            selection: alpha(t.accent, if t.dark { 0.30 } else { 0.20 }),
            ansi: if t.dark { DARK_ANSI } else { LIGHT_ANSI },
        }
    }

    /// The terminal keeps more of its background than the islands so text stays readable.
    pub fn translucent(mut self, t: f32) -> Self {
        self.background_alpha = 1. - 0.45 * t.clamp(0., 1.);
        self
    }

    pub fn background_hsla(&self) -> Hsla {
        let mut c = rgb_to_hsla(self.background);
        c.a = self.background_alpha;
        c
    }

    /// Resolve a cell colour, honouring palette changes the program made (OSC 4/10/11).
    pub fn resolve(&self, color: Color, overrides: &Colors) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Indexed(index) => overrides[index as usize].unwrap_or_else(|| self.indexed(index)),
            Color::Named(named) => overrides[named].unwrap_or_else(|| self.named(named)),
        }
    }

    /// The colour a program asks about with OSC 4 (0–255), 10 (256, text), 11 (257,
    /// background) or 12 (258, cursor), using alacritty's numbering.
    pub fn color_at(&self, index: usize) -> Rgb {
        match index {
            0..=255 => self.indexed(index as u8),
            i if i == NamedColor::Background as usize => self.background,
            i if i == NamedColor::Cursor as usize => self.cursor,
            _ => self.foreground,
        }
    }

    fn named(&self, named: NamedColor) -> Rgb {
        use NamedColor::*;
        match named {
            Foreground | BrightForeground => self.foreground,
            Background => self.background,
            Cursor => self.cursor,
            DimForeground => dim(self.foreground),
            DimBlack => dim(self.ansi[0]),
            DimRed => dim(self.ansi[1]),
            DimGreen => dim(self.ansi[2]),
            DimYellow => dim(self.ansi[3]),
            DimBlue => dim(self.ansi[4]),
            DimMagenta => dim(self.ansi[5]),
            DimCyan => dim(self.ansi[6]),
            DimWhite => dim(self.ansi[7]),
            other => self.ansi[(other as usize).min(15)],
        }
    }

    fn indexed(&self, index: u8) -> Rgb {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let i = index - 16;
                let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb { r: step(i / 36), g: step((i / 6) % 6), b: step(i % 6) }
            }
            _ => {
                let v = 8 + (index - 232) * 10;
                Rgb { r: v, g: v, b: v }
            }
        }
    }
}

fn dim(c: Rgb) -> Rgb {
    Rgb { r: (c.r as u16 * 2 / 3) as u8, g: (c.g as u16 * 2 / 3) as u8, b: (c.b as u16 * 2 / 3) as u8 }
}

/// The 12 Project color presets. All read well on light and dark backgrounds.
pub const PROJECT_COLORS: [(&str, u32); 12] = [
    ("slate", 0x64748b),
    ("red", 0xef4444),
    ("orange", 0xf97316),
    ("amber", 0xf59e0b),
    ("lime", 0x84cc16),
    ("green", 0x22c55e),
    ("teal", 0x14b8a6),
    ("cyan", 0x06b6d4),
    ("blue", 0x3b82f6),
    ("indigo", 0x6366f1),
    ("violet", 0x8b5cf6),
    ("pink", 0xec4899),
];

/// A preset name or `#rrggbb`. Unknown values fall back to slate.
pub fn project_color(value: &str) -> Hsla {
    if let Some(hexstr) = value.strip_prefix('#')
        && let Ok(v) = u32::from_str_radix(hexstr, 16)
    {
        return hex(v);
    }
    let v = PROJECT_COLORS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(value))
        .map(|(_, v)| *v)
        .unwrap_or(PROJECT_COLORS[0].1);
    hex(v)
}

pub fn hex(v: u32) -> Hsla {
    rgb(v).into()
}

pub fn alpha(v: u32, a: f32) -> Hsla {
    let mut c: Rgba = rgba((v << 8) | 0xff);
    c.a = a;
    c.into()
}

pub fn rgb_to_hsla(c: Rgb) -> Hsla {
    rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32).into()
}

#[rustfmt::skip]
const DARK_ANSI: [Rgb; 16] = [
    // Black and bright black are lifted so they stay visible on grey backgrounds.
    rgb8(0x3f3f46), rgb8(0xf87171), rgb8(0x4ade80), rgb8(0xfacc15),
    rgb8(0x60a5fa), rgb8(0xc084fc), rgb8(0x22d3ee), rgb8(0xd4d4d8),
    rgb8(0x6b6b74), rgb8(0xfca5a5), rgb8(0x86efac), rgb8(0xfde047),
    rgb8(0x93c5fd), rgb8(0xd8b4fe), rgb8(0x67e8f9), rgb8(0xfafafa),
];

#[rustfmt::skip]
const LIGHT_ANSI: [Rgb; 16] = [
    rgb8(0x27272a), rgb8(0xdc2626), rgb8(0x15803d), rgb8(0xa16207),
    rgb8(0x1d4ed8), rgb8(0x7e22ce), rgb8(0x0e7490), rgb8(0xa1a1aa),
    rgb8(0x52525b), rgb8(0xef4444), rgb8(0x16a34a), rgb8(0xca8a04),
    rgb8(0x2563eb), rgb8(0x9333ea), rgb8(0x0891b2), rgb8(0x71717a),
];

const fn rgb8(v: u32) -> Rgb {
    Rgb { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_colors_accept_presets_and_hex() {
        assert_eq!(project_color("red"), hex(0xef4444));
        assert_eq!(project_color("#123456"), hex(0x123456));
        assert_eq!(project_color("nonsense"), hex(0x64748b));
    }

    /// Every theme keeps its text readable: body and secondary text at 4.5:1 or better on
    /// the islands and the sidebar, the accent at 3:1 (it marks controls and links), and the
    /// terminal text at 7:1.
    #[test]
    fn themes_are_readable() {
        for t in DARK_THEMES.iter().chain(LIGHT_THEMES) {
            for bg in [t.background, t.sidebar] {
                for (what, fg, min) in [("text", t.text, 7.), ("muted", t.text_muted, 4.5), ("faint", t.text_faint, 4.5), ("accent", t.accent, 3.)] {
                    let c = contrast(fg, bg);
                    assert!(c >= min, "{} ({}): {what} {fg:06x} on {bg:06x} is {c:.2}:1", t.name, if t.dark { "dark" } else { "light" });
                }
            }
            let c = contrast(t.term_foreground, t.term_background);
            assert!(c >= 7., "{}: terminal text is {c:.2}:1", t.name);
            let c = contrast(on_color(t.accent), t.accent);
            assert!(c >= 4.5, "{}: button text on accent is {c:.2}:1", t.name);
        }
    }

    #[test]
    fn theme_ids_are_unique_and_unknown_ids_fall_back() {
        for list in [DARK_THEMES, LIGHT_THEMES] {
            let mut ids: Vec<_> = list.iter().map(|t| t.id).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), list.len());
        }
        assert_eq!(theme(true, "nope").id, "ember");
        assert_eq!(theme(false, "sky").name, "Sky");
    }

    #[test]
    fn xterm_256_cube_and_grays() {
        let p = TerminalPalette::dark();
        assert_eq!(p.indexed(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(p.indexed(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(p.indexed(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(p.indexed(255), Rgb { r: 238, g: 238, b: 238 });
    }

    #[test]
    fn color_queries_use_xterm_numbering() {
        let p = TerminalPalette::dark();
        assert_eq!(p.color_at(1), p.ansi[1]);
        assert_eq!(p.color_at(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(p.color_at(NamedColor::Foreground as usize), p.foreground);
        assert_eq!(p.color_at(NamedColor::Background as usize), p.background);
        assert_eq!(p.color_at(NamedColor::Cursor as usize), p.cursor);
    }
}

/// The active App Theme, Terminal Theme and terminal font, shared by all views.
pub struct Appearance {
    pub app: AppPalette,
    pub term: TerminalPalette,
    pub font_family: gpui::SharedString,
    pub font_size: f32,
}

impl gpui::Global for Appearance {}

pub const UI_FONT: &str = "Inter";
/// Serif used for the KOMANDO wordmark, matching the KO logo.
pub const DISPLAY_FONT: &str = "Young Serif";
