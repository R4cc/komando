//! Bundled, full-color technology artwork alongside the Lucide interface icons.
use std::sync::{Arc, OnceLock};
use crate::model::LocalShell;

mod catalog;
pub use catalog::ICONS;
mod symbols;
pub use symbols::SYMBOLS;

/// Theme-colored SVG symbols, separate from full-color Devicon artwork.
pub struct SymbolIcon {
    pub id: &'static str,
    pub search: &'static str,
    pub svg: &'static [u8],
    color_dark: Option<u32>,
    color_light: Option<u32>,
}

impl SymbolIcon {
    pub fn color(&self, dark: bool) -> Option<gpui::Hsla> {
        (if dark { self.color_dark } else { self.color_light }).map(crate::theme::hex)
    }
}

pub fn find_symbol(id: &str) -> Option<&'static SymbolIcon> {
    SYMBOLS.iter().find(|icon| icon.id == id)
}

pub struct BrandIcon {
    pub id: &'static str,
    pub search: &'static str,
    svg: &'static [u8],
    image: OnceLock<Arc<gpui::Image>>,
}

impl BrandIcon {
    pub fn image(&self) -> Arc<gpui::Image> {
        self.image.get_or_init(|| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Svg, self.svg.to_vec()))).clone()
    }
}

pub fn find(id: &str) -> Option<&'static BrandIcon> {
    ICONS.binary_search_by_key(&id, |icon| icon.id).ok().map(|i| &ICONS[i])
}

pub fn label(id: &str) -> String {
    if id == "shell:cmd" { return "Command Prompt".into(); }
    if let Some(name) = id.strip_prefix("tabler:") { return format!("{} (Tabler outline)", name.replace('-', " ")); }
    if let Some(name) = id.strip_prefix("simpleicons:") { return format!("{name} (Simple Icons)"); }
    id.strip_prefix("devicon:").map(|name| format!("{name} (Devicon)")).unwrap_or_else(|| id.to_string())
}

pub fn distro_icon(name: &str) -> &'static str {
    let name = name.to_ascii_lowercase();
    for (needle, icon) in [
        ("ubuntu", "simpleicons:ubuntu"), ("debian", "simpleicons:debian"), ("kali", "simpleicons:kalilinux"),
        ("opensuse", "simpleicons:opensuse"), ("suse", "simpleicons:opensuse"), ("fedora", "simpleicons:fedora"),
        ("almalinux", "simpleicons:almalinux"), ("rocky", "simpleicons:rockylinux"), ("centos", "simpleicons:centos"),
        ("redhat", "simpleicons:redhat"), ("rhel", "simpleicons:redhat"), ("arch", "simpleicons:archlinux"),
        ("gentoo", "simpleicons:gentoo"), ("nixos", "simpleicons:nixos"), ("mint", "simpleicons:linuxmint"),
    ] {
        if name.contains(needle) {
            return icon;
        }
    }
    "simpleicons:linux"
}

/// Explicit artwork wins; WSL's -d/--distribution argument identifies its distro.
pub fn shell_icon(shell: &LocalShell) -> &str {
    if let Some(icon) = shell.icon.as_deref().filter(|icon| !icon.is_empty()) {
        return icon;
    }
    let file = shell.program.rsplit(['\\', '/']).next().unwrap_or(&shell.program).to_ascii_lowercase();
    if file.trim_end_matches(".exe") == "wsl" {
        let distro = shell.args.iter().enumerate().find_map(|(i, arg)| {
            if arg == "-d" || arg == "--distribution" { shell.args.get(i + 1).map(String::as_str) }
            else { arg.strip_prefix("--distribution=") }
        });
        return distro_icon(distro.unwrap_or(&shell.name));
    }
    crate::ui::widgets::shell_icon(&shell.program).unwrap_or("square-terminal")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distro_arguments_and_explicit_choices_survive() {
        let mut shell = LocalShell { name: "My Linux".into(), program: "wsl.exe".into(), args: vec!["--distribution".into(), "Ubuntu-24.04".into()], ..Default::default() };
        assert_eq!(shell_icon(&shell), "simpleicons:ubuntu");
        shell.args = vec!["--distribution=Debian".into()];
        assert_eq!(shell_icon(&shell), "simpleicons:debian");
        shell.icon = Some("devicon:fedora".into());
        assert_eq!(shell_icon(&shell), "devicon:fedora");
        assert_eq!(distro_icon("Unknown custom distro"), "simpleicons:linux");
        for name in ["Ubuntu", "Debian", "kali-linux", "openSUSE-Tumbleweed", "FedoraLinux-42", "ArchLinux", "AlmaLinux", "RockyLinux"] {
            assert!(find_symbol(distro_icon(name)).is_some(), "{name}");
        }
    }
    #[test]
    fn catalog_is_sorted_unique_and_contains_svg_artwork() {
        assert!(ICONS.len() > 400);
        assert!(ICONS.windows(2).all(|pair| pair[0].id < pair[1].id));
        for icon in &ICONS {
            assert!(std::str::from_utf8(icon.svg).unwrap().contains("<svg"), "{}", icon.id);
            assert!(find(icon.id).is_some());
        }
    }
    #[test]
    fn bundled_artwork_decodes_in_the_actual_svg_renderer() {
        let renderer = gpui::SvgRenderer::new(Arc::new(gpui_kit_assets::AllAssets));
        for id in ["devicon:powershell", "devicon:linux", "devicon:ubuntu", "devicon:debian", "devicon:docker", "devicon:kubernetes", "devicon:github"] {
            renderer.render_single_frame(find(id).unwrap().svg, 1.).unwrap_or_else(|e| panic!("{id}: {e}"));
        }
        for icon in &SYMBOLS {
            renderer.render_single_frame(icon.svg, 1.).unwrap_or_else(|e| panic!("{}: {e}", icon.id));
        }
    }
}
