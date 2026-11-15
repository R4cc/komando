//! Source exports plus individually tuned native palettes; conversion happens once.

use std::{collections::BTreeMap, sync::LazyLock};
use serde::Deserialize;
use super::{ThemeColors, ThemeShadow, ThemeSpec};

#[derive(Deserialize)]
struct Source<'a> {
    id: &'a str,
    name: &'a str,
    source: &'a str,
    styles: BTreeMap<String, BTreeMap<String, String>>,
}

static SOURCES: LazyLock<Vec<Source<'static>>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../assets/themes/catalog.json")).expect("bundled theme catalog")
});

type Refinements = BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>;
static REFINEMENTS: LazyLock<Refinements> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../assets/themes/refinements.json")).expect("bundled theme refinements")
});

pub(super) fn load(dark: bool) -> Vec<ThemeSpec> {
    SOURCES.iter().map(|source| {
        let mode = if dark { "dark" } else { "light" };
        let mut styles = source.styles[mode].clone();
        styles.extend(REFINEMENTS[source.id][mode].clone());
        let color = |key: &str| parse_color(&styles[key]).unwrap_or_else(|e| panic!("{} {key}: {e}", source.name));
        let number = |key: &str| styles[key].trim_end_matches("px").parse::<f32>().expect("theme dimension");
        ThemeSpec {
            id: source.id, name: source.name, source: source.source, dark,
            colors: ThemeColors::read(&color),
            font_sans: font(&styles["font-sans"], false),
            font_mono: font(&styles["font-mono"], true),
            radius: styles["radius"].trim_end_matches("rem").parse::<f32>().expect("theme radius") * 16.,
            shadow: ThemeShadow {
                color: color("shadow-color"), opacity: number("shadow-opacity"),
                x: number("shadow-offset-x"), y: number("shadow-offset-y"),
                blur: number("shadow-blur"), spread: number("shadow-spread"),
            },
        }
    }).collect()
}

fn font(value: &str, mono: bool) -> &'static str {
    // DirectWrite exposes this variable font's optical-size family under its legacy name.
    if !mono && value.starts_with("DM Sans") { return if cfg!(windows) { "DM Sans 14pt" } else { "DM Sans" }; }
    let families = if mono {
        &["Geist Mono", "JetBrains Mono", "IBM Plex Mono", "Fira Code"][..]
    } else {
        &["Outfit", "Plus Jakarta Sans", "DM Sans", "Inter", "Segoe UI"][..]
    };
    families.iter().copied().find(|name| value.starts_with(name))
        // FL-Theme specifies Inter as its mono font; the native terminal requires fixed width.
        .unwrap_or(if mono { "JetBrains Mono" } else { "Geist" })
}

pub(super) fn parse_color(value: &str) -> Result<u32, String> {
    let value = value.trim();
    let hsla = if let Some(hex) = value.strip_prefix('#') {
        let expanded;
        let hex = if hex.len() == 3 || hex.len() == 4 {
            expanded = hex.chars().flat_map(|c| [c, c]).collect::<String>();
            expanded.as_str()
        } else { hex };
        if hex.len() != 6 && hex.len() != 8 { return Err(format!("invalid hex {value}")); }
        let rgb = u32::from_str_radix(&hex[..6], 16).map_err(|e| e.to_string())?;
        if hex.len() == 8 && &hex[6..] != "ff" && &hex[6..] != "FF" { return Err(format!("non-opaque token {value}")); }
        return Ok(rgb);
    } else if let Some(args) = value.strip_prefix("oklch(").and_then(|s| s.strip_suffix(')')) {
        let v: Vec<f32> = args.split_whitespace().map(str::parse).collect::<Result<_, _>>().map_err(|e: std::num::ParseFloatError| e.to_string())?;
        if v.len() != 3 || !v.iter().all(|v| v.is_finite()) { return Err(format!("invalid OKLCH {value}")); }
        gpui_component::oklch(v[0], v[1], v[2])
    } else if value == "hsl(0 0% 0%)" {
        return Ok(0);
    } else { return Err(format!("unsupported bundled color {value}")); };
    let rgba: gpui::Rgba = hsla.into();
    let channel = |v: f32| (v.clamp(0., 1.) * 255.).round() as u32;
    Ok((channel(rgba.r) << 16) | (channel(rgba.g) << 8) | channel(rgba.b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn komando_default_keeps_the_supplied_colors_fonts_radius_and_zero_shadows() {
        let source = SOURCES.iter().find(|s| s.id == crate::theme::DEFAULT_THEME).unwrap();
        for dark in [false, true] {
            let styles = &source.styles[if dark { "dark" } else { "light" }];
            let expected = ThemeColors::read(&|key| parse_color(&styles[key]).unwrap());
            let resolved = load(dark).into_iter().find(|t| t.id == crate::theme::DEFAULT_THEME).unwrap();
            assert_eq!(resolved.colors, expected, "the supplied default must not inherit palette refinements");
            assert_eq!(resolved.colors.background == 0, dark);
            assert_eq!(resolved.font_sans, "Inter");
            assert_eq!(resolved.font_mono, "JetBrains Mono");
            assert_eq!(resolved.radius, 8.);
            assert_eq!(resolved.shadow, ThemeShadow { color: 0, opacity: 0., x: 0., y: 0., blur: 0., spread: 0. });
        }
    }

    #[test]
    fn css_tokens_preserve_hex_and_convert_oklch() {
        assert_eq!(parse_color("#FFF").unwrap(), 0xffffff);
        assert_eq!(parse_color("#FFFF").unwrap(), 0xffffff);
        assert_eq!(parse_color("#c96442").unwrap(), 0xc96442);
        assert_eq!(parse_color("oklch(0 0 0)").unwrap(), 0);
        assert_eq!(parse_color("oklch(1 0 0)").unwrap(), 0xffffff);
        assert_eq!(parse_color("oklch(0.985 0 0)").unwrap(), 0xfafafa);
        assert!(parse_color("oklch(NaN 0 0)").is_err());
        assert!(parse_color("#12345").is_err());
    }
}
