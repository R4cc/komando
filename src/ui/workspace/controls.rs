//! Small building blocks the workspace's pages and forms are drawn with: switches,
//! segmented choices, the icon picker, form cards, window controls.

use super::*;

/// The sign that output is streaming in.
pub(super) fn activity_icon(p: AppPalette) -> AnyElement {
    div()
        .id("output-activity-status")
        .role(gpui::Role::Status)
        .aria_label("Receiving output")
        .flex_shrink_0()
        .child(spinner("output-activity", 12., p.success))
        .into_any_element()
}

/// A Session's state in the sidebar: a dot while live (green) or connecting (amber), a hollow
/// ring once it dropped (red, with its reason as the tooltip, when it never connected), and a
/// small turning green ring for output streaming in the background.
pub(super) fn status_indicator(status: Option<&Status>, busy: bool, failed: bool, p: AppPalette) -> AnyElement {
    match status {
        Some(Status::Connected) if busy => activity_icon(p),
        Some(Status::Connected) => status_dot(p.success).into_any_element(),
        Some(Status::Connecting) => status_dot(p.warning).into_any_element(),
        Some(Status::Disconnected(reason)) if failed => {
            let tip: SharedString = if reason.is_empty() { "Couldn't connect".into() } else { reason.clone().into() };
            div()
                .id("status-failed")
                .flex_shrink_0()
                .aria_label(tip.clone())
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(div().size(px(8.)).rounded_full().border_2().border_color(p.danger))
                .into_any_element()
        }
        Some(Status::Disconnected(_)) => {
            div().size(px(7.)).flex_shrink_0().rounded_full().border_1().border_color(p.text_faint).into_any_element()
        }
        None => div().into_any_element(),
    }
}

/// The keyboard cursor's outline on a sidebar row. Drawn over the row rather than as its
/// border, so the row's content doesn't move when the cursor arrives.
pub(super) fn focus_ring(radius: gpui::Pixels, p: AppPalette) -> gpui::Div {
    div().absolute().top_0().left_0().right_0().bottom_0().rounded(radius).border_2().border_color(p.focus)
}

/// What a Session's row in the sidebar says, and its tooltip. The row: its protocol, then the
/// name it was given, else which one it is: a terminal's number, or a VNC desktop's display
/// (port 5901 is display 1). `port` is a desktop's. A Local Shell's row has no protocol. The
/// tooltip: the name, else the title its shell set unless that only repeats the item's name or
/// is the program's path (as Windows shells set it), with what the Session is.
pub(super) fn session_row_text(session: &SessionEntry, port: Option<u16>) -> (SharedString, SharedString) {
    let protocol = desktop_protocol(session);
    let kind = match protocol {
        Some(protocol) => format!("Remote desktop ({})", protocol.label()),
        None => format!("Terminal #{}", session.number()),
    };
    let ssh = matches!(session.kind(), SessionKind::Ssh { .. });
    let label: SharedString = session_row_label(protocol, ssh, session.name().map(|n| n.as_ref()), session.number(), port).into();
    if let Some(name) = session.name() {
        return (label, format!("{name} · {kind}").into());
    }
    let title = session.ui.title.trim();
    let useless = title.is_empty() || title == session.label().as_ref() || title.to_ascii_lowercase().ends_with(".exe");
    let tip = if useless || protocol.is_some() { kind } else { format!("{title} · {kind}") };
    (label, tip.into())
}

/// A Session row's label: "SSH — Session 2", "VNC — Display 1", "RDP — Desktop", or with
/// the name the Session was given in place of which one it is. A Local Shell's has no protocol.
pub(super) fn session_row_label(protocol: Option<Protocol>, ssh: bool, name: Option<&str>, number: usize, port: Option<u16>) -> String {
    let which = match (name, protocol, port) {
        (Some(name), ..) => name.to_string(),
        (None, Some(Protocol::Vnc), Some(port @ 5900..=5999)) => format!("Display {}", port - 5900),
        (None, Some(_), _) => "Desktop".to_string(),
        (None, None, _) => format!("Session {number}"),
    };
    match protocol.map(|p| p.label()).or(ssh.then_some("SSH")) {
        Some(prefix) => format!("{prefix} — {which}"),
        None => which,
    }
}

/// A segmented control: one choice of several.
pub(super) fn segmented<L: Into<SharedString> + Clone>(
    id: &'static str,
    options: &[(L, bool)],
    p: AppPalette,
    cx: &mut Context<Workspace>,
    pick: impl Fn(&mut Workspace, usize, &mut Window, &mut Context<Workspace>) + Clone + 'static,
) -> AnyElement {
    let items: Vec<_> = options.iter().enumerate().map(|(i, (label, selected))| {
            let pick = pick.clone();
            let label: SharedString = label.clone().into();
            div()
                .id((id, i))
                .role(gpui::Role::RadioButton)
                .aria_label(label.clone())
                .aria_toggled(if *selected { gpui::Toggled::True } else { gpui::Toggled::False })
                .tab_index(0)
                .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
                .h(px(26.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(p.corner(6.))
                .cursor_pointer()
                .text_size(px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if *selected { p.active_text } else { p.muted_text })
                .when(*selected, |el| el.bg(p.brand_soft).font_weight(FontWeight::SEMIBOLD))
                .when(!*selected, |el| el.hover(|s| s.bg(p.hover).text_color(p.hover_text)))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| pick(this, i, window, cx)))
                .child(label)
        })
        .collect();
    div()
        .id(id)
        .role(gpui::Role::RadioGroup)
        .aria_label(id.replace('-', " "))
        .flex()
        .flex_shrink_0()
        .p(px(3.))
        .gap(px(2.))
        .rounded(p.corner(9.))
        .bg(p.muted)
        .border_1()
        .border_color(p.border)
        .children(items)
        .into_any_element()
}

/// An on/off switch, 32×18.
pub(super) fn switch(id: SharedString, label: impl Into<SharedString>, on: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    switch_with(id, label.into(), on, false, p)
}

/// A smaller, neutral switch for long lists (single highlight rules), so the brand-coloured
/// switches of the settings that matter more stand out.
pub(super) fn small_switch(id: SharedString, label: impl Into<SharedString>, on: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    switch_with(id, label.into(), on, true, p)
}

fn switch_with(id: SharedString, label: SharedString, on: bool, small: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    let (w, h, knob) = if small { (26., 14., 10.) } else { (32., 18., 14.) };
    // The off track has to be visible on its own, not only through the knob's position.
    let off = p.border;
    let on_color = if small { p.text_muted.opacity(0.6) } else { p.brand };
    div()
        .id(id)
        .role(gpui::Role::Switch)
        .aria_label(label)
        .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
        .tab_index(0)
        .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
        .w(px(w))
        .h(px(h))
        .flex_shrink_0()
        .p(px(2.))
        .rounded_full()
        .cursor_pointer()
        .bg(if on { on_color } else { off })
        .flex()
        .when(on, |el| el.justify_end())
        // Keep the knob readable on every supplied primary color.
        .child(
            div()
                .size(px(knob))
                .rounded_full()
                .bg(if on && !small { p.on_brand } else { p.text }),
        )
}

/// The bundled Lucide names, legacy artwork, Devicon IDs and shell symbols, sorted.
pub(super) fn all_icons() -> &'static [String] {
    static ICONS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    ICONS.get_or_init(|| {
        use gpui::AssetSource as _;
        let mut names: Vec<String> = gpui_kit_assets::AllAssets
            .list("icons/")
            .unwrap_or_default()
            .iter()
            .filter_map(|path| path.strip_prefix("icons/")?.strip_suffix(".svg").map(str::to_string))
            // Only plain Lucide icons: no sub-folders, no window chrome.
            .filter(|name| !name.contains('/') && !name.starts_with("window-"))
            .collect();
        names.push("shell:cmd".into());
        names.extend(crate::icons::ICONS.iter().map(|icon| icon.id.to_string()));
        names.extend(crate::icons::SYMBOLS.iter().map(|icon| icon.id.to_string()));
        names.sort();
        names.dedup();
        names
    })
}

/// Shown before anything is typed: icons for servers and the things that run on them.
pub(super) const SUGGESTED_ICONS: &[&str] = &[
    "folder", "server", "database", "globe", "shield", "hard-drive", "cpu", "network", "cloud", "box", "router",
    "tabler:brand-powershell", "tabler:terminal-2", "simpleicons:linux", "simpleicons:ubuntu", "simpleicons:debian", "devicon:docker", "devicon:kubernetes",
    "container", "monitor", "laptop", "terminal", "code", "git-branch", "key-round", "lock", "mail", "cloud-cog",
    "server-cog", "database-backup", "rocket", "flame", "activity", "bug", "building-2", "factory", "house",
    "flask-conical", "wrench", "zap", "radio-tower", "satellite", "warehouse",
];

/// Most results shown at once; a more specific search narrows them down.
const ICON_RESULTS: usize = 60;

/// What an icon is found by, prepared once: the search runs on every frame of the icon picker.
pub(super) struct IconTerms {
    id: &'static str,
    /// The id without its set's prefix.
    name: &'static str,
    /// A Devicon's name without dashes, so "nodejs" finds "node-js".
    undashed: Option<String>,
    /// Its catalog's search words, lower case and joined by dashes like the query.
    words: Option<String>,
}

fn icon_terms() -> &'static [IconTerms] {
    static TERMS: std::sync::OnceLock<Vec<IconTerms>> = std::sync::OnceLock::new();
    TERMS.get_or_init(|| {
        all_icons()
            .iter()
            .map(|id| {
                let id = id.as_str();
                let name = if id == "shell:cmd" { "command-prompt-cmd-windows-terminal" } else { id.split_once(':').map_or(id, |(_, name)| name) };
                let search = crate::icons::find(id).map(|icon| icon.search).or_else(|| crate::icons::find_symbol(id).map(|icon| icon.search));
                IconTerms {
                    id,
                    name,
                    undashed: id.starts_with("devicon:").then(|| name.replace('-', "")),
                    words: search.map(|search| search.to_lowercase().replace(' ', "-")),
                }
            })
            .collect()
    })
}

/// Icons matching `query`: exact name first, then names starting with it, then names with a
/// word starting with it ("backup" finds "database-backup"), then any other match.
pub(super) fn search_icons(query: &str) -> (Vec<&'static str>, usize) {
    let q = query.trim().to_lowercase().replace(' ', "-");
    if q.is_empty() {
        let all = all_icons();
        let shown = SUGGESTED_ICONS.iter().filter_map(|s| all.iter().find(|n| n.as_str() == *s)).map(|n| n.as_str()).collect();
        return (shown, 0);
    }
    let q_undashed = q.replace('-', "");
    let rank = |icon: &IconTerms| {
        let name = icon.name;
        if name == q {
            Some(0)
        } else if name.starts_with(&q) {
            Some(1)
        } else if name.split('-').any(|word| word.starts_with(&q)) {
            Some(2)
        } else if name.contains(&q) || icon.undashed.as_ref().is_some_and(|n| n.contains(&q_undashed)) {
            Some(3)
        } else if icon.words.as_ref().is_some_and(|words| words.contains(&q)) || (q == "devicon" && icon.id.starts_with("devicon:")) {
            Some(4)
        } else {
            None
        }
    };
    let mut hits: Vec<(u8, &'static str)> = icon_terms().iter().filter_map(|icon| rank(icon).map(|r| (r, icon.id))).collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(b.1)));
    let total = hits.len();
    (hits.into_iter().take(ICON_RESULTS).map(|(_, n)| n).collect(), total)
}

/// The icon picker: a search over the whole icon set and a grid of results. The selected icon
/// is always in the grid, first if the search doesn't include it.
pub(super) fn icon_picker(
    query: &Entity<InputState>,
    selected: &str,
    color: Hsla,
    p: AppPalette,
    cx: &mut Context<Workspace>,
    set: impl Fn(&mut Workspace, String) + Clone + 'static,
) -> impl IntoElement {
    let text = query.read(cx).value().to_string();
    let (mut icons, total) = search_icons(&text);
    let searching = !text.trim().is_empty();
    if !selected.is_empty() && !icons.contains(&selected) && !(searching && total == 0) {
        if let Some(name) = all_icons().iter().find(|n| n.as_str() == selected) {
            icons.insert(0, name.as_str());
        }
    }
    let status = if !searching {
        format!("Suggested · {} icons in total, search to find more", all_icons().len())
    } else if total == 0 {
        format!("No icon matches \"{}\".", text.trim())
    } else if total > icons.len() {
        format!("{} matches, showing the first {}. Keep typing to narrow it down.", total, icons.len())
    } else if total == 1 {
        "1 match".to_string()
    } else {
        format!("{total} matches")
    };
    let grid = div().flex().flex_wrap().gap(px(6.)).children(icons.into_iter().map(|name| {
        let is_selected = name == selected;
        let set = set.clone();
        let owned = name.to_string();
        let tip: SharedString = crate::icons::label(name).into();
        div()
            .id(SharedString::from(format!("icon-{name}")))
            .role(gpui::Role::RadioButton)
            .aria_label(format!("Icon: {}", crate::icons::label(name)))
            .aria_toggled(if is_selected { gpui::Toggled::True } else { gpui::Toggled::False })
            .tab_index(0)
            .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
            .size(px(36.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(p.corner(8.))
            .cursor_pointer()
            .border_1()
            .border_color(if is_selected { color } else { p.border })
            .bg(if is_selected { color.opacity(0.14) } else { gpui::transparent_black() })
            .hover(|s| s.bg(p.hover))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                set(this, owned.clone());
                cx.notify();
            }))
            .child(icon_el(name, 16., if is_selected { color } else { p.text_muted }))
    }));
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(Input::new(query).prefix(icon_el("search", 14., p.text_faint)).cleanable(true))
        // Three rows; more results scroll.
        .child(div().id("icon-grid").max_h(px(126.)).overflow_y_scroll().child(grid))
        .child(div().text_size(px(11.5)).text_color(p.text_faint).child(status))
}

/// Over a host or project form being closed with changes: discard them, or go back to it.
pub(super) fn discard_question(p: AppPalette, cx: &mut Context<Workspace>) -> AnyElement {
    div()
        .id("discard-backdrop")
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(alpha(0x000000, if p.dark { 0.35 } else { 0.15 }))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .id("discard-card")
                .role(gpui::Role::AlertDialog)
                .aria_label("Discard your changes?")
                .rounded(p.corner(14.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .child(form_card(
                    p,
                    "Discard your changes?",
                    div()
                        .text_size(px(13.))
                        .line_height(px(20.))
                        .text_color(p.text_muted)
                        .child("What you changed in this form hasn't been saved.")
                        .into_any_element(),
                    vec![
                        button("discard-keep", "Keep editing", ButtonKind::Ghost, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.keep_editing(window, cx)))
                            .into_any_element(),
                        button("discard-ok", "Discard", ButtonKind::Danger, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx)))
                            .into_any_element(),
                    ],
                )),
        )
        .into_any_element()
}

pub(super) fn form_card(p: AppPalette, title: &str, body: AnyElement, actions: Vec<AnyElement>) -> AnyElement {
    div()
        .w(px(440.))
        .p(px(22.))
        .flex()
        .flex_col()
        .gap(px(16.))
        // Room on the right for the dialog's close button.
        .child(div().pr(px(28.)).text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title.to_string()))
        .child(body)
        .child(div().flex().justify_end().gap(px(8.)).children(actions))
        .into_any_element()
}

/// A form field's name, above its control.
pub(super) fn field_label(label: &str, p: AppPalette) -> AnyElement {
    div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(label.to_string()).into_any_element()
}

/// A form field: label above the control, an optional hint below.
pub(super) fn form_field(label: &str, hint: Option<&str>, control: AnyElement, p: AppPalette) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(field_label(label, p))
        .child(control)
        .when_some(hint, |el, hint| el.child(div().text_size(px(12.)).text_color(p.text_muted).child(hint.to_string())))
        .into_any_element()
}

/// A connected Session's Disconnect and Close entries. Delete does what × does: it closes
/// the Session, or with `keep` (a terminal, "Keep sessions after disconnecting" on) only
/// disconnects it, so then there is no separate Close.
pub(super) fn live_session_items(menu: PopupMenu, id: usize, keep: bool, ws: &gpui::WeakEntity<Workspace>) -> PopupMenu {
    let disconnect = menu_item("Disconnect", Some("unplug"), ws, move |this, window, cx| this.end_session(id, EndRequest::Disconnect, window, cx));
    if keep {
        return menu.item(disconnect.action(Box::new(nav::NavDelete)));
    }
    menu.item(disconnect)
        .item(menu_item("Close session", None, ws, move |this, window, cx| this.end_session(id, EndRequest::Close, window, cx)).action(Box::new(nav::NavDelete)))
}

/// The rows of `old` that differ from `new`, past their shared start and end, and how many
/// rows of `new` take their place.
pub(super) fn changed_rows<T: PartialEq>(old: &[T], new: &[T]) -> (std::ops::Range<usize>, usize) {
    let start = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let end = old[start..].iter().rev().zip(new[start..].iter().rev()).take_while(|(a, b)| a == b).count();
    (start..old.len() - end, new.len() - start - end)
}

/// The ✕ in a dialog's top-right corner. It does what Escape does there.
pub(super) fn close_x(id: &'static str, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    icon_button(id, "x", p).aria_label("Close")
}

/// Minimise, maximise/restore and close, drawn like Windows 11's own caption buttons: flush
/// rectangles along the window's top edge with no gap between them, the close button in the
/// window's corner (DWM rounds it with the window). The highlight is the system's 32 px tall,
/// so it stays clear of the terminal island's coloured edge below the bar.
///
/// Each button's hit area is the full height of the bar, and the close button's reaches the
/// window's corner, so flicking the pointer to the top-right corner of a maximised window
/// hits it. On Windows the buttons are hit-tested by the system (snap layouts on
/// maximise work); elsewhere (Linux dev builds) they act on click.
pub(super) fn window_controls(p: AppPalette, window: &Window) -> impl IntoElement {
    let maximized = window.is_maximized();
    let button = |id: &'static str, icon: &'static str, area: WindowControlArea, label: &'static str| {
        let close = area == WindowControlArea::Close;
        let (hover_bg, hover_fg, press_bg) = if close {
            (crate::theme::hex(0xc42b1c), gpui::white(), crate::theme::hex(0xa82518))
        } else {
            (p.hover, p.text, p.active)
        };
        div()
            .id(id)
            .group(id)
            .role(gpui::Role::Button)
            .aria_label(label)
            .h_full()
            .flex()
            .items_start()
            // The button's hitbox hides the title bar's drag area behind it. Without this the
            // system hit test can answer "caption" for a button (seen after maximising with a
            // dialog open): a click then does nothing and only a double click acts.
            .occlude()
            .window_control_area(area)
            .when(cfg!(not(windows)), |el| {
                el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(move |_, window, _| match area {
                    WindowControlArea::Min => window.minimize_window(),
                    WindowControlArea::Max => window.zoom_window(),
                    _ => window.remove_window(),
                })
            })
            .child(
                div()
                    .id("shape")
                    .w(px(WINDOW_BUTTON_WIDTH))
                    .h(px(WINDOW_BUTTON_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .group_hover(id, move |s| s.bg(hover_bg))
                    .group_active(id, move |s| s.bg(press_bg))
                    .child(
                        gpui::svg()
                            .path(SharedString::from(format!("icons/{icon}.svg")))
                            .size(px(13.))
                            .text_color(p.text_muted)
                            .group_hover(id, move |s| s.text_color(hover_fg)),
                    ),
            )
    };
    div()
        .flex()
        .h_full()
        .flex_shrink_0()
        .child(button("window-minimize", "window-minimize", WindowControlArea::Min, "Minimize"))
        .child(if maximized {
            button("window-restore", "window-restore", WindowControlArea::Max, "Restore")
        } else {
            button("window-maximize", "window-maximize", WindowControlArea::Max, "Maximize")
        })
        .child(button("window-close", "window-close", WindowControlArea::Close, "Close"))
}

/// Marks a window running with administrator rights, where every shell is elevated. Amber,
/// like a caution sign, and always in view in the title bar.
pub(super) fn admin_badge(p: AppPalette) -> impl IntoElement {
    div()
        .id("admin-badge")
        .h(px(22.))
        .px(px(8.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(5.))
        .rounded(p.corner(6.))
        .bg(p.warning.opacity(0.14))
        .border_1()
        .border_color(p.warning.opacity(0.3))
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.warning)
        .child(icon_el("shield", 13., p.warning))
        .child("Administrator")
        .tooltip(|window, cx| Tooltip::new("Shells in this window run with administrator rights").build(window, cx))
}

/// A small icon-only action for the session strip above the terminal.
/// `danger` marks an action that ends something: it turns red on hover.
pub(super) fn strip_action(id: &'static str, icon: &str, label: &'static str, danger: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    let (hover_bg, hover_fg) = if danger { (p.danger.opacity(0.12), p.danger) } else { (p.hover, p.text) };
    div()
        .id(id)
        .group(id)
        .role(gpui::Role::Button)
        .aria_label(label)
        .tab_index(0)
        .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(p.corner(7.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg))
        // The strip sits in the title bar, whose drag area is everywhere behind it. Without
        // this, Windows is told the button is caption: a click starts a window move and is
        // lost, and a double click maximises the window. Blocking the area keeps it a button.
        .occlude()
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        .child(
            gpui::svg()
                .path(SharedString::from(format!("icons/{icon}.svg")))
                .size(px(13.))
                .flex_shrink_0()
                .text_color(p.text_faint)
                .group_hover(id, move |s| s.text_color(hover_fg)),
        )
}

/// A keyboard shortcut ("Ctrl ,") in a small outlined chip, the only one in the app.
/// The "Switch to tab" pill on a quick connect result that already has an open Session.
pub(super) fn switch_to_tab_badge(selected: bool, p: AppPalette) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .h(px(22.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(5.))
        .rounded(p.corner(6.))
        .border_1()
        .border_color(p.border)
        .bg(if selected { p.active } else { p.hover })
        .text_size(px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.text_muted)
        .child("Switch to tab")
        .child(icon_el("arrow-right", 12., p.text_muted))
}

pub(super) fn kbd(keys: &str, p: AppPalette) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .px(px(5.))
        .h(px(18.))
        // Centred on its default line, the text rendered about a pixel above the middle of
        // the chip on Windows, so the capitals touched the top border. Pin the line to the
        // space inside the border and start it a pixel lower.
        .pt(px(2.))
        .line_height(px(14.))
        .flex()
        .items_center()
        .rounded(p.corner(4.))
        .border_1()
        .border_color(p.border)
        .text_size(px(10.5))
        .text_color(p.text_faint)
        .child(keys.to_string())
}
