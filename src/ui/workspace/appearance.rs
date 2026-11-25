//! Themes, fonts and the window's look, applied to every open view.

use super::*;

impl Workspace {
    // ---- appearance -------------------------------------------------------------------

    /// Whether the app is dark in `window`'s appearance.
    pub(super) fn app_dark(&self, window: &Window) -> bool {
        let system_dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
        match self.settings.app_theme {
            AppThemeSetting::System => system_dark,
            AppThemeSetting::Light => false,
            AppThemeSetting::Dark => true,
        }
    }

    /// Give a window the app's backdrop (Mica, when on), light or dark frame and frame colors.
    /// Returns whether the backdrop shows.
    pub(super) fn style_window(&self, window: &mut Window, app_dark: bool) -> bool {
        // Mica: Windows blurs the desktop behind the window; our surfaces become partly
        // transparent so it shows through. The slider sets how much.
        let blur = crate::platform::SUPPORTS_BACKDROP && self.settings.background_blur;
        window.set_background_appearance(if blur {
            gpui::WindowBackgroundAppearance::MicaBackdrop
        } else {
            gpui::WindowBackgroundAppearance::Opaque
        });
        crate::platform::set_window_dark_mode(window, app_dark);
        crate::platform::set_app_icon(window, self.settings.app_icon);
        let backdrop_enabled = crate::platform::set_backdrop(window, blur);
        if blur && !backdrop_enabled {
            window.set_background_appearance(gpui::WindowBackgroundAppearance::Opaque);
        }
        let opaque = AppPalette::from_theme(self.theme_for(app_dark));
        crate::platform::set_native_caption_color(
            window,
            (!backdrop_enabled).then(|| crate::theme::packed(opaque.canvas)),
        );
        backdrop_enabled
    }

    pub(super) fn apply_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app_dark = self.app_dark(window);
        let term_dark = self.terminal_dark(app_dark);
        let backdrop_enabled = self.style_window(window, app_dark);
        #[cfg(windows)]
        if let Some(tray) = &self.tray { tray.refresh_icon(); }
        for handle in self.own_windows() {
            let _ = handle.update(cx, |_, window, _| self.style_window(window, app_dark));
        }
        let t = if backdrop_enabled { self.settings.background_blur_intensity as f32 / 100. } else { 0. };
        cx.set_global(Appearance {
            app: AppPalette::from_theme(self.theme_for(app_dark)).translucent(t),
            term: TerminalPalette::for_mode(term_dark, self.terminal_colors_for(term_dark), self.theme_for(term_dark)).translucent(t, app_dark),
            font_family: if self.settings.font_family.trim().is_empty() { crate::theme::TERMINAL_FONT.into() } else { self.settings.font_family.clone().into() },
            font_size: self.terminal_font_size(),
            font_weight: font_weight(self.settings.font_weight),
            bold_font_weight: font_weight(self.settings.bold_font_weight),
        });
        gpui_component::Theme::change(
            if app_dark { gpui_component::ThemeMode::Dark } else { gpui_component::ThemeMode::Light },
            Some(window),
            cx,
        );
        let app = cx.global::<Appearance>().app;
        gpui_component::Theme::update(cx, |theme| app.apply_components(theme));
        cx.refresh_windows();
    }

    /// Whether the terminal is dark while the app is dark (`app_dark`) or light.
    pub(super) fn terminal_dark(&self, app_dark: bool) -> bool {
        match self.settings.terminal_theme {
            TerminalThemeSetting::Follow => app_dark,
            TerminalThemeSetting::Light => false,
            TerminalThemeSetting::Dark => true,
        }
    }

    /// The Theme picked for dark or light mode.
    pub(super) fn theme_for(&self, dark: bool) -> &'static crate::theme::ThemeSpec {
        crate::theme::theme(dark, if dark { &self.settings.dark_theme } else { &self.settings.light_theme })
    }

    /// The Terminal Colors id picked for a dark or light terminal.
    pub(super) fn terminal_colors_for(&self, dark: bool) -> &str {
        if dark { &self.settings.dark_terminal_colors } else { &self.settings.light_terminal_colors }
    }

    pub(super) fn set_terminal_colors(&mut self, dark: bool, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if dark {
            self.settings.dark_terminal_colors = id.into();
        } else {
            self.settings.light_terminal_colors = id.into();
        }
        self.save_settings(cx);
        self.apply_appearance(window, cx);
    }

    pub(super) fn set_theme(&mut self, dark: bool, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if dark {
            self.settings.dark_theme = id.into();
        } else {
            self.settings.light_theme = id.into();
        }
        self.save_settings(cx);
        self.apply_appearance(window, cx);
    }

    pub(super) fn set_app_theme(&mut self, theme: AppThemeSetting, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.app_theme = theme;
        self.appearance_other_mode = false;
        self.save_settings(cx);
        self.apply_appearance(window, cx);
    }

    pub(super) fn set_terminal_theme(&mut self, theme: TerminalThemeSetting, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.terminal_theme = theme;
        self.save_settings(cx);
        self.apply_appearance(window, cx);
    }

    pub(super) fn set_background_blur(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.background_blur = on;
        self.save_settings(cx);
        self.apply_appearance(window, cx);
    }
}
