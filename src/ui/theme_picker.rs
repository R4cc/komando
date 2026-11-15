//! Named theme dropdowns with palette swatches and native menu keyboard handling.
use std::rc::Rc;
use gpui::{App, IntoElement, Window, SharedString, div, px, prelude::*};
use gpui_component::{button::Button, menu::{DropdownMenu, PopupMenuItem}, tooltip::Tooltip};
use crate::theme::{AppPalette, ThemeSpec};
use crate::ui::widgets::icon_el;

pub fn dropdown(
    dark: bool, current: &'static ThemeSpec, p: AppPalette,
    selected: impl Fn(&'static str, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let selected = Rc::new(selected);
    Button::new(if dark { "dark-theme-picker" } else { "light-theme-picker" })
        .w_full()
        .accessibility_label(format!("{} theme: {}", if dark { "Dark" } else { "Light" }, current.name))
        .bg(p.popover).text_color(p.popover_text).border_color(p.input)
        .child(theme_choice(current, p))
        .child(icon_el("chevron-down", 14., p.text_muted))
        .dropdown_menu(move |mut menu, _, _| {
            menu = menu.min_w(px(380.)).max_w(px(460.)).max_h(px(400.));
            for t in crate::theme::themes(dark) {
                let selected = selected.clone();
                menu = menu.item(PopupMenuItem::element(move |_, _| theme_choice(t, p))
                    .checked(t.id == current.id)
                    .on_click(move |_, window, cx| selected(t.id, window, cx)));
            }
            menu
        })
}

/// Shared by the closed selector and each dropdown row; swatches show the tuned colors.
fn theme_choice(t: &'static crate::theme::ThemeSpec, p: AppPalette) -> impl IntoElement {
    div().w_full().min_w(px(310.)).h(px(28.)).flex().items_center().gap(px(16.))
        .child(div().flex_1().child(t.name))
        .child(theme_swatches(t, p))
}

fn theme_swatches(t: &'static crate::theme::ThemeSpec, p: AppPalette) -> impl IntoElement {
    let c = t.colors;
    div().flex().gap(px(3.)).children([
        ("Background", c.background), ("Sidebar", c.sidebar), ("Panels", c.card),
        ("Primary", c.primary), ("Secondary", c.secondary), ("Accent", c.accent), ("Text", c.foreground),
    ].into_iter().map(move |(name, color)| {
        div().id(SharedString::from(format!("swatch-{}-{}-{name}", t.id, t.dark)))
            .w(px(12.)).h(px(17.)).rounded(px(3.)).bg(crate::theme::hex(color))
            .border_1().border_color(p.border)
            .tooltip(move |window, cx| Tooltip::new(format!("{name}: #{color:06X}")).build(window, cx))
    }))
}
