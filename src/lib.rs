//! KOMANDO: a fast terminal for system engineers who work on Linux machines over SSH.
//! Windows 11 only (see docs/adr/0001-windows-11-only.md).

#[cfg(all(test, unix))]
mod compat_tests;
pub mod desktop;
pub mod elevate;
pub mod export;
pub mod favorites;
pub mod highlight;
pub mod history;
pub mod icons;
pub mod import;
pub mod keys;
pub mod logging;
pub mod marks;
pub mod model;
pub mod perf;
pub mod platform;
pub mod search;
pub mod session;
pub mod sftp;
pub mod share;
pub mod theme;
#[cfg(test)]
mod throughput_tests;
pub mod ui;
pub mod update;
pub mod vault;
pub mod wsl;

use std::borrow::Cow;
use std::path::PathBuf;

use gpui::{App, AppContext as _, Bounds, KeyBinding, WindowBounds, WindowOptions, px, size};

use crate::ui::terminal_view::{
    self, Copy, Find, Paste, ScrollPageDown, ScrollPageUp, SearchNewer, SearchOlder, SendBackTab, SendTab, SelectOutput,
};
use crate::ui::workspace::{
    CloseSession, CopyOutputFile, CopyOutputText, LockVault, NewSession, NextSession, OpenSettings, PickerDown, PickerUp,
    PrevSession, QuickConnect, ToggleSftp, ToggleSidebar, Workspace,
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
    cx.bind_keys(key_bindings());
}

fn key_bindings() -> Vec<KeyBinding> {
    let t = Some(terminal_view::CONTEXT);
    vec![
        // Key bindings run before on_key_down; override Root's focus traversal in a terminal.
        KeyBinding::new("tab", SendTab, t),
        KeyBinding::new("shift-tab", SendBackTab, t),
        // A remote desktop takes Tab too.
        KeyBinding::new("tab", ui::desktop_view::SendTab, Some(ui::desktop_view::CONTEXT)),
        KeyBinding::new("shift-tab", ui::desktop_view::SendBackTab, Some(ui::desktop_view::CONTEXT)),
        KeyBinding::new("ctrl-shift-c", Copy, t),
        KeyBinding::new("ctrl-shift-v", Paste, t),
        KeyBinding::new("shift-pageup", ScrollPageUp, t),
        KeyBinding::new("shift-pagedown", ScrollPageDown, t),
        // Ctrl+F stays the shell's (forward a character in bash, page down in vim and less).
        KeyBinding::new("ctrl-shift-f", Find, t),
        // In the search bar's field Up and Down step through the matches. They are bound after
        // the component library's own, so in that field they win.
        KeyBinding::new("up", SearchOlder, Some("TerminalSearch > Input")),
        KeyBinding::new("down", SearchNewer, Some("TerminalSearch > Input")),
        // In quick connect's field Up and Down move through the results.
        KeyBinding::new("up", PickerUp, Some("QuickConnect > Input")),
        KeyBinding::new("down", PickerDown, Some("QuickConnect > Input")),
        KeyBinding::new("ctrl-shift-k", QuickConnect, None),
        KeyBinding::new("ctrl-shift-t", NewSession, None),
        KeyBinding::new("ctrl-shift-w", CloseSession, None),
        KeyBinding::new("ctrl-tab", NextSession, None),
        KeyBinding::new("ctrl-shift-tab", PrevSession, None),
        KeyBinding::new("ctrl-shift-a", CopyOutputText, None),
        // Global bindings have the deepest context's priority too; register the terminal's
        // override last so it can select locally or propagate to Copy output when disabled.
        KeyBinding::new("ctrl-shift-a", SelectOutput, t),
        KeyBinding::new("ctrl-shift-s", CopyOutputFile, None),
        KeyBinding::new("ctrl-shift-l", LockVault, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-shift-e", ToggleSftp, None),
        KeyBinding::new("ctrl-shift-b", ToggleSidebar, Some("Workspace")),
    ]
}

/// The full-color Komando sunrise logo, shared by every in-app wordmark.
pub const LOGO_PNG: &str = "komando/logo.png";

/// The bundled icons, plus Komando's own artwork.
struct Assets;

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO_PNG {
            return Ok(Some(Cow::Borrowed(include_bytes!("../assets/logo.png"))));
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
                        // Like Windows' own consoles, so the taskbar tells the windows apart.
                        title: Some(if elevate::is_elevated() { "Administrator: Komando" } else { "Komando" }.into()),
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
    fn terminal_select_all_has_the_existing_copy_output_fallback() {
        use gpui::{KeyContext, Keymap, Keystroke};
        let keymap = Keymap::new(super::key_bindings());
        let terminal = [KeyContext::parse("Root").unwrap(), KeyContext::parse("Terminal").unwrap()];
        let key = Keystroke::parse("ctrl-shift-a").unwrap();
        let (bindings, pending) = keymap.bindings_for_input(&[key.clone()], &terminal);
        assert!(!pending);
        assert_eq!(bindings.len(), 2);
        assert!(bindings[0].action().as_any().is::<super::SelectOutput>());
        assert!(bindings[1].action().as_any().is::<super::CopyOutputText>());
        let settings = [KeyContext::parse("Root").unwrap()];
        let (bindings, _) = keymap.bindings_for_input(&[key], &settings);
        assert_eq!(bindings.len(), 1);
        assert!(bindings[0].action().as_any().is::<super::CopyOutputText>());
    }

    #[test]
    fn terminal_tab_bindings_precede_focus_navigation_and_keep_its_fallback() {
        use gpui::{KeyBinding, KeyContext, Keymap, Keystroke};
        use super::{OpenSettings, SendBackTab, SendTab};

        // Root registers these keys before Komando's bindings. Use a distinct action to
        // identify its focus-navigation fallback without depending on its private action type.
        let mut keymap = Keymap::new(vec![
            KeyBinding::new("tab", OpenSettings, Some("Root")),
            KeyBinding::new("shift-tab", OpenSettings, Some("Root")),
        ]);
        keymap.add_bindings(super::key_bindings());
        let terminal = [KeyContext::parse("Root").unwrap(), KeyContext::parse("Terminal").unwrap()];
        for (key, backwards) in [("tab", false), ("shift-tab", true)] {
            let keystroke = Keystroke::parse(key).unwrap();
            let (bindings, pending) = keymap.bindings_for_input(&[keystroke.clone()], &terminal);
            assert!(!pending);
            assert_eq!(bindings.len(), 2);
            if backwards {
                assert!(bindings[0].action().as_any().is::<SendBackTab>());
            } else {
                assert!(bindings[0].action().as_any().is::<SendTab>());
            }
            // A child field/button makes send_tab propagate, so Root must remain available.
            assert!(bindings[1].action().as_any().is::<OpenSettings>());

            let settings = [KeyContext::parse("Root").unwrap(), KeyContext::parse("Input").unwrap()];
            let (bindings, _) = keymap.bindings_for_input(&[keystroke], &settings);
            assert_eq!(bindings.len(), 1);
            assert!(bindings[0].action().as_any().is::<OpenSettings>());
        }
        for (key, next) in [("ctrl-tab", true), ("ctrl-shift-tab", false)] {
            let (bindings, _) = keymap.bindings_for_input(&[Keystroke::parse(key).unwrap()], &terminal);
            assert_eq!(bindings.len(), 1);
            if next {
                assert!(bindings[0].action().as_any().is::<super::NextSession>());
            } else {
                assert!(bindings[0].action().as_any().is::<super::PrevSession>());
            }
        }
    }

    #[test]
    fn up_and_down_in_quick_connect_move_its_selection() {
        use gpui::{KeyBinding, KeyContext, Keymap, Keystroke};
        use super::{OpenSettings, PickerDown, PickerUp};

        // Stands in for the field's own Up and Down, registered before Komando's bindings.
        let mut keymap = Keymap::new(vec![
            KeyBinding::new("up", OpenSettings, Some("Input")),
            KeyBinding::new("down", OpenSettings, Some("Input")),
        ]);
        keymap.add_bindings(super::key_bindings());
        let picker = [
            KeyContext::parse("Workspace").unwrap(),
            KeyContext::parse(crate::ui::workspace::PICKER_CONTEXT).unwrap(),
            KeyContext::parse("Input").unwrap(),
        ];
        let (bindings, _) = keymap.bindings_for_input(&[Keystroke::parse("up").unwrap()], &picker);
        assert!(bindings[0].action().as_any().is::<PickerUp>());
        let (bindings, _) = keymap.bindings_for_input(&[Keystroke::parse("down").unwrap()], &picker);
        assert!(bindings[0].action().as_any().is::<PickerDown>());

        // Other fields keep theirs.
        let form = [KeyContext::parse("Workspace").unwrap(), KeyContext::parse("Input").unwrap()];
        let (bindings, _) = keymap.bindings_for_input(&[Keystroke::parse("up").unwrap()], &form);
        assert!(bindings[0].action().as_any().is::<OpenSettings>());
    }

    #[test]
    fn the_logo_is_served_alongside_the_icons() {
        let logo = super::Assets.load(super::LOGO_PNG).unwrap().expect("logo");
        let image = image::load_from_memory(&logo).expect("bundled logo must decode");
        assert_eq!((image.width(), image.height()), (1500, 344));
        assert!(super::Assets.load("icons/server.svg").unwrap().is_some());
        assert!(super::Assets.load("icons/eraser.svg").unwrap().is_some(), "the Unhighlight menu entry");
    }
}
