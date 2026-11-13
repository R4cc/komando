//! KOMANDO: a fast terminal for system engineers who work on Linux machines over SSH.
//! Windows 11 only (see docs/adr/0001-windows-11-only.md).

#[cfg(all(test, unix))]
mod compat_tests;
pub mod export;
pub mod favorites;
pub mod highlight;
pub mod import;
pub mod keys;
pub mod logging;
pub mod marks;
pub mod model;
pub mod perf;
pub mod platform;
pub mod session;
pub mod sftp;
pub mod theme;
pub mod ui;
pub mod update;
pub mod vault;

use std::borrow::Cow;
use std::path::PathBuf;

use gpui::{App, AppContext as _, Bounds, KeyBinding, WindowBounds, WindowOptions, px, size};

use crate::ui::terminal_view::{self, Copy, Paste, ScrollPageDown, ScrollPageUp};
use crate::ui::workspace::{
    CloseSession, CopyOutputFile, CopyOutputText, LockVault, NewSession, NextSession, OpenSettings, PrevSession, QuickConnect,
    ToggleSftp, Workspace,
};

pub const FONTS: [&[u8]; 6] = [
    include_bytes!("../assets/fonts/CascadiaMono-Regular.ttf"),
    include_bytes!("../assets/fonts/CascadiaMono-Bold.ttf"),
    include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Geist-Bold.ttf"),
];

/// Fonts, component library and key bindings. Shared by the app and the screenshot tool.
pub fn init(cx: &mut App) {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect())
        .expect("bundled fonts");
    perf::startup("fonts added");
    gpui_component::init(cx);
    perf::startup("component library ready");
    let t = Some(terminal_view::CONTEXT);
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-c", Copy, t),
        KeyBinding::new("ctrl-shift-v", Paste, t),
        KeyBinding::new("shift-pageup", ScrollPageUp, t),
        KeyBinding::new("shift-pagedown", ScrollPageDown, t),
        KeyBinding::new("ctrl-shift-k", QuickConnect, None),
        KeyBinding::new("ctrl-shift-t", NewSession, None),
        KeyBinding::new("ctrl-shift-w", CloseSession, None),
        KeyBinding::new("ctrl-tab", NextSession, None),
        KeyBinding::new("ctrl-shift-tab", PrevSession, None),
        KeyBinding::new("ctrl-shift-a", CopyOutputText, None),
        KeyBinding::new("ctrl-shift-s", CopyOutputFile, None),
        KeyBinding::new("ctrl-shift-l", LockVault, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-shift-e", ToggleSftp, None),
    ]);
}

/// The Komando logo (">_Komando."), one shape so it can be drawn in any colour.
pub const LOGO_SVG: &str = "komando/logo.svg";

/// The bundled icons, plus Komando's own artwork.
struct Assets;

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO_SVG {
            return Ok(Some(Cow::Borrowed(include_bytes!("../assets/logo.svg"))));
        }
        gpui_kit_assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<gpui::SharedString>> {
        gpui_kit_assets::AllAssets.list(path)
    }
}

pub struct Options {
    pub data_dir: PathBuf,
    pub startup: ui::workspace::Startup,
}

pub fn run(options: Options) {
    // First, so the graphics driver loads while the rest of startup runs.
    platform::warm_up_gpu();
    logging::init(&options.data_dir);
    perf::startup("logging ready");
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            perf::startup("platform ready");
            perf::init();
            update::clean_up();
            export::clean_up();
            init(cx);
            perf::startup("app initialised");
            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(720.), px(420.))),
                    // Komando draws its own title bar (see `Workspace::render_title_bar`).
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Komando".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    app_id: Some("komando".into()),
                    ..Default::default()
                },
                |window, cx| {
                    perf::startup("window created");
                    let workspace =
                        cx.new(|cx| Workspace::new(options.data_dir.clone(), options.startup.clone(), window, cx));
                    window.on_next_frame(|_, _| perf::startup("first frame drawn"));
                    cx.new(|cx| gpui_component::Root::new(workspace, window, cx))
                },
            )
            .expect("open window");
            perf::startup("window opened");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;

    #[test]
    fn the_logo_is_served_alongside_the_icons() {
        let logo = super::Assets.load(super::LOGO_SVG).unwrap().expect("logo");
        assert!(std::str::from_utf8(&logo).unwrap().starts_with("<svg"));
        assert!(super::Assets.load("icons/server.svg").unwrap().is_some());
    }
}
