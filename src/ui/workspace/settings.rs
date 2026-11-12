//! The Settings page: a full-window page with category navigation on the left.
//! Sessions keep running behind it; Esc or "Back to sessions" returns to them.

use std::sync::{Arc, OnceLock};

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsSection {
    Appearance,
    Terminal,
    Highlights,
    Security,
    About,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 5] =
        [Self::Appearance, Self::Terminal, Self::Highlights, Self::Security, Self::About];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Terminal => "Terminal",
            Self::Highlights => "Highlights",
            Self::Security => "Security",
            Self::About => "About",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Appearance => "palette",
            Self::Terminal => "square-terminal",
            Self::Highlights => "highlighter",
            Self::Security => "shield",
            Self::About => "info",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Appearance => "Dark and light mode, a theme for each, and the background blur.",
            Self::Terminal => "Font, scrollback and clipboard behaviour for every session.",
            Self::Highlights => "Colour matching text in terminal output. Rules apply top to bottom; the first match wins.",
            Self::Security => "The vault that holds your passwords and keys.",
            Self::About => "",
        }
    }
}

const SCROLLBACK_CHOICES: [(usize, &str); 4] = [(10_000, "10k"), (50_000, "50k"), (100_000, "100k"), (500_000, "500k")];
const AUTO_LOCK_CHOICES: [(u32, &str); 5] = [(0, "Off"), (5, "5 min"), (15, "15 min"), (30, "30 min"), (60, "1 h")];

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn logo() -> Arc<gpui::Image> {
    static LOGO: OnceLock<Arc<gpui::Image>> = OnceLock::new();
    LOGO.get_or_init(|| {
        Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, include_bytes!("../../../assets/icon.png").to_vec()))
    })
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
                    let _ = this.settings.save(&this.data_dir);
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
                    let _ = this.settings.save(&this.data_dir);
                }
            });
            self._subscriptions.push(sub);
            self.font_slider = Some(slider);
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
                        let _ = this.settings.save(&this.data_dir);
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
        for entry in &self.sessions {
            entry.view.update(cx, |v, cx| {
                v.confirm_multiline_paste = paste;
                v.putty_clipboard = putty;
                v.command_rail = rail;
                v.minimap = minimap;
                cx.notify();
            });
        }
        let _ = self.settings.save(&self.data_dir);
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
            SettingsSection::Highlights => self.settings_highlights(p, cx),
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

    /// The dark or light themes as bubbles: each shows the theme's background, its text and
    /// its accent. The mode in use is marked, and picking one of its themes applies at once.
    fn theme_picker(&self, dark: bool, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let themes = if dark { crate::theme::DARK_THEMES } else { crate::theme::LIGHT_THEMES };
        let current = self.theme_for(dark).id;
        let in_use = p.dark == dark;
        let bubbles = themes.iter().map(|t| {
            let selected = t.id == current;
            let id = t.id;
            let hex = crate::theme::hex;
            let bubble = div()
                .size(px(46.))
                .rounded_full()
                .bg(hex(t.background))
                .border_1()
                .border_color(if t.dark { gpui::white().opacity(0.12) } else { gpui::black().opacity(0.12) })
                .relative()
                // A line of text and a tag: how the theme's text and accent sit on its background.
                .child(div().absolute().left(px(10.)).top(px(11.)).w(px(14.)).h(px(4.)).rounded_full().bg(hex(t.text_muted)))
                .child(div().absolute().left(px(10.)).top(px(18.)).w(px(8.)).h(px(4.)).rounded_full().bg(hex(t.text_faint)))
                .child(
                    div()
                        .absolute()
                        .right(px(6.))
                        .bottom(px(6.))
                        .size(px(19.))
                        .rounded_full()
                        .bg(hex(t.accent))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(selected, |el| {
                            el.child(icon_el("check", 12., crate::theme::AppPalette::from_theme(t).on_brand))
                        }),
                );
            div()
                .id(SharedString::from(format!("theme-{}-{id}", if dark { "dark" } else { "light" })))
                .role(gpui::Role::RadioButton)
                .aria_label(format!("{} theme: {}", if dark { "Dark" } else { "Light" }, t.name))
                .aria_toggled(if selected { gpui::Toggled::True } else { gpui::Toggled::False })
                .tab_index(0)
                .focus_visible(move |s| s.border_2().border_color(p.brand))
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
                        .rounded_full()
                        .border_2()
                        .border_color(if selected { p.brand } else { gpui::transparent_black() })
                        .when(!selected, |el| el.hover(|s| s.border_color(p.border)))
                        .child(bubble),
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
            .child(div().flex().flex_wrap().gap(px(6.)).children(bubbles))
            .into_any_element()
    }

    fn settings_terminal(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let scrollback = self.settings.scrollback_lines;
        let paste = self.settings.confirm_multiline_paste;
        let putty = self.settings.putty_clipboard;
        let rail = self.settings.command_rail;
        let minimap = self.settings.minimap;
        let font_slider = self.font_slider.clone().map(|s| Slider::new(&s).into_any_element());
        let font_input = self.font_input.clone().map(|i| Input::new(&i).into_any_element());
        let scrollback_choice = segmented(
            "scrollback",
            &SCROLLBACK_CHOICES.map(|(lines, label)| (label, lines == scrollback)),
            p,
            cx,
            |this, i, _, cx| {
                this.settings.scrollback_lines = SCROLLBACK_CHOICES[i].0;
                let _ = this.settings.save(&this.data_dir);
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
                    "Selecting text copies it, right-click pastes.",
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
                    "The whole output in miniature on the right edge, coloured like the text. Click or drag it to scroll.",
                    switch("minimap".into(), "Minimap", minimap, p)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.settings.minimap = !minimap;
                            this.apply_terminal_settings(cx);
                        }))
                        .into_any_element(),
                ),
            ],
        )
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
                let _ = this.settings.save(&this.data_dir);
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
                setting_row(
                    p,
                    "Auto-lock",
                    "Lock the vault after this much inactivity. Open sessions stay connected.",
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
fn group(p: AppPalette, rows: Vec<AnyElement>) -> AnyElement {
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
