//! KOMANDO: a fast terminal for system engineers who work on Linux machines over SSH.
//! Built for Windows 11 first; Linux and macOS builds ship too (see
//! docs/adr/0014-linux-and-macos-builds.md).

#[cfg(target_os = "macos")]
mod app_menu;
pub mod desktop;
pub mod elevate;
pub mod export;
pub mod favorites;
#[cfg(test)]
mod fuzz;
pub mod highlight;
pub mod history;
pub mod icons;
pub mod import;
pub mod keys;
pub mod links;
pub mod logging;
pub mod marks;
pub mod memory;
pub mod model;
pub mod perf;
pub mod platform;
pub mod search;
pub mod session;
pub mod sftp;
pub mod share;
pub mod theme;
pub mod ui;
pub mod update;
pub mod usage;
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
    PrevSession, QuickConnect, ToggleSftp, TogglePresentation, ToggleSidebar, Workspace,
    nav::{
        self, FocusSidebar, NavCollapse, NavDelete, NavDeleteHost, NavDown, NavEdit, NavEditHost, NavExpand, NavFirst, NavLast,
        NavLeave, NavMenu, NavOpen, NavOpenAnother, NavUp,
    },
};

pub const FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/CascadiaMono-Regular.ttf"),
    include_bytes!("../assets/fonts/CascadiaMono-Bold.ttf"),
    include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Geist-Bold.ttf"),
    include_bytes!("../assets/fonts/themes/dmsans/DMSans[opsz,wght].ttf"),
    include_bytes!("../assets/fonts/themes/firacode/FiraCode[wght].ttf"),
    include_bytes!("../assets/fonts/themes/geistmono/GeistMono[wght].ttf"),
    include_bytes!("../assets/fonts/themes/ibmplexmono/IBMPlexMono-Bold.ttf"),
    include_bytes!("../assets/fonts/themes/ibmplexmono/IBMPlexMono-Medium.ttf"),
    include_bytes!("../assets/fonts/themes/ibmplexmono/IBMPlexMono-Regular.ttf"),
    include_bytes!("../assets/fonts/themes/ibmplexmono/IBMPlexMono-SemiBold.ttf"),
    include_bytes!("../assets/fonts/themes/inter/Inter[opsz,wght].ttf"),
    include_bytes!("../assets/fonts/themes/jetbrainsmono/JetBrainsMono[wght].ttf"),
    include_bytes!("../assets/fonts/themes/outfit/Outfit[wght].ttf"),
    include_bytes!("../assets/fonts/themes/plusjakartasans/PlusJakartaSans[wght].ttf"),
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
    #[cfg(target_os = "macos")]
    app_menu::install(cx);
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
        KeyBinding::new("enter", terminal_view::ConfirmPaste, Some(terminal_view::PASTE_CONFIRM_CONTEXT)),
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
        KeyBinding::new("ctrl-shift-p", TogglePresentation, None),
        KeyBinding::new("ctrl-shift-h", FocusSidebar, Some("Workspace")),
        KeyBinding::new("up", NavUp, Some(nav::CONTEXT)),
        KeyBinding::new("down", NavDown, Some(nav::CONTEXT)),
        KeyBinding::new("home", NavFirst, Some(nav::CONTEXT)),
        KeyBinding::new("end", NavLast, Some(nav::CONTEXT)),
        KeyBinding::new("right", NavExpand, Some(nav::CONTEXT)),
        KeyBinding::new("left", NavCollapse, Some(nav::CONTEXT)),
        KeyBinding::new("enter", NavOpen, Some(nav::CONTEXT)),
        KeyBinding::new("ctrl-enter", NavOpenAnother, Some(nav::CONTEXT)),
        KeyBinding::new("f2", NavEdit, Some(nav::CONTEXT)),
        KeyBinding::new("shift-f2", NavEditHost, Some(nav::CONTEXT)),
        KeyBinding::new("delete", NavDelete, Some(nav::CONTEXT)),
        KeyBinding::new("shift-delete", NavDeleteHost, Some(nav::CONTEXT)),
        KeyBinding::new("shift-f10", NavMenu, Some(nav::CONTEXT)),
        KeyBinding::new("menu", NavMenu, Some(nav::CONTEXT)),
        KeyBinding::new("escape", NavLeave, Some(nav::CONTEXT)),
    ]
}

/// The borderless Komando wordmark, drawn in the current theme's text color.
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
    #[cfg(windows)]
    platform::crash::install(&options.data_dir);
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
                        traffic_light_position: ui::workspace::TRAFFIC_LIGHT_POSITION,
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
            // Every way out leaves a line, so a window that vanishes can be told apart from a
            // crash (which `platform::crash` logs) or from being killed (which logs nothing).
            cx.on_window_closed(|cx, id| log::info!("window {id:?} closed, {} still open", cx.windows().len())).detach();
            cx.on_app_quit(|_| {
                log::info!("Komando quitting");
                log::logger().flush();
                async {}
            })
            .detach();
        });
    log::info!("Komando exited");
    log::logger().flush();
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;

    #[test]
    fn enter_confirms_paste_only_in_the_paste_confirmation() {
        use gpui::{KeyContext, Keymap, Keystroke};
        use crate::ui::terminal_view::{ConfirmPaste, PASTE_CONFIRM_CONTEXT};
        let keymap = Keymap::new(super::key_bindings());
        let confirmation = [KeyContext::parse("Workspace").unwrap(), KeyContext::parse(&format!("Terminal {PASTE_CONFIRM_CONTEXT}")).unwrap()];
        let enter = Keystroke::parse("enter").unwrap();
        let (bindings, _) = keymap.bindings_for_input(&[enter.clone()], &confirmation);
        assert!(bindings.first().is_some_and(|b| b.action().as_any().is::<ConfirmPaste>()));
        for context in ["Terminal", "Input", "TerminalSearch Input", "Sidebar"] {
            let path = [KeyContext::parse("Workspace").unwrap(), KeyContext::parse(context).unwrap()];
            let (bindings, _) = keymap.bindings_for_input(&[enter.clone()], &path);
            assert!(bindings.iter().all(|b| !b.action().as_any().is::<ConfirmPaste>()), "{context}");
        }
    }

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
    fn the_sidebar_keys_work_only_in_the_sidebar() {
        use gpui::{KeyContext, Keymap, Keystroke};
        use super::{FocusSidebar, NavDelete, NavEdit, NavLeave, NavOpen, NavOpenAnother, NavUp};

        let keymap = Keymap::new(super::key_bindings());
        let sidebar = [KeyContext::parse("Workspace").unwrap(), KeyContext::parse(super::nav::CONTEXT).unwrap()];
        fn first(keymap: &Keymap, key: &'static str, context: &[KeyContext]) -> Option<Box<dyn gpui::Action>> {
            let (bindings, _) = keymap.bindings_for_input(&[Keystroke::parse(key).unwrap()], context);
            bindings.first().map(|b| b.action().boxed_clone())
        }
        let action = |key: &'static str, context: &[KeyContext]| first(&keymap, key, context);
        let is = |key: &'static str, context: &[KeyContext], check: fn(&dyn gpui::Action) -> bool| {
            action(key, context).is_some_and(|a| check(a.as_ref()))
        };
        assert!(is("up", &sidebar, |a| a.as_any().is::<NavUp>()));
        assert!(is("enter", &sidebar, |a| a.as_any().is::<NavOpen>()));
        assert!(is("ctrl-enter", &sidebar, |a| a.as_any().is::<NavOpenAnother>()));
        assert!(is("f2", &sidebar, |a| a.as_any().is::<NavEdit>()));
        assert!(is("delete", &sidebar, |a| a.as_any().is::<NavDelete>()));
        assert!(is("escape", &sidebar, |a| a.as_any().is::<NavLeave>()));
        assert!(is("shift-f2", &sidebar, |a| a.as_any().is::<super::NavEditHost>()));
        assert!(is("shift-delete", &sidebar, |a| a.as_any().is::<super::NavDeleteHost>()));
        assert!(is("shift-f10", &sidebar, |a| a.as_any().is::<super::NavMenu>()));

        // In a terminal, the same keys stay the shell's, and Ctrl+Shift+H reaches the sidebar.
        let terminal = [KeyContext::parse("Workspace").unwrap(), KeyContext::parse("Terminal").unwrap()];
        for key in ["up", "enter", "f2", "delete", "escape", "ctrl-enter"] {
            assert!(action(key, &terminal).is_none(), "{key} is bound in the terminal");
        }
        assert!(is("ctrl-shift-h", &terminal, |a| a.as_any().is::<FocusSidebar>()));
    }

    #[test]
    fn the_logo_is_served_alongside_the_icons() {
        let logo = super::Assets.load(super::LOGO_SVG).unwrap().expect("logo");
        assert!(std::str::from_utf8(&logo).unwrap().starts_with("<svg"));
        assert!(super::Assets.load("icons/server.svg").unwrap().is_some());
        assert!(super::Assets.load("icons/eraser.svg").unwrap().is_some(), "the Unhighlight menu entry");
        for icon in ["ellipsis", "text-cursor-input", "arrow-down-up", "list-tree", "folder-tree"] {
            assert!(super::Assets.load(&format!("icons/{icon}.svg")).unwrap().is_some(), "{icon}, used by the sidebar");
        }
        for icon in ["copy", "folder-input", "trash"] {
            assert!(super::Assets.load(&format!("icons/{icon}.svg")).unwrap().is_some(), "{icon}, used by Transfers");
        }
        for icon in ["key-round", "file-key", "eye", "eye-off", "copy", "trash", "lock"] {
            assert!(super::Assets.load(&format!("icons/{icon}.svg")).unwrap().is_some(), "{icon}, used by Settings → Vault");
        }
    }
}
