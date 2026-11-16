//! Terminal Colors: named terminal palettes picked per mode, independently of the Theme.
//!
//! Default uses VS Code's default terminal colors (Dark+ and Light+ backgrounds). Campbell,
//! One Half, Solarized and Tango use Windows Terminal's built-in schemes; Dracula, Nord and
//! Gruvbox use their projects' published palettes. Selection colors come from each
//! project where it defines one, otherwise they are chosen to suit the background.

/// The Terminal Colors id that derives the terminal from the Theme of the same mode.
pub const MATCH_THEME: &str = "theme";
/// Selected for new settings and when a saved id is unknown.
pub const DEFAULT_TERMINAL_COLORS: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalColors {
    pub id: &'static str,
    pub name: &'static str,
    pub dark: bool,
    pub background: u32,
    pub foreground: u32,
    pub cursor: u32,
    pub selection: u32,
    /// Black, red, green, yellow, blue, magenta, cyan, white, then their bright variants.
    pub ansi: [u32; 16],
}

const fn colors(
    id: &'static str, name: &'static str, dark: bool,
    [background, foreground, cursor, selection]: [u32; 4], ansi: [u32; 16],
) -> TerminalColors {
    TerminalColors { id, name, dark, background, foreground, cursor, selection, ansi }
}

/// Dark-mode Terminal Colors. The first is the default.
#[rustfmt::skip]
pub const DARK_TERMINAL_COLORS: &[TerminalColors] = &[
    colors("default", "Default", true, [0x1e1e1e, 0xcccccc, 0xffffff, 0x264f78], [
        0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5,
        0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xe5e5e5,
    ]),
    colors("campbell", "Campbell", true, [0x0c0c0c, 0xcccccc, 0xffffff, 0x4a4a4a], [
        0x0c0c0c, 0xc50f1f, 0x13a10e, 0xc19c00, 0x0037da, 0x881798, 0x3a96dd, 0xcccccc,
        0x767676, 0xe74856, 0x16c60c, 0xf9f1a5, 0x3b78ff, 0xb4009e, 0x61d6d6, 0xf2f2f2,
    ]),
    colors("one-half", "One Half Dark", true, [0x282c34, 0xdcdfe4, 0xffffff, 0x474e5d], [
        0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
        0x5a6374, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
    ]),
    colors("solarized", "Solarized Dark", true, [0x002b36, 0x839496, 0x93a1a1, 0x073642], [
        0x002b36, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
        0x073642, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
    ]),
    colors("tango", "Tango Dark", true, [0x000000, 0xd3d7cf, 0xffffff, 0x404040], [
        0x000000, 0xcc0000, 0x4e9a06, 0xc4a000, 0x3465a4, 0x75507b, 0x06989a, 0xd3d7cf,
        0x555753, 0xef2929, 0x8ae234, 0xfce94f, 0x729fcf, 0xad7fa8, 0x34e2e2, 0xeeeeec,
    ]),
    colors("gruvbox", "Gruvbox Dark", true, [0x282828, 0xebdbb2, 0xebdbb2, 0x504945], [
        0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
        0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
    ]),
    colors("dracula", "Dracula", true, [0x282a36, 0xf8f8f2, 0xf8f8f2, 0x44475a], [
        0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
        0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
    ]),
    colors("nord", "Nord", true, [0x2e3440, 0xd8dee9, 0xd8dee9, 0x434c5e], [
        0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
        0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
    ]),
];

/// Light-mode Terminal Colors. The first is the default. A shared id marks the same
/// family's dark variant, so one name can be picked in both modes.
#[rustfmt::skip]
pub const LIGHT_TERMINAL_COLORS: &[TerminalColors] = &[
    colors("default", "Default", false, [0xffffff, 0x333333, 0x000000, 0xadd6ff], [
        0x000000, 0xcd3131, 0x107c10, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555,
        0x666666, 0xcd3131, 0x14ce14, 0xb5ba00, 0x0451a5, 0xbc05bc, 0x0598bc, 0xa5a5a5,
    ]),
    colors("one-half", "One Half Light", false, [0xfafafa, 0x383a42, 0x4f525d, 0xbfceff], [
        0x383a42, 0xe45649, 0x50a14f, 0xc18301, 0x0184bc, 0xa626a4, 0x0997b3, 0xfafafa,
        0x4f525d, 0xdf6c75, 0x98c379, 0xe4c07a, 0x61afef, 0xc577dd, 0x56b5c1, 0xffffff,
    ]),
    colors("solarized", "Solarized Light", false, [0xfdf6e3, 0x657b83, 0x586e75, 0xeee8d5], [
        0x002b36, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
        0x073642, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
    ]),
    colors("tango", "Tango Light", false, [0xffffff, 0x555753, 0x000000, 0xb5d5ff], [
        0x000000, 0xcc0000, 0x4e9a06, 0xc4a000, 0x3465a4, 0x75507b, 0x06989a, 0xd3d7cf,
        0x555753, 0xef2929, 0x8ae234, 0xfce94f, 0x729fcf, 0xad7fa8, 0x34e2e2, 0xeeeeec,
    ]),
    colors("gruvbox", "Gruvbox Light", false, [0xfbf1c7, 0x3c3836, 0x3c3836, 0xd5c4a1], [
        0xfbf1c7, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0x7c6f64,
        0x928374, 0x9d0006, 0x79740e, 0xb57614, 0x076678, 0x8f3f71, 0x427b58, 0x3c3836,
    ]),
];

pub fn terminal_colors_list(dark: bool) -> &'static [TerminalColors] {
    if dark { DARK_TERMINAL_COLORS } else { LIGHT_TERMINAL_COLORS }
}

/// The Terminal Colors with this id in the requested mode. Unknown ids, and ids that exist
/// only in the other mode, resolve to Default. `MATCH_THEME` is not resolved here.
pub fn terminal_colors(dark: bool, id: &str) -> &'static TerminalColors {
    let list = terminal_colors_list(dark);
    list.iter().find(|c| c.id == id).unwrap_or(&list[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{TerminalPalette, contrast, luminance};

    #[test]
    fn ids_are_unique_modes_match_backgrounds_and_default_comes_first() {
        for dark in [true, false] {
            let list = terminal_colors_list(dark);
            assert_eq!(list[0].id, DEFAULT_TERMINAL_COLORS);
            let mut ids = std::collections::HashSet::new();
            for c in list {
                assert!(ids.insert(c.id), "duplicate {}", c.id);
                assert_ne!(c.id, MATCH_THEME);
                assert_eq!(c.dark, dark, "{} is listed in the wrong mode", c.name);
                assert_eq!(luminance(c.background) < 0.2, dark, "{} background", c.name);
            }
        }
    }

    #[test]
    fn unknown_and_other_mode_ids_fall_back_to_default() {
        assert_eq!(terminal_colors(true, "nope").id, DEFAULT_TERMINAL_COLORS);
        assert_eq!(terminal_colors(false, "dracula").id, DEFAULT_TERMINAL_COLORS);
        assert_eq!(terminal_colors(false, "solarized").name, "Solarized Light");
        assert_eq!(terminal_colors(true, "solarized").name, "Solarized Dark");
    }

    #[test]
    fn palettes_keep_ansi_colors_and_readable_text_and_selection() {
        for c in DARK_TERMINAL_COLORS.iter().chain(LIGHT_TERMINAL_COLORS) {
            let p = TerminalPalette::from_colors(c);
            assert_eq!(p.dark, c.dark);
            assert_eq!(packed(p.background), c.background, "{}", c.name);
            for (slot, &color) in c.ansi.iter().enumerate() {
                assert_eq!(packed(p.ansi[slot]), color, "{} ANSI {slot} is copied unchanged", c.name);
            }
            assert!(contrast(packed(p.foreground), c.background) >= 4.48, "{} text", c.name);
            assert!(contrast(packed(p.selection_foreground), c.selection) >= 4.48, "{} selection", c.name);
            assert!(contrast(packed(p.cursor), c.background) >= 2.98, "{} cursor", c.name);
        }
    }

    fn packed(c: alacritty_terminal::vte::ansi::Rgb) -> u32 {
        (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
    }
}
