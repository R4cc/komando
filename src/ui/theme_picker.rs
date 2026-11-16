//! Named theme dropdowns with palette swatches and native menu keyboard handling.
use std::rc::Rc;
use gpui::{App, IntoElement, Window, SharedString, div, px, prelude::*};
use gpui_component::{button::Button, menu::{DropdownMenu, PopupMenuItem}, tooltip::Tooltip};
use crate::theme::{AppPalette, MATCH_THEME, TerminalPalette, ThemeSpec};
use crate::ui::widgets::icon_el;

/// One dropdown row: a name and the colors that preview it.
#[derive(Clone)]
struct Choice {
    id: &'static str,
    name: SharedString,
    swatches: Vec<(&'static str, u32)>,
}

/// The Theme for one mode.
pub fn dropdown(
    dark: bool, current: &'static ThemeSpec, p: AppPalette,
    selected: impl Fn(&'static str, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let choices = crate::theme::themes(dark).iter().map(|t| {
        let c = t.colors;
        Choice { id: t.id, name: t.name.into(), swatches: vec![
            ("Background", c.background), ("Sidebar", c.sidebar), ("Primary", c.primary), ("Accent", c.accent),
        ] }
    }).collect();
    let mode = if dark { "dark" } else { "light" };
    picker(SharedString::from(format!("{mode}-theme")), format!("{} theme", if dark { "Dark" } else { "Light" }), choices, current.id, p, selected)
}

/// The Terminal Colors for one mode, led by the option to match that mode's Theme.
pub fn terminal_dropdown(
    dark: bool, current: &str, theme: &'static ThemeSpec, p: AppPalette,
    selected: impl Fn(&'static str, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let matched = std::iter::once((MATCH_THEME, SharedString::from(format!("Match theme ({})", theme.name)), TerminalPalette::from_theme(theme)));
    let named = crate::theme::terminal_colors_list(dark).iter().map(|c| (c.id, c.name.into(), TerminalPalette::from_colors(c)));
    let mut choices: Vec<Choice> = named.chain(matched).map(|(id, name, term)| {
        let rgb = |c: alacritty_terminal::vte::ansi::Rgb| (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32;
        Choice { id, name, swatches: vec![
            ("Background", rgb(term.background)), ("Text", rgb(term.foreground)),
            ("Red", rgb(term.ansi[1])), ("Green", rgb(term.ansi[2])), ("Blue", rgb(term.ansi[4])),
        ] }
    }).collect();
    // Default first, then Match theme, then the rest in catalog order.
    let matched = choices.pop().expect("match theme choice");
    choices.insert(1, matched);
    let current = choices.iter().find(|c| c.id == current).map_or(choices[0].id, |c| c.id);
    let mode = if dark { "dark" } else { "light" };
    picker(SharedString::from(format!("{mode}-terminal-colors")), format!("{} terminal colors", if dark { "Dark" } else { "Light" }), choices, current, p, selected)
}

fn picker(
    key: SharedString, label: String, choices: Vec<Choice>, current: &'static str, p: AppPalette,
    selected: impl Fn(&'static str, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let selected = Rc::new(selected);
    let shown = choices.iter().find(|c| c.id == current).unwrap_or(&choices[0]).clone();
    Button::new(SharedString::from(format!("{key}-picker")))
        .w_full()
        .accessibility_label(format!("{label}: {}", shown.name))
        .bg(p.popover).text_color(p.popover_text).border_color(p.input)
        .child(choice_row(&key, &shown, p))
        .child(icon_el("chevron-down", 14., p.text_muted))
        .dropdown_menu(move |mut menu, _, _| {
            menu = menu.min_w(px(300.)).max_w(px(380.)).max_h(px(400.));
            for choice in &choices {
                let selected = selected.clone();
                let (id, row, key) = (choice.id, choice.clone(), key.clone());
                menu = menu.item(PopupMenuItem::element(move |_, _| choice_row(&key, &row, p))
                    .checked(id == current)
                    .on_click(move |_, window, cx| selected(id, window, cx)));
            }
            menu
        })
}

/// Shared by the closed selector and each dropdown row; swatches show the actual colors.
fn choice_row(key: &SharedString, choice: &Choice, p: AppPalette) -> impl IntoElement + use<> {
    div().w_full().min_w(px(240.)).h(px(28.)).flex().items_center().gap(px(12.))
        .child(div().flex_1().min_w_0().truncate().child(choice.name.clone()))
        .child(swatches(key, choice, p))
}

fn swatches(key: &SharedString, choice: &Choice, p: AppPalette) -> impl IntoElement + use<> {
    let prefix = format!("swatch-{key}-{}", choice.id);
    div().flex().flex_shrink_0().rounded(px(3.)).overflow_hidden().border_1().border_color(p.border).children(
        choice.swatches.clone().into_iter().map(move |(name, color)| {
            div().id(SharedString::from(format!("{prefix}-{name}")))
                .w(px(12.)).h(px(17.)).flex_shrink_0().bg(crate::theme::hex(color))
                .tooltip(move |window, cx| Tooltip::new(format!("{name}: #{color:06X}")).build(window, cx))
        }),
    )
}
