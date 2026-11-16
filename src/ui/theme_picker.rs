//! Named theme dropdowns with miniature previews and native menu keyboard handling.
use std::rc::Rc;
use gpui::{App, IntoElement, Window, SharedString, div, px, prelude::*};
use gpui_component::{button::Button, menu::{DropdownMenu, PopupMenuItem}, tooltip::Tooltip};
use crate::theme::{AppPalette, MATCH_THEME, TerminalPalette, ThemeSpec};
use crate::ui::widgets::icon_el;

/// One dropdown row: a name and the miniature that previews it.
#[derive(Clone)]
struct Choice {
    id: &'static str,
    name: SharedString,
    thumb: Thumb,
}

/// A miniature in a choice's own colors.
#[derive(Clone, Copy)]
enum Thumb {
    /// A window: the sidebar beside the content, with the primary and accent colors.
    Theme { background: u32, sidebar: u32, sidebar_border: u32, primary: u32, accent: u32, text: u32 },
    /// A terminal: a prompt in the text color over four ANSI colors.
    Terminal { background: u32, text: u32, ansi: [u32; 4] },
}

impl Thumb {
    /// The colors by name, for the tooltip.
    fn describe(self) -> String {
        let named: Vec<(&str, u32)> = match self {
            Self::Theme { background, sidebar, primary, accent, .. } =>
                vec![("Background", background), ("Sidebar", sidebar), ("Primary", primary), ("Accent", accent)],
            Self::Terminal { background, text, ansi: [red, green, yellow, blue] } =>
                vec![("Background", background), ("Text", text), ("Red", red), ("Green", green), ("Yellow", yellow), ("Blue", blue)],
        };
        named.iter().map(|(name, color)| format!("{name} #{color:06X}")).collect::<Vec<_>>().join("\n")
    }
}

/// The Theme for one mode.
pub fn dropdown(
    dark: bool, current: &'static ThemeSpec, p: AppPalette,
    selected: impl Fn(&'static str, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let choices = crate::theme::themes(dark).iter().map(|t| {
        let c = t.colors;
        Choice { id: t.id, name: t.name.into(), thumb: Thumb::Theme {
            background: c.background, sidebar: c.sidebar, sidebar_border: c.sidebar_border,
            primary: c.primary, accent: c.accent, text: c.foreground,
        } }
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
        Choice { id, name, thumb: Thumb::Terminal {
            background: rgb(term.background), text: rgb(term.foreground),
            ansi: [rgb(term.ansi[1]), rgb(term.ansi[2]), rgb(term.ansi[3]), rgb(term.ansi[4])],
        } }
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

/// Shared by the closed selector and each dropdown row; the miniature shows the actual colors.
fn choice_row(key: &SharedString, choice: &Choice, p: AppPalette) -> impl IntoElement + use<> {
    div().w_full().min_w(px(240.)).h(px(28.)).flex().items_center().gap(px(12.))
        .child(div().flex_1().min_w_0().truncate().child(choice.name.clone()))
        .child(thumbnail(key, choice, p))
}

fn thumbnail(key: &SharedString, choice: &Choice, p: AppPalette) -> impl IntoElement + use<> {
    use crate::theme::hex;
    let thumb = choice.thumb;
    let frame = div()
        .id(SharedString::from(format!("thumb-{key}-{}", choice.id)))
        .w(px(46.)).h(px(24.)).flex_shrink_0()
        .rounded(px(4.)).border_1().border_color(p.border)
        .tooltip(move |window, cx| Tooltip::new(thumb.describe()).build(window, cx));
    match thumb {
        Thumb::Theme { background, sidebar, sidebar_border, primary, accent, text } => frame
            .bg(hex(background)).flex()
            .child(div().w(px(13.)).h_full().rounded_l(px(3.)).bg(hex(sidebar)).border_r_1().border_color(hex(sidebar_border))
                .pt(px(4.)).px(px(3.)).flex().flex_col().gap(px(3.))
                .child(div().h(px(3.)).rounded(px(1.)).bg(hex(primary)))
                .child(div().h(px(2.)).w(px(5.)).rounded(px(1.)).bg(hex(text).opacity(0.45))))
            .child(div().flex_1().p(px(4.)).flex().flex_col().gap(px(3.))
                .child(div().h(px(3.)).w(px(16.)).rounded(px(1.)).bg(hex(text).opacity(0.7)))
                .child(div().h(px(2.)).w(px(11.)).rounded(px(1.)).bg(hex(text).opacity(0.35)))
                .child(div().flex().gap(px(2.))
                    .child(div().h(px(4.)).w(px(9.)).rounded(px(1.)).bg(hex(primary)))
                    .child(div().h(px(4.)).w(px(7.)).rounded(px(1.)).bg(hex(accent)))))
            .into_any_element(),
        Thumb::Terminal { background, text, ansi } => frame
            .bg(hex(background)).px(px(5.)).py(px(3.)).flex().flex_col().justify_between()
            .child(div().flex().items_center().gap(px(2.))
                .child(div().h(px(2.)).w(px(6.)).rounded(px(1.)).bg(hex(ansi[1])))
                .child(div().h(px(2.)).w(px(12.)).rounded(px(1.)).bg(hex(text)))
                .child(div().h(px(6.)).w(px(3.)).bg(hex(text))))
            .child(div().flex().gap(px(3.)).children(ansi.map(|color| div().size(px(5.)).rounded_full().bg(hex(color)))))
            .into_any_element(),
    }
}
