//! The Settings page: a full-window page with category navigation on the left.
//! Sessions keep running behind it; Esc or "Back to sessions" returns to them.

use std::sync::{Arc, OnceLock};

use super::*;
use crate::model::MinimapPlacement;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsSection {
    Appearance,
    Terminal,
    FileBrowser,
    Shells,
    Highlights,
    Import,
    Security,
    About,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 8] = [
        Self::Appearance,
        Self::Terminal,
        Self::FileBrowser,
        Self::Shells,
        Self::Highlights,
        Self::Import,
        Self::Security,
        Self::About,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Terminal => "Terminal",
            Self::FileBrowser => "File browser",
            Self::Shells => "Local shells",
            Self::Highlights => "Highlights",
            Self::Import => "Import",
            Self::Security => "Security",
            Self::About => "About",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Appearance => "palette",
            Self::Terminal => "square-terminal",
            Self::FileBrowser => "folder-open",
            Self::Shells => "terminal",
            Self::Highlights => "highlighter",
            Self::Import => "import",
            Self::Security => "shield",
            Self::About => "info",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Appearance => "Dark and light mode, a theme for each, and the background blur.",
            Self::Terminal => "Font, scrollback and clipboard behaviour for every session.",
            Self::FileBrowser => "The SFTP file browser beside SSH sessions.",
            Self::Shells => "The shells on this PC listed at the top of the sidebar. Hide the ones you don't use, remove them, or add your own, such as WSL or Git Bash.",
            Self::Highlights => "Colour matching text in terminal output. Rules apply top to bottom; the first match wins.",
            Self::Import => "Bring your hosts over from another SSH client. Nothing changes until you confirm the review.",
            Self::Security => "The vault that holds your passwords and keys.",
            Self::About => "",
        }
    }
}

/// Where the minimap goes.
const MINIMAP_PLACEMENTS: [(MinimapPlacement, &str); 2] =
    [(MinimapPlacement::Beside, "Beside output"), (MinimapPlacement::Over, "Over output")];
/// How long the pointer rests at the right edge before the overlaid minimap opens, in ms.
const MINIMAP_DELAYS: [(u32, &str); 5] = [(0, "None"), (150, "0.15 s"), (300, "0.3 s"), (600, "0.6 s"), (1000, "1 s")];
const SCROLLBACK_CHOICES: [(usize, &str); 4] = [(10_000, "10k"), (50_000, "50k"), (100_000, "100k"), (500_000, "500k")];
/// Idle time before the vault locks, in minutes; 0 keeps it unlocked until Komando closes.
const AUTO_LOCK_CHOICES: [(u32, &str); 8] = [
    (5, "5 min"),
    (15, "15 min"),
    (30, "30 min"),
    (60, "1 h"),
    (8 * 60, "8 h"),
    (24 * 60, "1 day"),
    (7 * 24 * 60, "7 days"),
    (0, "Until restart"),
];

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Settings → Local shells: the fields that add a shell, or edit the one named in `editing`.
pub struct ShellEditor {
    name: Entity<InputState>,
    program: Entity<InputState>,
    args: Entity<InputState>,
    editing: Option<String>,
    error: Option<String>,
}

fn logo() -> Arc<gpui::Image> {
    static LOGO: OnceLock<Arc<gpui::Image>> = OnceLock::new();
    LOGO.get_or_init(|| {
        Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, include_bytes!("../../../assets/icon.png").to_vec()))
    })
    .clone()
}

fn theme_thumbnail(theme: &crate::theme::ThemeSpec) -> Arc<gpui::Image> {
    static IMAGES: OnceLock<std::collections::HashMap<&'static str, Arc<gpui::Image>>> = OnceLock::new();
    IMAGES.get_or_init(|| {
        crate::theme::DARK_THEMES
            .iter()
            .chain(crate::theme::LIGHT_THEMES)
            .map(|t| (t.thumbnail_file, Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, t.thumbnail_png.to_vec()))))
            .collect()
    })[theme.thumbnail_file]
        .clone()
}

impl Workspace {
    pub(super) fn open_settings(&mut self, section: SettingsSection, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_settings_controls(window, cx);
        self.overlay = None;
        self.confirm_delete_set = None;
        self.page = Page::Settings(section);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Sessions;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Sliders and inputs keep their own state, so they are created once and reused.
    fn ensure_settings_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blur_slider.is_none() {
            let value = self.settings.background_blur_intensity as f32;
            let slider = cx.new(|_| SliderState::new().min(0.).max(100.).step(5.).default_value(value));
            let sub = cx.subscribe_in(&slider, window, |this, _, event: &SliderEvent, window, cx| match event {
                // Live preview while dragging; written to disk on release.
                SliderEvent::Change(value) => {
                    this.settings.background_blur_intensity = value.end().round().clamp(0., 100.) as u8;
                    this.apply_appearance(window, cx);
                }
                SliderEvent::Release(_) => {
                    this.save_settings(cx);
                }
            });
            self._subscriptions.push(sub);
            self.blur_slider = Some(slider);
        }
        if self.font_slider.is_none() {
            let value = self.settings.font_size;
            let slider = cx.new(|_| SliderState::new().min(10.).max(24.).step(1.).default_value(value));
            let sub = cx.subscribe_in(&slider, window, |this, _, event: &SliderEvent, window, cx| match event {
                SliderEvent::Change(value) => {
                    this.settings.font_size = value.end().round().clamp(10., 24.);
                    this.apply_appearance(window, cx);
                }
                SliderEvent::Release(_) => {
                    this.save_settings(cx);
                }
            });
            self._subscriptions.push(sub);
            self.font_slider = Some(slider);
        }
        if self.shell_editor.is_none() {
            let name = cx.new(|cx| InputState::new(window, cx).placeholder("Ubuntu (WSL)"));
            let program = cx.new(|cx| InputState::new(window, cx).placeholder("wsl.exe"));
            let args = cx.new(|cx| InputState::new(window, cx).placeholder("-d Ubuntu"));
            for input in [&name, &program, &args] {
                let sub = cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.save_shell(window, cx);
                    }
                });
                self._subscriptions.push(sub);
            }
            self.shell_editor = Some(ShellEditor { name, program, args, editing: None, error: None });
        }
        if self.font_input.is_none() {
            let family = self.settings.font_family.clone();
            let input = cx.new(|cx| InputState::new(window, cx).default_value(family).placeholder("Cascadia Mono"));
            let sub = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let value = input.read(cx).value().trim().to_string();
                    let family = if value.is_empty() { Settings::default().font_family } else { value };
                    if family != this.settings.font_family {
                        this.settings.font_family = family;
                        this.save_settings(cx);
                        this.apply_appearance(window, cx);
                    }
                }
            });
            self._subscriptions.push(sub);
            self.font_input = Some(input);
        }
    }

    /// Push terminal behaviour settings into every open session.
    fn apply_terminal_settings(&mut self, cx: &mut Context<Self>) {
        let (paste, putty) = (self.settings.confirm_multiline_paste, self.settings.putty_clipboard);
        let rail = self.settings.command_rail;
        let minimap = self.settings.minimap;
        let minimap_overlay = self.settings.minimap_placement == MinimapPlacement::Over;
        let minimap_delay = std::time::Duration::from_millis(self.settings.minimap_delay_ms.into());
        for entry in &self.sessions {
            entry.view.update(cx, |v, cx| {
                v.confirm_multiline_paste = paste;
                v.putty_clipboard = putty;
                v.command_rail = rail;
                v.minimap = minimap;
                v.minimap_overlay = minimap_overlay;
                v.minimap_delay = minimap_delay;
                cx.notify();
            });
        }
        self.save_settings(cx);
        cx.notify();
    }

    pub(super) fn render_settings(&self, section: SettingsSection, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let nav = island(p)
            .id("settings-navigation")
            .role(gpui::Role::TabList)
            .aria_label("Settings")
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .p(px(8.))
            .gap(px(2.))
            .child(
                div()
                    .id("settings-back")
                    .role(gpui::Role::Button)
                    .aria_label("Back to sessions")
                    .tab_index(0)
                    .focus_visible(move |s| s.border_2().border_color(p.brand))
                    .h(px(36.))
                    .px(px(8.))
                    .mb(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(p.text_muted)
                    .hover(|s| s.bg(p.hover).text_color(p.text))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_settings(window, cx)))
                    .child(icon_el("arrow-left", 15., p.text_muted))
                    .child(div().flex_1().child("Back to sessions"))
                    .child(kbd("Esc", p)),
            )
            .child(div().px(px(8.)).pt(px(4.)).pb(px(6.)).child(section_label("Settings", p)))
            .children(SettingsSection::ALL.into_iter().map(|s| {
                let active = s == section;
                div()
                    .id(SharedString::from(format!("settings-{}", s.label())))
                    .role(gpui::Role::Tab)
                    .aria_label(s.label())
                    .aria_selected(active)
                    .tab_index(0)
                    .focus_visible(move |st| st.border_2().border_color(p.brand))
                    .h(px(36.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(if active { p.text } else { p.text_muted })
                    .when(active, |el| el.bg(p.active).font_weight(FontWeight::MEDIUM))
                    .when(!active, |el| el.hover(|st| st.bg(p.hover).text_color(p.text)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_settings(s, window, cx)))
                    .child(icon_el(s.icon(), 16., if active { p.brand } else { p.text_faint }))
                    .child(s.label())
            }))
            .child(div().flex_1())
            .when(section != SettingsSection::About, |el| {
                el.child(
                    div()
                        .px(px(8.))
                        .pb(px(4.))
                        .text_size(px(12.))
                        .text_color(p.text_faint)
                        .child(format!("Komando {VERSION}")),
                )
            });

        let body = match section {
            SettingsSection::Appearance => self.settings_appearance(p, cx),
            SettingsSection::Terminal => self.settings_terminal(p, cx),
            SettingsSection::FileBrowser => self.settings_file_browser(p, cx),
            SettingsSection::Shells => self.settings_shells(p, cx),
            SettingsSection::Highlights => self.settings_highlights(p, cx),
            SettingsSection::Import => self.settings_import(p, cx),
            SettingsSection::Security => self.settings_security(p, cx),
            SettingsSection::About => self.settings_about(p, cx),
        };
        let content = island(p)
            .id("settings-content")
            .role(gpui::Role::TabPanel)
            .aria_label(section.label())
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .child(
                div()
                    .max_w(px(760.))
                    .px(px(40.))
                    .py(px(36.))
                    .flex()
                    .flex_col()
                    .gap(px(28.))
                    .when(section != SettingsSection::About, |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .text_size(px(22.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(p.text)
                                        .child(section.label()),
                                )
                                .child(div().text_size(px(13.)).text_color(p.text_muted).child(section.description())),
                        )
                    })
                    .child(body),
            );

        div()
            .id("settings-page")
            .tab_group()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .gap(px(ISLAND_GAP))
            // The title bar above is the top margin.
            .px(px(ISLAND_GAP))
            .pb(px(ISLAND_GAP))
            .child(nav)
            .child(content)
            .into_any_element()
    }

    fn settings_appearance(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let app = self.settings.app_theme;
        let term = self.settings.terminal_theme;
        let blur = self.settings.background_blur;
        let mode_choice = segmented(
            "app-theme",
            &[("Dark", app == AppThemeSetting::Dark), ("Light", app == AppThemeSetting::Light), ("System", app == AppThemeSetting::System)],
            p,
            cx,
            |this, i, window, cx| {
                let theme = [AppThemeSetting::Dark, AppThemeSetting::Light, AppThemeSetting::System][i];
                this.set_app_theme(theme, window, cx);
            },
        );
        let term_choice = segmented(
            "term-theme",
            &[
                ("Follow app", term == TerminalThemeSetting::Follow),
                ("Dark", term == TerminalThemeSetting::Dark),
                ("Light", term == TerminalThemeSetting::Light),
            ],
            p,
            cx,
            |this, i, window, cx| {
                let theme = [TerminalThemeSetting::Follow, TerminalThemeSetting::Dark, TerminalThemeSetting::Light][i];
                this.set_terminal_theme(theme, window, cx);
            },
        );
        let blur_hint = if crate::platform::SUPPORTS_BACKDROP {
            "Windows 11 Mica: your desktop, blurred, shows through the window, even with other windows behind it."
        } else {
            "Windows 11 only. This build runs elsewhere, so the setting is saved but has no effect."
        };
        let slider = self.blur_slider.clone().map(|s| Slider::new(&s).disabled(!blur).into_any_element());
        let mode_hint = match app {
            AppThemeSetting::System => "Follows Windows: dark or light, each with its own theme below.",
            _ => "Each mode has its own theme below, so switching keeps the colours you picked for it.",
        };
        div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .child(group(p, vec![setting_row(p, "Mode", mode_hint, mode_choice)]))
            .when(cfg!(windows), |el| el.child(group(p, vec![setting_row(
                p,
                "Minimize to tray",
                "Keep sessions running in the notification area when minimized. Click the tray icon to restore; right-click for Exit.",
                switch("minimize-to-tray".into(), "Minimize to tray", self.settings.minimize_to_tray, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.settings.minimize_to_tray = !this.settings.minimize_to_tray;
                        #[cfg(windows)]
                        if let Some(tray) = &this.tray { tray.set_enabled(this.settings.minimize_to_tray); }
                        if let Err(error) = this.settings.save(&this.data_dir) {
                            log::error!("Could not save minimize-to-tray setting: {error}");
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            )])))
            .child(group(p, vec![self.theme_picker(true, p, cx), self.theme_picker(false, p, cx)]))
            .child(group(
                p,
                vec![
                    setting_row(
                        p,
                        "Terminal",
                        "The terminal can follow the app or keep the dark or light theme's colours on its own.",
                        term_choice,
                    ),
                    setting_row(
                        p,
                        "Background blur",
                        blur_hint,
                        switch("blur-toggle".into(), "Background blur", blur, p)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.set_background_blur(!blur, window, cx)))
                            .into_any_element(),
                    ),
                    setting_row(
                        p,
                        "Intensity",
                        "How much of the blurred backdrop shows through. The terminal always keeps more of its background.",
                        div()
                            .w(px(260.))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .when(!blur, |el| el.opacity(0.4))
                            .child(div().flex_1().children(slider))
                            .child(value_label(format!("{}%", self.settings.background_blur_intensity), p))
                            .into_any_element(),
                    ),
                ],
            ))
            .into_any_element()
    }

    /// Original theme artwork at the existing compact size. Picking a theme in the mode
    /// in use applies its palette at once; each mode retains its own selection.
    fn theme_picker(&self, dark: bool, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let themes = if dark { crate::theme::DARK_THEMES } else { crate::theme::LIGHT_THEMES };
        let current = self.theme_for(dark).id;
        let in_use = p.dark == dark;
        let selection = crate::theme::hex(if p.dark { crate::theme::BRAND_DARK } else { crate::theme::BRAND });
        let selection_ink = if p.dark { gpui::black() } else { gpui::white() };
        let thumbnails = themes.iter().map(|t| {
            let selected = t.id == current;
            let id = t.id;
            let thumbnail = div()
                .size(px(46.))
                .flex_shrink_0()
                .relative()
                // The PNG includes its rounded corners and transparency. Contain keeps all
                // of its artwork visible without cropping, stretching or recolouring it.
                .child(gpui::img(theme_thumbnail(t)).size_full().object_fit(gpui::ObjectFit::Contain))
                .when(selected, |el| {
                    el.child(
                        div()
                            .absolute()
                            .right(px(6.))
                            .bottom(px(6.))
                            .size(px(19.))
                            .rounded_full()
                            .bg(selection)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon_el("check", 12., selection_ink)),
                    )
                });
            div()
                .id(SharedString::from(format!("theme-{}-{id}", if dark { "dark" } else { "light" })))
                .role(gpui::Role::RadioButton)
                .aria_label(format!("{} theme: {}", if dark { "Dark" } else { "Light" }, t.name))
                .aria_toggled(if selected { gpui::Toggled::True } else { gpui::Toggled::False })
                .tab_index(0)
                .focus_visible(move |s| s.border_2().border_color(selection))
                .w(px(72.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(7.))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.set_theme(dark, id, window, cx)))
                .child(
                    div()
                        .p(px(3.))
                        .rounded(px(14.))
                        .border_2()
                        .border_color(if selected { selection } else { gpui::transparent_black() })
                        .when(!selected, |el| el.hover(|s| s.border_color(p.border)))
                        .child(thumbnail),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(if selected { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                        .text_color(if selected { p.text } else { p.text_muted })
                        .child(t.name),
                )
        });
        div()
            .id(if dark { "dark-theme-picker" } else { "light-theme-picker" })
            .role(gpui::Role::RadioGroup)
            .aria_label(if dark { "Dark theme" } else { "Light theme" })
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.text)
                                    .child(if dark { "Dark theme" } else { "Light theme" }),
                            )
                            .when(in_use, |el| {
                                el.child(
                                    div()
                                        .px(px(7.))
                                        .py(px(1.))
                                        .rounded_full()
                                        .bg(p.brand_soft)
                                        .text_size(px(11.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.brand)
                                        .child("In use"),
                                )
                            }),
                    )
                    .child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(if dark {
                        "Colours of the app and the terminal in dark mode."
                    } else {
                        "Colours of the app and the terminal in light mode."
                    })),
            )
            .child(div().flex().flex_wrap().gap(px(6.)).children(thumbnails))
            .into_any_element()
    }

    fn settings_terminal(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let scrollback = self.settings.scrollback_lines;
        let paste = self.settings.confirm_multiline_paste;
        let putty = self.settings.putty_clipboard;
        let rail = self.settings.command_rail;
        let minimap = self.settings.minimap;
        let placement = self.settings.minimap_placement;
        let delay = self.settings.minimap_delay_ms;
        let placement_choice = segmented(
            "minimap-placement",
            &MINIMAP_PLACEMENTS.map(|(value, label)| (label, value == placement)),
            p,
            cx,
            |this, i, _, cx| {
                this.settings.minimap_placement = MINIMAP_PLACEMENTS[i].0;
                this.apply_terminal_settings(cx);
            },
        );
        let delay_choice = segmented(
            "minimap-delay",
            &MINIMAP_DELAYS.map(|(ms, label)| (label, ms == delay)),
            p,
            cx,
            |this, i, _, cx| {
                this.settings.minimap_delay_ms = MINIMAP_DELAYS[i].0;
                this.apply_terminal_settings(cx);
            },
        );
        let font_slider = self.font_slider.clone().map(|s| Slider::new(&s).into_any_element());
        let font_input = self.font_input.clone().map(|i| Input::new(&i).into_any_element());
        let scrollback_choice = segmented(
            "scrollback",
            &SCROLLBACK_CHOICES.map(|(lines, label)| (label, lines == scrollback)),
            p,
            cx,
            |this, i, _, cx| {
                this.settings.scrollback_lines = SCROLLBACK_CHOICES[i].0;
                this.save_settings(cx);
                cx.notify();
            },
        );
        group(
            p,
            vec![
                setting_row(
                    p,
                    "Font",
                    "A monospaced font installed on this PC. Cascadia Mono is built in. Press Enter to apply.",
                    div().w(px(260.)).children(font_input).into_any_element(),
                ),
                setting_row(
                    p,
                    "Font size",
                    "Applies to all sessions right away.",
                    div()
                        .w(px(260.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(div().flex_1().children(font_slider))
                        .child(value_label(format!("{} px", self.settings.font_size), p))
                        .into_any_element(),
                ),
                setting_row(
                    p,
                    "Scrollback",
                    "Lines each session keeps for scrolling, search and \"Copy output\". Applies to new sessions.",
                    scrollback_choice,
                ),
                setting_row(
                    p,
                    "Confirm multi-line paste",
                    "Ask before pasting more than one line, since each line may run as a command.",
                    switch("paste-confirm".into(), "Confirm multi-line paste", paste, p)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.settings.confirm_multiline_paste = !paste;
                            this.apply_terminal_settings(cx);
                        }))
                        .into_any_element(),
                ),
                setting_row(
                    p,
                    "PuTTY-style clipboard",
                    "Selecting text copies it, right-click pastes. Ctrl+right-click opens the menu.",
                    switch("putty".into(), "PuTTY-style clipboard", putty, p)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.settings.putty_clipboard = !putty;
                            this.apply_terminal_settings(cx);
                        }))
                        .into_any_element(),
                ),
                setting_row(
                    p,
                    "Command rail",
                    "A short line at the left edge for each command you entered. Hover one to see the command, click to jump back to it.",
                    switch("command-rail".into(), "Command rail", rail, p)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.settings.command_rail = !rail;
                            this.apply_terminal_settings(cx);
                        }))
                        .into_any_element(),
                ),
                setting_row(
                    p,
                    "Minimap",
                    "The output in miniature at the right edge, coloured like the text. Drag the slider to scroll, or click a spot to jump there.",
                    switch("minimap".into(), "Minimap", minimap, p)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.settings.minimap = !minimap;
                            this.apply_terminal_settings(cx);
                        }))
                        .into_any_element(),
                ),
            ]
            .into_iter()
            .chain(minimap.then(|| {
                setting_row(
                    p,
                    "Minimap placement",
                    if placement == MinimapPlacement::Beside {
                        "Beside output: always shown in a column of its own, so it never covers output. The terminal is a little narrower."
                    } else {
                        "Over output: the terminal keeps its full width. Rest the pointer on the thin bar at the right edge, or click it, to open the minimap over the output."
                    },
                    placement_choice,
                )
            }))
            .chain((minimap && placement == MinimapPlacement::Over).then(|| {
                setting_row(
                    p,
                    "Open after",
                    "How long the pointer rests on the bar before the minimap opens. It stays open while the pointer is on it.",
                    delay_choice,
                )
            }))
            .collect(),
        )
    }

    fn settings_file_browser(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let (rename, copy_to, move_to) =
            (self.settings.sftp_menu_rename, self.settings.sftp_menu_copy_to, self.settings.sftp_menu_move_to);
        group(
            p,
            vec![
                setting_row(
                    p,
                    "\"Rename\" in the menu",
                    "Show Rename when you right-click a file or folder. F2 renames either way.",
                    switch("sftp-menu-rename".into(), "Rename in the menu", rename, p)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.settings.sftp_menu_rename = !rename;
                            this.apply_sftp_menu_settings(cx);
                        }))
                        .into_any_element(),
                ),
                setting_row(
                    p,
                    "\"Copy to…\" in the menu",
                    "Show Copy to… when you right-click files or folders, to copy them to another folder on the server.",
                    switch("sftp-menu-copy-to".into(), "Copy to in the menu", copy_to, p)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.settings.sftp_menu_copy_to = !copy_to;
                            this.apply_sftp_menu_settings(cx);
                        }))
                        .into_any_element(),
                ),
                setting_row(
                    p,
                    "\"Move to…\" in the menu",
                    "Show Move to… when you right-click files or folders, to move them to another folder on the server.",
                    switch("sftp-menu-move-to".into(), "Move to in the menu", move_to, p)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.settings.sftp_menu_move_to = !move_to;
                            this.apply_sftp_menu_settings(cx);
                        }))
                        .into_any_element(),
                ),
            ],
        )
    }

    fn apply_sftp_menu_settings(&mut self, cx: &mut Context<Self>) {
        let extras = self.sftp_menu_extras();
        for browser in self.sessions.iter().filter_map(|s| s.browser.clone()) {
            browser.update(cx, |b, _| b.menu_extras = extras);
        }
        self.save_settings(cx);
        cx.notify();
    }

    fn settings_security(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let minutes = self.settings.auto_lock_minutes;
        let auto_lock = segmented(
            "auto-lock",
            &AUTO_LOCK_CHOICES.map(|(m, label)| (label, m == minutes)),
            p,
            cx,
            |this, i, _, cx| {
                this.settings.auto_lock_minutes = AUTO_LOCK_CHOICES[i].0;
                this.save_settings(cx);
                cx.notify();
            },
        );
        let (state, action) = if self.vault.is_some() {
            (
                "Unlocked. Saved passwords and keys are filled in automatically.",
                button("vault-lock", "Lock now", ButtonKind::Secondary, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.lock_vault(cx)))
                    .into_any_element(),
            )
        } else {
            (
                "Locked. Connections that need a saved password ask you to unlock first.",
                button("vault-unlock", "Unlock", ButtonKind::Primary, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show_unlock("Unlock to connect with your saved passwords and keys and see them here.", window, cx)))
                    .into_any_element(),
            )
        };
        let saved = self.vault.as_ref().map(|v| v.list().count());
        let known_hosts = self.data_dir.join("known_hosts");
        group(
            p,
            vec![
                setting_row(p, "Vault", state, action),
                setting_row_below(
                    p,
                    "Auto-lock",
                    "Lock the vault after this much inactivity, or keep it unlocked until Komando closes. Open sessions stay connected.",
                    auto_lock,
                ),
                setting_row(
                    p,
                    "Saved credentials",
                    "Passwords and keys in the vault. Hosts and projects only refer to them.",
                    div()
                        .text_size(px(13.))
                        .text_color(p.text_muted)
                        .child(saved.map(|n| format!("{n} saved")).unwrap_or_else(|| "Locked".into()))
                        .into_any_element(),
                ),
                setting_row(
                    p,
                    "Trusted server keys",
                    "Komando's own list of host keys you trusted on first connect.",
                    button("open-known-hosts", "Show file", ButtonKind::Secondary, p)
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.reveal_path(&known_hosts)))
                        .into_any_element(),
                ),
            ],
        )
    }

    fn settings_about(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let data_dir = self.data_dir.clone();
        let path: SharedString = self.data_dir.display().to_string().into();
        div()
            .flex()
            .flex_col()
            .gap(px(28.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(20.))
                    // A hairline ring, so the dark tile doesn't dissolve into a dark panel.
                    .child(
                        div()
                            .size(px(72.))
                            .rounded(px(16.))
                            .border_1()
                            .border_color(p.border)
                            .overflow_hidden()
                            .child(gpui::img(logo()).size_full()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(wordmark(24., p.text))
                            .child(div().text_size(px(13.)).text_color(p.text_muted).child(format!("Version {VERSION}"))),
                    ),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .line_height(px(20.))
                    .text_color(p.text_muted)
                    .child("A fast terminal for system engineers who work on Linux servers from Windows 11."),
            )
            .child(group(
                p,
                vec![
                    div()
                        .flex()
                        .items_center()
                        .gap(px(24.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child("Data folder"))
                                // A path: monospace, on one line, cut off at the end rather than wrapped.
                                .child(
                                    div()
                                        .id("data-path")
                                        .font_family("Cascadia Mono")
                                        .text_size(px(12.))
                                        .text_color(p.text_muted)
                                        .truncate()
                                        .tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
                                        .child(self.data_dir.display().to_string()),
                                ),
                        )
                        .child(
                            button("open-data", "Open folder", ButtonKind::Secondary, p)
                                .flex_shrink_0()
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.open_with_system(&data_dir))),
                        )
                        .into_any_element(),
                ],
            ))
            .child(self.render_updates(p, cx))
            .into_any_element()
    }

    // ---- local shells ------------------------------------------------------------------

    fn save_library_or_toast(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = self.library.save(&self.data_dir) {
            self.show_toast(format!("Could not save: {err:#}"), cx);
        }
        cx.notify();
    }

    fn toggle_shell_hidden(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(shell) = self.library.standalone_shells_mut().iter_mut().find(|s| s.name == name) {
            shell.hidden = !shell.hidden;
        }
        self.save_library_or_toast(cx);
    }

    fn remove_shell(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.library.standalone_shells_mut().retain(|s| s.name != name);
        if self.shell_editor.as_ref().is_some_and(|e| e.editing.as_deref() == Some(name)) {
            self.clear_shell_editor(window, cx);
        }
        self.save_library_or_toast(cx);
    }

    fn restore_detected_shells(&mut self, cx: &mut Context<Self>) {
        self.library.restore_detected_shells();
        self.save_library_or_toast(cx);
    }

    /// Fill the form with a shell to edit it.
    fn edit_shell(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shell) = self.library.standalone_shells().into_iter().find(|s| s.name == name) else { return };
        let Some(editor) = self.shell_editor.as_mut() else { return };
        editor.editing = Some(shell.name.clone());
        editor.error = None;
        let (n, prog, args) = (editor.name.clone(), editor.program.clone(), editor.args.clone());
        n.update(cx, |f, cx| f.set_value(shell.name.clone(), window, cx));
        prog.update(cx, |f, cx| f.set_value(shell.program.clone(), window, cx));
        args.update(cx, |f, cx| f.set_value(shell.args_line(), window, cx));
        n.update(cx, |f, cx| f.focus(window, cx));
        cx.notify();
    }

    fn clear_shell_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.shell_editor.as_mut() else { return };
        editor.editing = None;
        editor.error = None;
        for input in [editor.name.clone(), editor.program.clone(), editor.args.clone()] {
            input.update(cx, |f, cx| f.set_value("", window, cx));
        }
        cx.notify();
    }

    /// Add the shell in the form, or save the changes to the one being edited.
    fn save_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.shell_editor.as_ref() else { return };
        let name = editor.name.read(cx).value().trim().to_string();
        let program = editor.program.read(cx).value().trim().to_string();
        let args = model::split_args(editor.args.read(cx).value().as_ref());
        let editing = editor.editing.clone();
        let error = if name.is_empty() {
            Some("Give the shell a name.".to_string())
        } else if program.is_empty() {
            Some("Enter the program to start, such as wsl.exe or the path to bash.exe.".to_string())
        } else if self.library.standalone_shells().iter().any(|s| s.name == name && Some(&s.name) != editing.as_ref()) {
            Some(format!("There is already a shell called {name}."))
        } else {
            None
        };
        if let Some(error) = error {
            if let Some(editor) = self.shell_editor.as_mut() {
                editor.error = Some(error);
            }
            cx.notify();
            return;
        }
        let shells = self.library.standalone_shells_mut();
        match editing.as_ref().and_then(|old| shells.iter_mut().find(|s| s.name == *old)) {
            Some(shell) => {
                shell.name = name.clone();
                shell.program = program;
                shell.args = args;
            }
            None => shells.push(LocalShell { name: name.clone(), program, args, hidden: false }),
        }
        let saved = self.library.standalone_shells().into_iter().find(|s| s.name == name);
        // Open Sessions follow the change: they stay under the shell, and reconnect with it.
        if let (Some(old), Some(saved)) = (editing, saved) {
            let (old_key, new_key) = (ItemKey::Shell { project: None, name: old }, ItemKey::Shell { project: None, name });
            for entry in self.sessions.iter_mut().filter(|s| s.item == old_key) {
                entry.item = new_key.clone();
                entry.kind = SessionKind::Local { shell: saved.clone() };
            }
            if let Some(id) = self.last_active.remove(&old_key) {
                self.last_active.insert(new_key.clone(), id);
            }
            if self.collapsed_items.remove(&old_key) {
                self.collapsed_items.insert(new_key);
            }
        }
        self.clear_shell_editor(window, cx);
        self.save_library_or_toast(cx);
    }

    fn browse_shell_program(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use program".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(editor) = this.shell_editor.as_ref() else { return };
                let (program, name) = (editor.program.clone(), editor.name.clone());
                program.update(cx, |f, cx| f.set_value(path.display().to_string(), window, cx));
                // Suggest a name from the file when there is none yet.
                if name.read(cx).value().trim().is_empty()
                    && let Some(stem) = path.file_stem()
                {
                    name.update(cx, |f, cx| f.set_value(stem.to_string_lossy().to_string(), window, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn settings_shells(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let shells = self.library.standalone_shells();
        let shown = shells.iter().filter(|s| !s.hidden).count();
        let mut list = div().flex().flex_col().gap(px(2.)).p(px(6.));
        if shells.is_empty() {
            list = list.child(
                div()
                    .p(px(16.))
                    .text_size(px(13.))
                    .text_color(p.text_faint)
                    .child("No local shells. Add one below, or restore the ones found on this PC."),
            );
        }
        let editing = self.shell_editor.as_ref().and_then(|e| e.editing.clone());
        for shell in &shells {
            let name = shell.name.clone();
            let (n_toggle, n_edit, n_remove, n_confirm) = (name.clone(), name.clone(), name.clone(), name.clone());
            let confirming = self.confirm_remove_shell.as_ref().is_some_and(|(n, _)| *n == name);
            let icon = shell_icon(&shell.program).unwrap_or("square-terminal");
            let command = format!("{} {}", shell.program, shell.args_line()).trim().to_string();
            let open = self.sessions_of(&ItemKey::Shell { project: None, name: name.clone() }).count();
            let detail = match open {
                0 => command,
                1 => format!("{command} · 1 session open"),
                n => format!("{command} · {n} sessions open"),
            };
            list = list.child(
                div()
                    .id(SharedString::from(format!("shell-{name}")))
                    .min_h(px(52.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .rounded(px(8.))
                    .when(editing.as_deref() == Some(name.as_str()), |el| el.bg(p.active))
                    .child(div().when(shell.hidden, |el| el.opacity(0.5)).child(icon_el(icon, 18., p.text_muted)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if shell.hidden { p.text_muted } else { p.text })
                                    .truncate()
                                    .child(name.clone()),
                            )
                            .child(div().text_size(px(12.)).text_color(p.text_faint).truncate().child(detail)),
                    )
                    .child(
                        icon_button(SharedString::from(format!("shell-edit-{name}")), "pencil", p)
                            .aria_label(format!("Edit shell: {name}"))
                            .tooltip(|window, cx| Tooltip::new("Edit").build(window, cx))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.confirm_remove_shell = None;
                                this.edit_shell(&n_edit, window, cx);
                            })),
                    )
                    // Removing takes a second click, like deleting a highlight set.
                    .child(if confirming {
                        button(SharedString::from(format!("shell-remove-{name}")), "Remove", ButtonKind::Danger, p)
                            .h(px(28.))
                            .w(px(84.))
                            .justify_center()
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                if this.confirm_remove_shell.as_ref().is_some_and(|(_, at)| at.elapsed() < Duration::from_millis(500)) {
                                    return;
                                }
                                this.confirm_remove_shell = None;
                                this.remove_shell(&n_confirm, window, cx);
                            }))
                            .into_any_element()
                    } else {
                        icon_button(SharedString::from(format!("shell-remove-{name}")), "trash", p)
                            .aria_label(format!("Remove shell: {name}"))
                            .tooltip(|window, cx| Tooltip::new("Remove").build(window, cx))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.confirm_remove_shell = Some((n_remove.clone(), Instant::now()));
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("shell-show-{name}")))
                            .tooltip(|window, cx| Tooltip::new("Show in sidebar").build(window, cx))
                            .child(
                                switch(SharedString::from(format!("shell-toggle-{name}")), format!("Show {name} in the sidebar"), !shell.hidden, p)
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.confirm_remove_shell = None;
                                        this.toggle_shell_hidden(&n_toggle, cx);
                                    })),
                            ),
                    ),
            );
        }

        let form = self.shell_editor.as_ref().map(|editor| {
            let title = match &editor.editing {
                Some(name) => format!("Edit {name}"),
                None => "Add a shell".to_string(),
            };
            let program_field = div()
                .flex()
                .gap(px(8.))
                .child(div().flex_1().child(Input::new(&editor.program)))
                .child(
                    button("shell-browse", "Browse…", ButtonKind::Secondary, p)
                        .flex_shrink_0()
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.browse_shell_program(window, cx))),
                )
                .into_any_element();
            div()
                .rounded(px(12.))
                .border_1()
                .border_color(p.border)
                .bg(p.background)
                .p(px(18.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(title))
                .child(form_field("Name", None, Input::new(&editor.name).into_any_element(), p))
                .child(form_field(
                    "Program",
                    Some("A program on the PATH, such as wsl.exe, or the full path to one."),
                    program_field,
                    p,
                ))
                .child(form_field(
                    "Arguments",
                    Some("Optional. Put an argument with spaces in double quotes."),
                    Input::new(&editor.args).into_any_element(),
                    p,
                ))
                .when_some(editor.error.clone(), |el, error| {
                    el.child(div().text_size(px(12.5)).text_color(p.danger).child(error))
                })
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.))
                        .when(editor.editing.is_some(), |el| {
                            el.child(
                                button("shell-cancel", "Cancel", ButtonKind::Ghost, p)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.clear_shell_editor(window, cx))),
                            )
                        })
                        .child(
                            button("shell-save", if editor.editing.is_some() { "Save" } else { "Add shell" }, ButtonKind::Primary, p)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save_shell(window, cx))),
                        ),
                )
        });

        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.))
                            .text_color(p.text_faint)
                            .child(format!("{shown} of {} shown in the sidebar. A hidden shell is still found by search.", shells.len())),
                    )
                    .child(
                        button("shells-restore", "Restore detected shells", ButtonKind::Secondary, p)
                            .flex_shrink_0()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.restore_detected_shells(cx))),
                    ),
            )
            .child(div().rounded(px(12.)).border_1().border_color(p.border).bg(p.background).child(list))
            .children(form)
            .into_any_element()
    }

    fn settings_highlights(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let term = cx.global::<Appearance>().term.clone();
        let term_bg = crate::theme::rgb_to_hsla(term.background);
        let expanded = self.highlights_expanded.clone();
        let mut list = div().flex().flex_col().gap(px(2.)).p(px(6.));
        if self.highlights.is_empty() {
            list = list.child(
                div()
                    .p(px(16.))
                    .text_size(px(13.))
                    .text_color(p.text_faint)
                    .child("No highlight sets. Import a .json file exported from Komando or Tabby."),
            );
        }
        for set in &self.highlights {
            let on = !self.settings.disabled_highlight_sets.contains(&set.id);
            let open = expanded.as_deref() == Some(set.id.as_str());
            let enabled_rules = set.keywords.iter().filter(|k| k.enabled).count();
            let confirming = self.confirm_delete_set.as_ref().is_some_and(|(id, _)| *id == set.id);
            let (id_toggle, id_export, id_delete, id_open) = (set.id.clone(), set.id.clone(), set.id.clone(), set.id.clone());
            list = list.child(
                div()
                    .id(SharedString::from(format!("hl-set-{}", set.id)))
                    .role(gpui::Role::Button)
                    .aria_label(format!("Highlight set: {}", set.name))
                    .aria_expanded(open)
                    .tab_index(0)
                    .focus_visible(move |s| s.border_2().border_color(p.brand))
                    .h(px(48.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(p.hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.confirm_delete_set = None;
                        this.highlights_expanded =
                            if this.highlights_expanded.as_deref() == Some(id_open.as_str()) { None } else { Some(id_open.clone()) };
                        cx.notify();
                    }))
                    .child(icon_el(if open { "chevron-down" } else { "chevron-right" }, 14., p.text_faint))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).truncate().child(set.name.clone()))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(p.text_faint)
                                    .child(format!("{enabled_rules} of {} rules on", set.keywords.len())),
                            ),
                    )
                    .child(
                        icon_button(SharedString::from(format!("hl-export-{}", set.id)), "download", p)
                            .aria_label(format!("Export highlight set: {}", set.name)).on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.confirm_delete_set = None;
                                this.export_highlight_set(&id_export, window, cx);
                            },
                        )),
                    )
                    // Deleting a whole set can't be undone, so it takes a second click.
                    .child(if confirming {
                        button(SharedString::from(format!("hl-delete-{}", set.id)), "Delete set", ButtonKind::Danger, p)
                            .h(px(28.))
                            .w(px(96.))
                            .justify_center()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                // The button appears under the pointer, so the second click of a
                                // double-click on the trash icon must not count as the confirmation.
                                if this.confirm_delete_set.as_ref().is_some_and(|(_, at)| at.elapsed() < Duration::from_millis(500)) {
                                    return;
                                }
                                this.confirm_delete_set = None;
                                this.delete_highlight_set(&id_delete, cx);
                            }))
                            .into_any_element()
                    } else {
                        icon_button(SharedString::from(format!("hl-delete-{}", set.id)), "trash", p)
                            .aria_label(format!("Delete highlight set: {}", set.name))
                            .tooltip(|window, cx| Tooltip::new("Delete set").build(window, cx))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.confirm_delete_set = Some((id_delete.clone(), Instant::now()));
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .child(switch(SharedString::from(format!("hl-toggle-{}", set.id)), format!("Enable highlight set: {}", set.name), on, p).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.confirm_delete_set = None;
                            this.toggle_highlight_set(&id_toggle, cx);
                        },
                    ))),
            );
            if !open {
                continue;
            }
            for (index, rule) in set.keywords.iter().enumerate() {
                let set_id = set.id.clone();
                let to_hsla = |c: highlight::Rgb8| crate::theme::hex(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32);
                // A rule with its own background keeps its colours; others are made readable on the terminal.
                let fg = rule.fg().map(|c| if rule.bg().is_some() { c } else { highlight::readable_on(c, term.dark) }).map(to_hsla).unwrap_or(p.text);
                let bg = rule.bg().map(to_hsla).unwrap_or(term_bg);
                let mut flags = vec![];
                if rule.is_regex() {
                    flags.push(("regex", "Regular expression"));
                }
                if rule.case_sensitive() {
                    flags.push(("Aa", "Case-sensitive"));
                }
                list = list.child(
                    div()
                        .id(SharedString::from(format!("hl-rule-{}-{index}", set.id)))
                        .h(px(34.))
                        .pl(px(34.))
                        .pr(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .rounded(px(6.))
                        .when(!rule.enabled, |el| el.opacity(0.45))
                        // The pattern as a chip around its text, shown on the terminal's background.
                        .child(
                            div().flex_1().min_w_0().flex().child(
                                div()
                                    .min_w_0()
                                    .px(px(8.))
                                    .py(px(3.))
                                    .rounded(px(5.))
                                    .bg(bg)
                                    .font_family("Cascadia Mono")
                                    .text_size(px(12.))
                                    .text_color(fg)
                                    .when(rule.bold.unwrap_or(false), |el| el.font_weight(FontWeight::BOLD))
                                    .truncate()
                                    .child(rule.text.clone()),
                            ),
                        )
                        .child(div().w(px(84.)).flex().justify_end().gap(px(6.)).children(flags.into_iter().map(|(f, tip)| {
                            div()
                                .id(SharedString::from(format!("hl-flag-{}-{index}-{f}", set.id)))
                                .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                                .px(px(6.))
                                .h(px(18.))
                                .flex()
                                .items_center()
                                .rounded(px(4.))
                                .border_1()
                                .border_color(p.border)
                                .text_size(px(10.5))
                                .text_color(p.text_faint)
                                .child(f)
                        })))
                        .child(
                            small_switch(SharedString::from(format!("hl-rule-toggle-{}-{index}", set.id)), format!("Enable highlight rule: {}", rule.text), rule.enabled, p).on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_highlight_rule(&set_id, index, cx)),
                            ),
                        ),
                );
            }
        }
        let skipped = (!self.highlight_skipped.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .text_size(px(12.))
                .text_color(p.warning)
                .child(format!("{} rule(s) can't be used:", self.highlight_skipped.len()))
                .children(
                    self.highlight_skipped
                        .iter()
                        .take(5)
                        .map(|(text, why)| div().text_color(p.text_muted).truncate().child(format!("{text}: {why}"))),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.))
                            .text_color(p.text_faint)
                            .child(format!("{} rules active. Files use Tabby's highlight format.", self.active_rule_count())),
                    )
                    .child(
                        button("hl-import", "Import…", ButtonKind::Secondary, p)
                            .flex_shrink_0()
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.import_highlights(window, cx))),
                    ),
            )
            .child(div().rounded(px(12.)).border_1().border_color(p.border).bg(p.background).child(list))
            .children(skipped)
            .into_any_element()
    }
}

/// A bordered group of setting rows separated by hairlines.
pub(super) fn group(p: AppPalette, rows: Vec<AnyElement>) -> AnyElement {
    let count = rows.len();
    div()
        .rounded(px(12.))
        .border_1()
        .border_color(p.border)
        .bg(p.background)
        .flex()
        .flex_col()
        .children(rows.into_iter().enumerate().map(move |(i, row)| {
            div()
                .px(px(18.))
                .py(px(14.))
                .when(i + 1 < count, |el| el.border_b_1().border_color(p.border))
                .child(row)
        }))
        .into_any_element()
}

/// Label and explanation above, the control below: for a control too wide to sit beside them.
fn setting_row_below(p: AppPalette, label: &str, hint: &str, control: AnyElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(label.to_string()))
                .child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(hint.to_string())),
        )
        .child(div().flex().child(control))
        .into_any_element()
}

/// Label and explanation on the left, the control on the right.
fn setting_row(p: AppPalette, label: &str, hint: &str, control: AnyElement) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(24.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(label.to_string()))
                .when(!hint.is_empty(), |el| {
                    el.child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(hint.to_string()))
                }),
        )
        .child(div().flex_shrink_0().child(control))
        .into_any_element()
}

fn value_label(text: String, p: AppPalette) -> AnyElement {
    div()
        .min_w(px(44.))
        .text_right()
        .font_family("Cascadia Mono")
        .text_size(px(12.))
        .text_color(p.text_muted)
        .child(text)
        .into_any_element()
}
