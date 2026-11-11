//! Colours: the App Theme (Komando's chrome), the Terminal Theme (terminal content), and the
//! Project color presets. The two themes are independent settings.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::{Hsla, Rgba, rgb, rgba};

/// KOMANDO brand purple. Only for Komando's own chrome, never for Project colors.
pub const BRAND: u32 = 0x7c3aed;

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
    pub danger: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
}

impl AppPalette {
    pub fn dark() -> Self {
        Self {
            dark: true,
            canvas: hex(0x050506),
            background: hex(0x0c0c0e),
            sidebar: hex(0x111114),
            surface: hex(0x18181b),
            hover: alpha(0xffffff, 0.05),
            active: alpha(0xffffff, 0.08),
            border: alpha(0xffffff, 0.07),
            text: hex(0xececef),
            text_muted: hex(0xa1a1aa),
            text_faint: hex(0x63636b),
            brand: hex(0x8b5cf6),
            brand_soft: alpha(0x8b5cf6, 0.16),
            danger: hex(0xf87171),
            success: hex(0x4ade80),
            warning: hex(0xfbbf24),
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

    pub fn light() -> Self {
        Self {
            dark: false,
            canvas: hex(0xe8e8eb),
            background: hex(0xffffff),
            sidebar: hex(0xfafafa),
            surface: hex(0xffffff),
            hover: alpha(0x000000, 0.04),
            active: alpha(0x000000, 0.07),
            border: alpha(0x000000, 0.09),
            text: hex(0x18181b),
            text_muted: hex(0x52525b),
            text_faint: hex(0xa1a1aa),
            brand: hex(BRAND),
            brand_soft: alpha(BRAND, 0.10),
            danger: hex(0xdc2626),
            success: hex(0x16a34a),
            warning: hex(0xd97706),
        }
    }
}

#[derive(Clone)]
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
        Self {
            dark: true,
            background: rgb8(0x0b0b0d),
            background_alpha: 1.,
            foreground: rgb8(0xd4d4d8),
            cursor: rgb8(0xe4e4e7),
            selection: alpha(0x8b5cf6, 0.35),
            ansi: [
                rgb8(0x27272a), rgb8(0xf87171), rgb8(0x4ade80), rgb8(0xfacc15),
                rgb8(0x60a5fa), rgb8(0xc084fc), rgb8(0x22d3ee), rgb8(0xd4d4d8),
                rgb8(0x52525b), rgb8(0xfca5a5), rgb8(0x86efac), rgb8(0xfde047),
                rgb8(0x93c5fd), rgb8(0xd8b4fe), rgb8(0x67e8f9), rgb8(0xfafafa),
            ],
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            background: rgb8(0xfcfcfb),
            background_alpha: 1.,
            foreground: rgb8(0x27272a),
            cursor: rgb8(0x27272a),
            selection: alpha(0x7c3aed, 0.20),
            ansi: [
                rgb8(0x27272a), rgb8(0xdc2626), rgb8(0x15803d), rgb8(0xa16207),
                rgb8(0x1d4ed8), rgb8(0x7e22ce), rgb8(0x0e7490), rgb8(0xa1a1aa),
                rgb8(0x52525b), rgb8(0xef4444), rgb8(0x16a34a), rgb8(0xca8a04),
                rgb8(0x2563eb), rgb8(0x9333ea), rgb8(0x0891b2), rgb8(0x71717a),
            ],
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

    #[test]
    fn xterm_256_cube_and_grays() {
        let p = TerminalPalette::dark();
        assert_eq!(p.indexed(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(p.indexed(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(p.indexed(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(p.indexed(255), Rgb { r: 238, g: 238, b: 238 });
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
