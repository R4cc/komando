//! Map semantic theme roles onto library-owned inputs, menus and controls in one place.

use super::{AppPalette, hex, readable};

impl AppPalette {
    pub fn apply_components(self, theme: &mut gpui_component::Theme) {
        let c = self.navigation;
        theme.font_family = self.font_family.into();
        theme.mono_font_family = self.mono_family.into();
        theme.radius = gpui::px(self.radius);
        theme.radius_lg = self.corner(12.);
        theme.shadow = self.shadow.opacity > 0.;
        let colors = &mut theme.colors;
        // Workspace paints the canvas; keep Root transparent for optional native Mica.
        colors.background = gpui::transparent_black();
        colors.foreground = self.text;
        colors.border = self.border;
        colors.input = self.input;
        colors.popover = self.popover;
        colors.popover_foreground = self.popover_text;
        colors.accent = self.hover;
        colors.accent_foreground = self.hover_text;
        colors.muted = hex(c.muted);
        colors.muted_foreground = self.text_muted;
        colors.primary = self.brand;
        colors.primary_foreground = self.on_brand;
        colors.primary_hover = self.brand;
        colors.primary_active = self.brand;
        colors.secondary = self.secondary;
        colors.secondary_foreground = self.on_secondary;
        colors.secondary_hover = self.secondary;
        colors.secondary_active = self.secondary;
        colors.button = self.secondary;
        colors.button_foreground = self.on_secondary;
        colors.button_hover = self.secondary;
        colors.button_active = self.secondary;
        colors.button_primary = self.brand;
        colors.button_primary_foreground = self.on_brand;
        colors.button_primary_hover = self.brand;
        colors.button_primary_active = self.brand;
        colors.button_secondary = self.secondary;
        colors.button_secondary_foreground = self.on_secondary;
        colors.button_secondary_hover = self.secondary;
        colors.button_secondary_active = self.secondary;
        colors.danger = self.danger;
        colors.danger_foreground = self.on_danger;
        colors.button_danger = self.danger;
        colors.button_danger_foreground = self.on_danger;
        colors.ring = self.focus;
        colors.caret = self.focus;
        colors.selection = self.active;
        colors.link = hex(readable(c.primary, c.popover, 4.5));
        colors.link_hover = colors.link;
        colors.link_active = colors.link;
        colors.list = self.popover;
        colors.list_hover = self.hover;
        colors.list_active = self.active;
        colors.list_active_border = self.focus;
        colors.slider_bar = self.brand;
        colors.slider_thumb = self.brand;
        colors.switch = self.brand;
        colors.switch_thumb = self.on_brand;
        colors.progress_bar = self.brand;
        colors.scrollbar = self.background;
        colors.scrollbar_thumb = self.border;
        colors.scrollbar_thumb_hover = self.focus;
        colors.sidebar = self.sidebar;
        colors.sidebar_foreground = hex(c.sidebar_foreground);
        colors.sidebar_primary = hex(c.sidebar_primary);
        colors.sidebar_primary_foreground = hex(readable(c.sidebar_primary_foreground, c.sidebar_primary, 4.5));
        colors.sidebar_accent = hex(c.sidebar_accent);
        colors.sidebar_accent_foreground = hex(readable(c.sidebar_accent_foreground, c.sidebar_accent, 4.5));
        colors.sidebar_border = hex(c.sidebar_border);
        colors.chart_1 = hex(c.chart_1);
        colors.chart_2 = hex(c.chart_2);
        colors.chart_3 = hex(c.chart_3);
        colors.chart_4 = hex(c.chart_4);
        colors.chart_5 = hex(c.chart_5);
    }
}
