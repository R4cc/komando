//! Small building blocks shared by every screen: buttons, icons, elevation, labels.
//!
//! Type scale (px): 11 uppercase labels (+tracking), 12 meta, 13 UI text, 14 inputs and
//! terminal, 15 dialog titles, 40+ the wordmark. Radii are concentric: an inner element's
//! radius is the outer radius minus the padding between them.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, BoxShadow, ElementId, FontWeight, Hsla, IntoElement, ParentElement, SharedString, Stateful,
    Styled, div, point, prelude::*, px, svg,
};

use crate::theme::{AppPalette, DISPLAY_FONT, UI_FONT, alpha};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// A 32px-high text button. Hit area is the full pill.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: ButtonKind, p: AppPalette) -> Stateful<gpui::Div> {
    let (bg, fg, hover, border) = match kind {
        ButtonKind::Primary => (p.brand, gpui::white(), p.brand.opacity(0.88), None),
        ButtonKind::Secondary => (p.hover, p.text, p.active, Some(p.border)),
        ButtonKind::Ghost => (gpui::transparent_black(), p.text_muted, p.hover, None),
        ButtonKind::Danger => (p.danger, gpui::white(), p.danger.opacity(0.88), None),
    };
    div()
        .id(id)
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
        .hover(move |s| s.bg(hover))
        .child(label.into())
}

/// A square icon-only button with a tooltip-free label for accessibility tooling.
pub fn icon_button(id: impl Into<ElementId>, icon: &str, p: AppPalette) -> Stateful<gpui::Div> {
    div()
        .id(id)
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

/// A Lucide icon by name (e.g. "server"), drawn in `color`.
pub fn icon_el(name: &str, size: f32, color: Hsla) -> AnyElement {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
        .into_any_element()
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

/// The KOMANDO wordmark. gpui has no letter-spacing, so letters are laid out one by one.
pub fn wordmark(size: f32, color: Hsla) -> impl IntoElement {
    tracked("KOMANDO", size, FontWeight::EXTRA_BOLD, size * 0.16, color, DISPLAY_FONT)
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
        .map(|el| match letter {
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
