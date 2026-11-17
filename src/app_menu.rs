//! macOS: the menu bar and the Cmd shortcuts a Mac user expects.
//!
//! Komando's own shortcuts use Ctrl+Shift on every system so plain Ctrl keys reach the shell;
//! on a Mac, Cmd is free for the app, so the common Cmd shortcuts are bound as well. Plain
//! Cmd+letter keys never reach the shell anyway (macOS sends no character for them).

use gpui::{App, KeyBinding, Menu, MenuItem, SystemMenuType, actions};

use crate::ui::terminal_view::{self, Copy, Find, Paste};
use crate::ui::workspace::{CloseSession, NewSession, OpenSettings, ShowAbout};

actions!(komando_mac, [Quit, Hide, HideOthers, ShowAll, Minimize, Zoom]);

pub fn install(cx: &mut App) {
    // Closing Komando's window ends it, as on Windows and Linux. Its desktop windows close
    // with it (`Workspace::close_own_windows`), so this is the last window. Without it the
    // app would stay in the Dock with sessions still connected and no window to show them.
    cx.set_quit_mode(gpui::QuitMode::LastWindowClosed);

    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.minimize_window());
        }
    });
    cx.on_action(|_: &Zoom, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.zoom_window());
        }
    });

    let t = Some(terminal_view::CONTEXT);
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-t", NewSession, None),
        KeyBinding::new("cmd-w", CloseSession, None),
        KeyBinding::new("cmd-c", Copy, t),
        KeyBinding::new("cmd-v", Paste, t),
        KeyBinding::new("cmd-f", Find, t),
    ]);

    // The menu shows each item's shortcut from the bindings above.
    cx.set_menus([
        Menu::new("Komando").items([
            MenuItem::action("About Komando", ShowAbout),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Komando", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Komando", Quit),
        ]),
        Menu::new("Session").items([
            MenuItem::action("New Session", NewSession),
            MenuItem::action("Close Session", CloseSession),
        ]),
        Menu::new("Window").items([MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", Zoom)]),
    ]);
}
