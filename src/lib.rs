//! KOMANDO: a fast terminal for system engineers who work on Linux machines over SSH.
//! Windows 11 only (see docs/adr/0001-windows-11-only.md).

pub mod export;
pub mod highlight;
pub mod keys;
pub mod marks;
pub mod model;
pub mod perf;
pub mod platform;
pub mod session;
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
    Workspace,
};

pub const FONTS: [&[u8]; 6] = [
    include_bytes!("../assets/fonts/CascadiaMono-Regular.ttf"),
    include_bytes!("../assets/fonts/CascadiaMono-Bold.ttf"),
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/YoungSerif-Regular.ttf"),
];

/// Fonts, component library and key bindings. Shared by the app and the screenshot tool.
pub fn init(cx: &mut App) {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect())
        .expect("bundled fonts");
    gpui_component::init(cx);
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
    ]);
}

pub struct Options {
    pub data_dir: PathBuf,
    /// Start without the unlock screen (the Vault stays locked).
    pub skip_unlock: bool,
    pub startup: ui::workspace::Startup,
}

pub fn run(options: Options) {
    gpui_platform::application()
        .with_assets(gpui_kit_assets::AllAssets)
        .run(move |cx: &mut App| {
            perf::init();
            update::clean_up();
            init(cx);
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
                    let workspace =
                        cx.new(|cx| Workspace::new(options.data_dir.clone(), options.skip_unlock, options.startup.clone(), window, cx));
                    cx.new(|cx| gpui_component::Root::new(workspace, window, cx))
                },
            )
            .expect("open window");
            cx.activate(true);
        });
}
