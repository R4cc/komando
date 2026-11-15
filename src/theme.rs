//! Colours: the App Theme (Komando's chrome), the Terminal Theme (terminal content), and the
//! Project color presets. The two themes are independent settings.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::{Hsla, Rgba, rgb, rgba};

mod catalog;
mod components;
use std::sync::LazyLock;

pub const DEFAULT_THEME: &str = "komando-default";

macro_rules! color_tokens {
    ($($field:ident),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct ThemeColors { $(pub $field: u32,)+ }
        impl ThemeColors {
            fn read(color: &impl Fn(&str) -> u32) -> Self {
                Self { $($field: color(&stringify!($field).replace('_', "-")),)+ }
            }
        }
    };
}

color_tokens! {
    background, foreground, card, card_foreground, popover, popover_foreground,
    primary, primary_foreground, secondary, secondary_foreground, muted, muted_foreground,
    accent, accent_foreground, destructive, destructive_foreground, border, input, ring,
    sidebar, sidebar_foreground, sidebar_primary, sidebar_primary_foreground,
    sidebar_accent, sidebar_accent_foreground, sidebar_border, sidebar_ring,
    chart_1, chart_2, chart_3, chart_4, chart_5,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeShadow {
    pub color: u32,
    pub opacity: f32,
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
}

/// One native variant of an embedded source theme. Layout and terminal font overrides
/// remain user settings; source colors, UI fonts, radii and shadows live here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub source: &'static str,
    pub dark: bool,
    pub colors: ThemeColors,
    pub font_sans: &'static str,
    pub font_mono: &'static str,
    pub radius: f32,
    pub shadow: ThemeShadow,
}

pub static DARK_THEMES: LazyLock<Vec<ThemeSpec>> = LazyLock::new(|| catalog::load(true));
pub static LIGHT_THEMES: LazyLock<Vec<ThemeSpec>> = LazyLock::new(|| catalog::load(false));

pub fn themes(dark: bool) -> &'static [ThemeSpec] {
    if dark { &DARK_THEMES } else { &LIGHT_THEMES }
}

/// Removed and unknown theme IDs resolve to Komando Default in the requested mode.
pub fn theme(dark: bool, id: &str) -> &'static ThemeSpec {
    let list = themes(dark);
    list.iter().find(|t| t.id == id).unwrap_or(&list[0])
}

#[derive(Clone, Copy)]
pub struct AppPalette {
    pub dark: bool,
    pub font_family: &'static str,
    pub mono_family: &'static str,
    pub radius: f32,
    pub shadow: ThemeShadow,
    pub canvas: Hsla,
    pub background: Hsla,
    pub sidebar: Hsla,
    pub surface: Hsla,
    pub surface_text: Hsla,
    pub popover: Hsla,
    pub popover_text: Hsla,
    pub hover: Hsla,
    pub hover_text: Hsla,
    pub muted: Hsla,
    pub muted_text: Hsla,
    pub active: Hsla,
    pub active_text: Hsla,
    pub border: Hsla,
    pub input: Hsla,
    pub focus: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_faint: Hsla,
    pub brand: Hsla,
    pub brand_soft: Hsla,
    pub on_brand: Hsla,
    pub secondary: Hsla,
    pub on_secondary: Hsla,
    pub danger: Hsla,
    pub on_danger: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    navigation: ThemeColors,
}

impl AppPalette {
    pub fn dark() -> Self { Self::from_theme(&DARK_THEMES[0]) }
    pub fn light() -> Self { Self::from_theme(&LIGHT_THEMES[0]) }

    pub fn from_theme(t: &ThemeSpec) -> Self {
        let c = t.colors;
        Self {
            dark: t.dark,
            font_family: t.font_sans, mono_family: t.font_mono, radius: t.radius, shadow: t.shadow,
            canvas: hex(c.background), background: hex(c.background), sidebar: hex(c.sidebar),
            surface: hex(c.card), surface_text: hex(readable(c.card_foreground, c.card, 4.5)),
            popover: hex(c.popover), popover_text: hex(readable(c.popover_foreground, c.popover, 4.5)),
            hover: hex(c.accent), hover_text: hex(readable(c.accent_foreground, c.accent, 4.5)),
            muted: hex(c.muted), muted_text: hex(readable(c.muted_foreground, c.muted, 4.5)),
            active: hex(c.accent), active_text: hex(readable(c.accent_foreground, c.accent, 4.5)),
            border: hex(c.border), input: hex(c.input), focus: hex(c.ring),
            text: hex(readable(c.foreground, c.background, 4.5)),
            text_muted: hex(readable(c.muted_foreground, c.background, 4.5)),
            text_faint: hex(readable(c.muted_foreground, c.background, 4.5)),
            brand: hex(c.primary), on_brand: hex(readable(c.primary_foreground, c.primary, 4.5)),
            brand_soft: hex(c.accent), secondary: hex(c.secondary),
            on_secondary: hex(readable(c.secondary_foreground, c.secondary, 4.5)),
            danger: hex(c.destructive), on_danger: hex(readable(c.destructive_foreground, c.destructive, 4.5)),
            success: hex(if t.dark { 0x86b98b } else { 0x347044 }),
            warning: hex(if t.dark { 0xd9bc7b } else { 0x865a12 }), navigation: c,
        }
    }

    /// Apply sidebar-specific foreground, selection, primary, border and focus tokens.
    pub fn navigation(mut self) -> Self {
        let c = self.navigation;
        self.background = self.sidebar;
        self.text = hex(readable(c.sidebar_foreground, c.sidebar, 4.5));
        self.text_muted = self.text;
        self.text_faint = self.text.opacity(0.8);
        self.hover = hex(c.sidebar_accent);
        self.hover_text = hex(readable(c.sidebar_accent_foreground, c.sidebar_accent, 4.5));
        self.active = self.hover;
        self.active_text = self.hover_text;
        self.brand_soft = self.hover;
        self.brand = hex(c.sidebar_primary);
        self.on_brand = hex(readable(c.sidebar_primary_foreground, c.sidebar_primary, 4.5));
        self.border = hex(c.sidebar_border);
        self.focus = hex(c.sidebar_ring);
        self
    }

    pub fn panel(mut self) -> Self {
        let c = self.navigation;
        self.background = self.surface;
        self.text = self.surface_text;
        self.text_muted = hex(readable(c.muted_foreground, c.card, 4.5));
        self.text_faint = self.text_muted;
        self.brand = hex(c.primary);
        self.on_brand = hex(readable(c.primary_foreground, c.primary, 4.5));
        self.border = hex(c.border);
        self.focus = hex(c.ring);
        self.hover = hex(c.accent);
        self.hover_text = hex(readable(c.accent_foreground, c.accent, 4.5));
        self.active = self.hover;
        self.active_text = self.hover_text;
        self.brand_soft = self.hover;
        self
    }

    pub fn highlighted(mut self) -> Self {
        self.background = self.active;
        self.text = self.active_text;
        self.text_muted = self.active_text;
        self.text_faint = self.active_text;
        self.brand = self.active_text;
        self
    }

    /// Preserve native size hierarchy while taking the source theme's base corner radius.
    pub fn corner(self, base: f32) -> gpui::Pixels { gpui::px((base + self.radius - 8.).max(0.)) }

    pub fn translucent(mut self, t: f32) -> Self {
        let t = t.clamp(0., 1.);
        self.canvas.a *= 1. - 0.92 * t;
        self.sidebar.a *= 1. - 0.55 * t;
        self.background.a *= 1. - 0.55 * t;
        self
    }
}

/// Preserve supplied foregrounds when readable. Otherwise minimally move them towards
/// black or white; never change a source background or lose a foreground/background pair.
fn readable(fg: u32, bg: u32, minimum: f32) -> u32 {
    if contrast(fg, bg) >= minimum { return fg; }
    let target: u32 = if contrast(0, bg) > contrast(0xffffff, bg) { 0 } else { 0xffffff };
    let mix = |amount: f32| {
        let ch = |shift: u32| {
            let a = ((fg >> shift) & 255u32) as f32;
            let b = ((target >> shift) & 255u32) as f32;
            (a + (b - a) * amount).round() as u32
        };
        (ch(16) << 16) | (ch(8) << 8) | ch(0)
    };
    let (mut low, mut high) = (0., 1.);
    for _ in 0..16 {
        let mid = (low + high) / 2.;
        if contrast(mix(mid), bg) >= minimum { high = mid; } else { low = mid; }
    }
    mix(high)
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
    pub selection_foreground: Rgb,
    pub ansi: [Rgb; 16],
}

impl TerminalPalette {
    pub fn dark() -> Self {
        Self::from_theme(&DARK_THEMES[0])
    }

    pub fn light() -> Self {
        Self::from_theme(&LIGHT_THEMES[0])
    }

    /// Theme-specific terminal neutrals and optional ANSI colours. Reds, greens, etc.
    /// retain their meaning in programs across all themes.
    pub fn from_theme(t: &ThemeSpec) -> Self {
        let c = t.colors;
        let dark_surface = luminance(c.background) < 0.2;
        let mut ansi = if dark_surface { DARK_ANSI } else { LIGHT_ANSI };
        // Gently tint semantic ANSI hues towards the theme, then keep colored text readable.
        // Programs still get red/green/blue meanings, rather than a primary hue in every slot.
        for (_, color) in ansi.iter_mut().enumerate().filter(|(i, _)| i % 8 != 0 && i % 8 != 7) {
            let blend = |channel: u8, shift: u32| ((channel as u32 * 9 + ((c.primary >> shift) & 255)) / 10) as u8;
            let tinted = Rgb { r: blend(color.r, 16), g: blend(color.g, 8), b: blend(color.b, 0) };
            let packed = ((tinted.r as u32) << 16) | ((tinted.g as u32) << 8) | tinted.b as u32;
            *color = rgb8(readable(packed, c.background, 4.5));
        }
        ansi[0] = rgb8(if dark_surface { readable(0x454850, c.background, 1.5) } else { c.foreground });
        ansi[7] = rgb8(c.foreground);
        ansi[8] = rgb8(readable(c.muted_foreground, c.background, 4.5));
        ansi[15] = rgb8(c.foreground);
        Self {
            dark: dark_surface,
            background: rgb8(c.background), background_alpha: 1.,
            foreground: rgb8(readable(c.foreground, c.background, 4.5)),
            cursor: rgb8(readable(c.primary, c.background, 3.)),
            selection: hex(c.accent), selection_foreground: rgb8(readable(c.accent_foreground, c.accent, 4.5)), ansi,
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
        && hexstr.len() == 6
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
    // Semantic colours are shared; from_theme replaces the neutral slots.
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

    fn value(h: Hsla) -> u32 {
        let c: Rgba = h.into();
        let ch = |c: f32| (c * 255.).round() as u32;
        (ch(c.r) << 16) | (ch(c.g) << 8) | ch(c.b)
    }

    #[test]
    fn catalog_has_all_nine_sources_in_both_modes() {
        let expected = ["Komando Default", "Sage Green", "Noctua", "AstroVista", "QraftHive 1.0", "Gold?", "Mint Theme", "FL-Theme", "Salesforce"];
        for dark in [false, true] {
            let list = themes(dark);
            assert_eq!(list.len(), 9);
            let mut ids = std::collections::HashSet::new();
            for (t, name) in list.iter().zip(expected) {
                assert_eq!(t.name, name);
                assert_eq!(t.dark, dark);
                assert!(ids.insert(t.id));
                assert!(t.source.starts_with("https://tweakcn.com/themes/"));
                assert!(!t.font_sans.is_empty() && !t.font_mono.is_empty());
                assert!(t.radius.is_finite() && t.radius >= 0.);
            }
        }
    }

    #[test]
    fn removed_ids_and_missing_settings_resolve_to_komando_default() {
        let settings: crate::model::Settings = toml::from_str("").unwrap();
        assert_eq!(settings.dark_theme, DEFAULT_THEME);
        assert_eq!(settings.light_theme, DEFAULT_THEME);
        assert!(settings.font_family.is_empty());
        for dark in [false, true] {
            for old in ["graphite", "ember", "ivory", "nope"] {
                assert_eq!(theme(dark, old).id, DEFAULT_THEME);
            }
        }
        let mut saved = settings;
        saved.dark_theme = "noctua".into();
        saved.light_theme = "salesforce".into();
        saved.font_family = "Cascadia Mono".into();
        let loaded: crate::model::Settings = toml::from_str(&toml::to_string(&saved).unwrap()).unwrap();
        assert_eq!(saved, loaded);
        assert_eq!(theme(true, &loaded.dark_theme).name, "Noctua");
    }

    #[test]
    fn native_roles_preserve_distinct_source_colors_and_styles() {
        let t = theme(true, "qrafthive");
        let p = AppPalette::from_theme(t);
        let nav = p.navigation();
        assert_eq!(value(p.brand), 0xc1253d);
        assert_eq!(value(nav.brand), 0x1da1f2);
        assert_eq!(value(nav.panel().brand), 0xc1253d);
        assert_eq!(value(p.secondary), 0xf0f3f4);
        assert_eq!(value(p.hover), 0x061622);
        assert_eq!(value(p.popover), 0x202020);
        assert_eq!(p.font_family, "Outfit");
        assert_eq!(p.mono_family, "JetBrains Mono");
        assert_eq!(p.corner(8.), gpui::px(8.));
        let default = AppPalette::dark();
        assert_eq!(default.font_family, "Outfit");
        assert_eq!(default.mono_family, "Geist Mono");
        assert_eq!(default.corner(8.), gpui::px(16.));
        assert_eq!(default.shadow.blur, 3.);
        assert_eq!(default.shadow.opacity, 0.1);
        // FL-Theme publishes a light background for both modes; keep it faithfully.
        assert_eq!(theme(true, "fl-theme").colors.background, 0xf1f5f8);
        assert!(!TerminalPalette::from_theme(theme(true, "fl-theme")).dark);
    }

    #[test]
    fn effective_foreground_pairs_and_terminal_selections_are_readable() {
        for t in themes(false).iter().chain(themes(true)) {
            let p = AppPalette::from_theme(t);
            let nav = p.navigation();
            let term = TerminalPalette::from_theme(t);
            for (fg, bg) in [(p.text, p.background), (p.text_muted, p.background), (p.surface_text, p.surface),
                (p.popover_text, p.popover), (p.on_brand, p.brand), (p.on_secondary, p.secondary),
                (p.active_text, p.active), (p.muted_text, p.muted), (p.on_danger, p.danger), (nav.text, nav.background), (nav.active_text, nav.active)] {
                assert!(contrast(value(fg), value(bg)) >= 4.48, "{} {} contrast", t.name, t.dark);
            }
            assert!(contrast(value(rgb_to_hsla(term.foreground)), value(rgb_to_hsla(term.background))) >= 4.48);
            assert!(contrast(value(rgb_to_hsla(term.selection_foreground)), value(term.selection)) >= 4.48);
            for index in [1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14] {
                assert!(contrast(value(rgb_to_hsla(term.ansi[index])), t.colors.background) >= 4.48, "{} ANSI {index}", t.name);
            }
        }
    }

    #[test]
    fn source_styles_reach_library_controls_and_reset_when_switching() {
        let mut library = gpui_component::Theme::default();
        for id in ["noctua", "mint", "komando-default", "salesforce"] {
            let p = AppPalette::from_theme(theme(true, id));
            p.apply_components(&mut library);
            assert_eq!(library.font_family.as_ref(), p.font_family);
            assert_eq!(library.radius, gpui::px(p.radius));
            assert_eq!(library.shadow, p.shadow.opacity > 0.);
            assert_eq!(library.colors.popover, p.popover);
            assert_eq!(library.colors.primary, p.brand);
            assert_eq!(library.colors.button_primary_foreground, p.on_brand);
            assert_eq!(library.colors.secondary, p.secondary);
            assert_eq!(library.colors.ring, p.focus);
            assert_eq!(crate::ui::widgets::elevation(p).is_empty(), p.shadow.opacity == 0.);
        }
    }

    #[test]
    fn xterm_extended_colors_and_color_queries_remain_standard() {
        for p in [TerminalPalette::dark(), TerminalPalette::light()] {
            assert_eq!(p.indexed(16), rgb8(0));
            assert_eq!(p.indexed(196), rgb8(0xff0000));
            assert_eq!(p.indexed(232), rgb8(0x080808));
            assert_eq!(p.indexed(255), rgb8(0xeeeeee));
            assert_eq!(p.color_at(1), p.ansi[1]);
            assert_eq!(p.color_at(NamedColor::Foreground as usize), p.foreground);
            assert_eq!(p.color_at(NamedColor::Background as usize), p.background);
            assert_eq!(p.color_at(NamedColor::Cursor as usize), p.cursor);
        }
    }

    #[test]
    fn project_colors_are_independent_of_theme() {
        assert_eq!(project_color("red"), hex(0xef4444));
        assert_eq!(project_color("#123456"), hex(0x123456));
        assert_eq!(project_color("nonsense"), hex(0x64748b));
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

pub const UI_FONT: &str = "Geist";
