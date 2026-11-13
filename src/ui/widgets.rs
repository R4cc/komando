//! Small building blocks shared by every screen: buttons, icons, elevation, labels.
//!
//! Type scale (px): 11 uppercase labels (+tracking), 12 meta, 13 UI text, 14 inputs and
//! terminal, 15 dialog titles. The logo is drawn, not set in a font. Radii are concentric: an inner element's
//! radius is the outer radius minus the padding between them.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, BoxShadow, ElementId, FontWeight, Hsla, IntoElement, ParentElement,
    SharedString, Stateful, Styled, Transformation, div, percentage, point, prelude::*, px, svg,
};

use crate::theme::{AppPalette, UI_FONT, alpha};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// A 32px-high text button. Hit area is the full pill.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: ButtonKind, p: AppPalette) -> Stateful<gpui::Div> {
    let label = label.into();
    button_shell(id, kind, p).0.aria_label(label.clone()).child(label)
}

/// A text button with a leading icon, drawn in the label's colour.
pub fn icon_text_button(
    id: impl Into<ElementId>,
    icon: &str,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    p: AppPalette,
) -> Stateful<gpui::Div> {
    let (el, fg) = button_shell(id, kind, p);
    let label = label.into();
    el.aria_label(label.clone()).gap(px(8.)).child(icon_el(icon, 15., fg)).child(label)
}

/// A button's pill without its content, and the colour its content is drawn in.
fn button_shell(id: impl Into<ElementId>, kind: ButtonKind, p: AppPalette) -> (Stateful<gpui::Div>, Hsla) {
    let (bg, fg, hover, border) = match kind {
        // Near-black on orange: white would read at only about 3:1.
        ButtonKind::Primary => (p.brand, p.on_brand, p.brand.opacity(0.88), None),
        ButtonKind::Secondary => (p.hover, p.text, p.active, Some(p.border)),
        ButtonKind::Ghost => (gpui::transparent_black(), p.text_muted, p.hover, None),
        // red-600 in both themes: white text on the lighter dark-theme red was 2.8:1.
        ButtonKind::Danger => {
            let red = crate::theme::hex(0xdc2626);
            (red, gpui::white(), red.opacity(0.88), None)
        }
    };
    let el = div()
        .id(id)
        .role(gpui::Role::Button)
        .tab_index(0)
        .focus_visible(move |s| s.border_2().border_color(p.brand))
        .h(px(32.))
        .px(px(14.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .bg(bg)
        .when_some(border, |el, b| el.border_1().border_color(b))
        .text_color(fg)
        .font_family(UI_FONT)
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(move |s| s.bg(hover));
    (el, fg)
}

/// A square icon-only button with a tooltip-free label for accessibility tooling.
pub fn icon_button(id: impl Into<ElementId>, icon: &str, p: AppPalette) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(icon.replace('-', " "))
        .tab_index(0)
        .focus_visible(move |s| s.border_2().border_color(p.brand))
        .size(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .text_color(p.text_muted)
        .cursor_pointer()
        .hover(move |s| s.bg(p.hover).text_color(p.text))
        .child(icon_el(icon, 16., p.text_muted))
}

/// The icon for a Local Shell's program: PowerShell 7, Windows PowerShell, Command Prompt, or
/// Tux for WSL and Unix shells. `None` for anything else, which gets the generic terminal icon.
pub fn shell_icon(program: &str) -> Option<&'static str> {
    let file = program.rsplit(['\\', '/']).next().unwrap_or(program).to_lowercase();
    let name = file.strip_suffix(".exe").unwrap_or(&file);
    match name {
        "pwsh" | "pwsh-preview" => Some("shell:pwsh"),
        "powershell" => Some("shell:powershell"),
        "cmd" => Some("shell:cmd"),
        "wsl" => Some("shell:linux"),
        // Git Bash and friends on Windows are not Linux; elsewhere these are the system's shell.
        "bash" | "zsh" | "fish" | "sh" | "dash" | "ksh" if !file.ends_with(".exe") => Some("shell:linux"),
        _ => None,
    }
}

/// The full-colour artwork behind a `shell:` icon name (from Windows Terminal's profile icons).
fn shell_image(name: &str) -> Option<std::sync::Arc<gpui::Image>> {
    use std::sync::{Arc, OnceLock};
    static IMAGES: OnceLock<Vec<(&'static str, Arc<gpui::Image>)>> = OnceLock::new();
    let images = IMAGES.get_or_init(|| {
        let png = |bytes: &[u8]| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes.to_vec()));
        vec![
            ("shell:pwsh", png(include_bytes!("../../assets/shells/pwsh.png"))),
            ("shell:powershell", png(include_bytes!("../../assets/shells/powershell.png"))),
            ("shell:cmd", png(include_bytes!("../../assets/shells/cmd.png"))),
            ("shell:linux", png(include_bytes!("../../assets/shells/linux.png"))),
        ]
    });
    images.iter().find(|(n, _)| *n == name).map(|(_, image)| image.clone())
}

/// Whether an icon name is full-colour artwork rather than a Lucide icon drawn in one colour.
pub fn is_artwork(name: &str) -> bool {
    name.starts_with("shell:")
}

/// A Lucide icon by name (e.g. "server"), drawn in `color`. `shell:` names are a shell's own
/// full-colour icon instead, which ignores `color`.
pub fn icon_el(name: &str, size: f32, color: Hsla) -> AnyElement {
    if let Some(image) = shell_image(name) {
        return gpui::img(image).size(px(size)).flex_shrink_0().into_any_element();
    }
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
        .into_any_element()
}

/// A turning `loader-circle`, for work that is under way (connecting, loading a folder).
pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> AnyElement {
    svg()
        .path("icons/loader-circle.svg")
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
        .with_animation(id, Animation::new(std::time::Duration::from_millis(900)).repeat(), |icon, delta| {
            icon.with_transformation(Transformation::rotate(percentage(delta)))
        })
        .into_any_element()
}

/// Islands are the rounded panels the window is made of (sidebar, terminal).
/// Rows inside them have an 8px radius and sit 8px from the edge: 8 + 8 = 16.
pub const ISLAND_RADIUS: f32 = 16.;
pub const ISLAND_GAP: f32 = 8.;

pub fn island(p: AppPalette) -> gpui::Div {
    div()
        .rounded(px(ISLAND_RADIUS))
        .bg(p.sidebar)
        .border_1()
        .border_color(p.border)
        .shadow(vec![BoxShadow {
            color: alpha(0x000000, if p.dark { 0.35 } else { 0.06 }),
            offset: point(px(0.), px(1.)),
            blur_radius: px(3.),
            spread_radius: px(0.),
            inset: false,
        }])
}

/// Layered, low-opacity shadows for floating surfaces (dialogs, popovers).
pub fn elevation(dark: bool) -> Vec<BoxShadow> {
    let a = if dark { 0.5 } else { 0.10 };
    vec![
        BoxShadow { color: alpha(0x000000, a * 0.6), offset: point(px(0.), px(1.)), blur_radius: px(2.), spread_radius: px(0.), inset: false },
        BoxShadow { color: alpha(0x000000, a), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(-8.), inset: false },
    ]
}

/// Small uppercase section label. Copy stays in natural case; it is uppercased here.
pub fn section_label(text: &str, p: AppPalette) -> impl IntoElement {
    tracked(&text.to_uppercase(), 10.5, FontWeight::SEMIBOLD, 0.6, p.text_faint, UI_FONT)
}

/// Width of the logo per pixel of height.
const LOGO_ASPECT: f32 = 1168. / 167.;

/// The Komando logo (">_Komando."), `height` pixels tall, in `color`: the theme's text colour
/// makes it dark in light mode and light in dark mode.
pub fn wordmark(height: f32, color: Hsla) -> impl IntoElement {
    svg()
        .path(crate::LOGO_SVG)
        .h(px(height))
        .w(px((height * LOGO_ASPECT).round()))
        .flex_shrink_0()
        .text_color(color)
}

fn tracked(text: &str, size: f32, weight: FontWeight, tracking: f32, color: Hsla, family: &'static str) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(tracking))
        .font_family(family)
        .text_size(px(size))
        .line_height(px((size * 1.1).ceil()))
        .font_weight(weight)
        .text_color(color)
        .children(text.chars().map(|c| div().child(c.to_string())))
}

/// A coloured square with an icon: how Projects and Hosts are told apart at a glance.
pub fn badge(icon: &str, color: Hsla, size: f32) -> impl IntoElement {
    let letter = icon.strip_prefix("letter:").map(|s| s.to_string());
    div()
        .size(px(size))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px((size * 0.28).round()))
        .bg(color.opacity(0.16))
        .text_color(color)
        // A shell's own icon is coloured already: shown as it is, without the tinted square.
        .when(is_artwork(icon), |el| el.bg(gpui::transparent_black()))
        .map(|el| match letter {
            None if is_artwork(icon) => el.child(icon_el(icon, (size * 0.85).round(), color)),
            Some(letter) => el
                .font_family(UI_FONT)
                .text_size(px(size * 0.5))
                .font_weight(FontWeight::BOLD)
                .child(letter),
            None => el.child(icon_el(icon, (size * 0.6).round(), color)),
        })
}

/// A small status dot: green connected, amber connecting, grey disconnected.
pub fn status_dot(color: Hsla) -> impl IntoElement {
    div().size(px(7.)).rounded_full().flex_shrink_0().bg(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shells_get_their_own_icons() {
        assert_eq!(shell_icon("pwsh.exe"), Some("shell:pwsh"));
        assert_eq!(shell_icon(r"C:\Program Files\PowerShell\7\pwsh.exe"), Some("shell:pwsh"));
        assert_eq!(shell_icon("powershell.exe"), Some("shell:powershell"));
        assert_eq!(shell_icon("CMD.EXE"), Some("shell:cmd"));
        assert_eq!(shell_icon("wsl.exe"), Some("shell:linux"));
        assert_eq!(shell_icon("/bin/bash"), Some("shell:linux"));
        // Git Bash on Windows isn't Linux.
        assert_eq!(shell_icon(r"C:\Program Files\Git\bin\bash.exe"), None);
        assert_eq!(shell_icon("nu.exe"), None);
        for name in ["shell:pwsh", "shell:powershell", "shell:cmd", "shell:linux"] {
            assert!(shell_image(name).is_some(), "{name}");
        }
        // Every default shell on this platform has one.
        for shell in crate::model::default_shells() {
            assert!(shell_icon(&shell.program).is_some(), "{}", shell.program);
        }
    }
}
