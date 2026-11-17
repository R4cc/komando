//! The ☰ menu at the left of the title bar, and Presentation mode.
//!
//! The menu gathers what is otherwise spread over Settings and shortcuts, as an IDE's main
//! menu does: opening things, switching parts of the window on and off, and the way into
//! Settings. Presentation mode, like IntelliJ's, makes the terminal readable for people
//! watching a shared screen: a larger terminal font, the window full screen and the sidebar
//! out of the way, all put back as they were when it ends.

use super::*;
use crate::ui::popup::Popup;
use gpui::point;

/// What Presentation mode changed, to put back when it ends.
pub(super) struct Presentation {
    /// The sidebar was showing (`Some(true)`) or not before Presentation mode hid it; `None`
    /// if it left the sidebar alone.
    sidebar_visible: Option<bool>,
    /// Presentation mode put the window in full screen, so ending it leaves full screen.
    full_screen: bool,
}

/// The terminal font sizes Presentation mode offers in Settings.
pub(super) const PRESENTATION_FONT_SIZES: std::ops::RangeInclusive<f32> = 14.0..=48.0;

impl Workspace {
    /// The terminal's font size now: Presentation mode's while it is on.
    pub(super) fn terminal_font_size(&self) -> f32 {
        if self.presentation.is_some() { self.settings.presentation_font_size } else { self.settings.font_size }
    }

    pub(super) fn presenting(&self) -> bool {
        self.presentation.is_some()
    }

    /// Start or end Presentation mode.
    pub(super) fn toggle_presentation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.presentation.take() {
            Some(before) => {
                // Left full screen already (a desktop's toolbar can): don't go back into it.
                if before.full_screen && window.is_fullscreen() {
                    window.toggle_fullscreen();
                }
                if let Some(visible) = before.sidebar_visible {
                    self.sidebar_visible = visible;
                }
            }
            None => {
                let full_screen = self.settings.presentation_full_screen && !window.is_fullscreen();
                let sidebar_visible = self.settings.presentation_hide_sidebar.then_some(self.sidebar_visible);
                self.presentation = Some(Presentation { sidebar_visible, full_screen });
                if full_screen {
                    window.toggle_fullscreen();
                }
                if sidebar_visible.is_some() {
                    self.sidebar_visible = false;
                    self.sidebar_peek = false;
                    self.sidebar_peek_timer = None;
                    self.transfers.update(cx, |transfers, cx| transfers.close(cx));
                }
            }
        }
        self.apply_appearance(window, cx);
        if self.page == Page::Sessions && !self.modal_open(cx) {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// Settings → Appearance → Expand sidebar on hover, also in the ☰ menu.
    pub(super) fn set_sidebar_auto_hide(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.sidebar_auto_hide = on;
        self.sidebar_visible = !on;
        self.sidebar_peek = false;
        self.sidebar_peek_timer = None;
        self.save_settings(cx);
        cx.notify();
    }

    fn toggle_minimap(&mut self, cx: &mut Context<Self>) {
        self.settings.minimap = !self.settings.minimap;
        self.apply_terminal_settings(cx);
    }

    fn toggle_command_rail(&mut self, cx: &mut Context<Self>) {
        self.settings.command_rail = !self.settings.command_rail;
        self.apply_terminal_settings(cx);
    }

    /// Open the ☰ menu below its button, or close it if it is open.
    pub(super) fn toggle_main_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.main_menu.is_open() {
            self.main_menu = Popup::default();
            cx.notify();
            return;
        }
        if self.modal_open(cx) || self.unlock.is_some() {
            return;
        }
        let ws = cx.entity().downgrade();
        // Read now: the menu's builder must not read the Workspace (see `nav::open_row_menu`).
        let sidebar = self.sidebar_visible;
        let auto_hide = self.settings.sidebar_auto_hide;
        let minimap = self.settings.minimap;
        let rail = self.settings.command_rail;
        let presenting = self.presenting();
        let vault_open = self.vault.is_some();
        // Below the button, its left edge lined up with the button's.
        let position = point(px(ISLAND_GAP + 8.), px(TITLE_BAR_HEIGHT - 4.));
        self.main_menu = Popup::open(position, |this: &mut Workspace| &mut this.main_menu, window, cx, move |menu, _, _| {
            // A toggle: its checkmark says whether it is on.
            let toggle = |label: &str, on: bool, run: fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
                let ws = ws.clone();
                PopupMenuItem::new(label.to_string()).checked(on).on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |this, cx| run(this, window, cx));
                })
            };
            let menu = menu
                .min_w(px(260.))
                .item(menu_item("Quick connect…", "search", &ws, |this, window, cx| this.open_picker(window, cx)).action(Box::new(QuickConnect)))
                .item(menu_item("New project…", "folder-plus", &ws, |this, window, cx| this.open_new_project(window, cx)))
                .item(menu_item("Import…", "import", &ws, |this, window, cx| this.open_settings(SettingsSection::Import, window, cx)))
                .separator()
                .label("View")
                .item(toggle("Sidebar", sidebar, |this, window, cx| this.toggle_sidebar(window, cx)).action(Box::new(ToggleSidebar)))
                .item(toggle("Expand sidebar on hover", auto_hide, |this, window, cx| {
                    this.set_sidebar_auto_hide(!this.settings.sidebar_auto_hide, cx);
                    this.focus_active(window, cx);
                }))
                .item(toggle("Minimap", minimap, |this, window, cx| {
                    this.toggle_minimap(cx);
                    this.focus_active(window, cx);
                }))
                .item(toggle("Command rail", rail, |this, window, cx| {
                    this.toggle_command_rail(cx);
                    this.focus_active(window, cx);
                }))
                .item(
                    toggle("Presentation mode", presenting, |this, window, cx| this.toggle_presentation(window, cx))
                        .action(Box::new(TogglePresentation)),
                )
                .separator();
            let menu = if vault_open {
                menu.item(menu_item("Lock vault", "lock", &ws, |this, _, cx| this.lock_vault(cx)).action(Box::new(LockVault)))
            } else {
                menu
            };
            menu.item(
                menu_item("Settings", "settings", &ws, |this, window, cx| this.open_settings(SettingsSection::Appearance, window, cx))
                    .action(Box::new(OpenSettings)),
            )
            .item(menu_item("Welcome tour", "map", &ws, |this, window, cx| this.start_tour(window, cx)))
            .item(menu_item("About Komando", "info", &ws, |this, window, cx| this.open_settings(SettingsSection::About, window, cx)))
        });
        cx.notify();
    }

    /// The ☰ button, and beside it what belongs at the window's top left: the button that
    /// brings a hidden sidebar back, and the way out of Presentation mode.
    pub(super) fn render_main_menu_buttons(&self, p: AppPalette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let locked = self.unlock.is_some();
        let menu_open = self.main_menu.is_open();
        let mut buttons = vec![
            strip_action("main-menu", "menu", "Main menu", false, p)
                .size(px(30.))
                .aria_expanded(menu_open)
                .when(menu_open, |el| el.bg(p.active))
                .when(locked, |el| el.opacity(0.45).cursor_default())
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_main_menu(window, cx)))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if pressed(event) {
                        this.toggle_main_menu(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .into_any_element(),
        ];
        // The sidebar's own button hides it; this one brings it back.
        if !self.sidebar_visible {
            buttons.push(
                strip_action("sidebar-show", "panel-left-open", "Show sidebar (Ctrl+Shift+B)", false, p)
                    .size(px(30.))
                    .aria_expanded(false)
                    .when(locked, |el| el.opacity(0.45).cursor_default())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_sidebar(window, cx)))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if pressed(event) {
                            this.toggle_sidebar(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .into_any_element(),
            );
        }
        if self.presenting() {
            buttons.push(
                div()
                    .id("presentation-exit")
                    .role(gpui::Role::Button)
                    .aria_label("Exit presentation mode")
                    .tab_index(0)
                    .focus_visible(move |s| s.border_2().border_color(p.focus))
                    .h(px(24.))
                    .px(px(9.))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(6.))
                    .rounded(p.corner(7.))
                    .bg(p.brand_soft)
                    .border_1()
                    .border_color(p.brand.opacity(0.45))
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.brand)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.hover))
                    // In the title bar, whose drag area is behind it: see `strip_action`.
                    .occlude()
                    .tooltip(|window, cx| Tooltip::new("Exit presentation mode (Ctrl+Shift+P)").build(window, cx))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_presentation(window, cx)))
                    .child(icon_el("presentation", 13., p.brand))
                    .child("Presenting")
                    .child(icon_el("x", 12., p.brand))
                    .into_any_element(),
            );
        }
        buttons
    }

    /// Beside Quick connect at the top of the sidebar: hide the sidebar. On the floated-out
    /// sidebar (Expand sidebar on hover) it keeps it open instead.
    pub(super) fn render_sidebar_hide_button(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let floating = !self.sidebar_visible;
        let (icon, label) = if floating { ("pin", "Keep sidebar open (Ctrl+Shift+B)") } else { ("panel-left-close", "Hide sidebar (Ctrl+Shift+B)") };
        div()
            .id("sidebar-hide")
            .role(gpui::Role::Button)
            .aria_label(label)
            .aria_expanded(self.sidebar_visible)
            .tab_index(0)
            .focus_visible(move |s| s.border_2().border_color(p.focus))
            .size(px(34.))
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .rounded(p.corner(8.))
            .border_1()
            .border_color(p.border)
            .cursor_pointer()
            .hover(|s| s.bg(p.hover))
            .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_sidebar(window, cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if pressed(event) {
                    this.toggle_sidebar(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(icon_el(icon, 15., p.text_muted))
            .into_any_element()
    }
}

/// Enter or Space on a focused button.
fn pressed(event: &KeyDownEvent) -> bool {
    matches!(event.keystroke.key.as_str(), "enter" | "space")
}
