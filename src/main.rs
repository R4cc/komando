// No console window behind the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    komando::perf::startup("main");
    let data_dir = komando::model::data_dir();
    let startup = komando::ui::workspace::Startup {
        // Set when another Komando asked for this one to run a shell as administrator.
        open_shell: komando::elevate::shell_from_args(std::env::args().skip(1)),
        ..Default::default()
    };
    #[cfg(windows)]
    let _instance = if startup.open_shell.is_some() && komando::elevate::is_elevated() {
        None // Deliberately separate administrator shell window, with no tray icon.
    } else {
        komando::platform::instance::wait_for_restart(std::env::args().skip(1));
        match komando::platform::instance::Instance::claim(&data_dir) {
            Ok(Some(instance)) => Some(instance),
            Ok(None) => return,
            Err(error) => {
                use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
                let message: Vec<u16> = format!("Could not open Komando: {error}").encode_utf16().chain([0]).collect();
                unsafe {
                    MessageBoxW(None, windows::core::PCWSTR(message.as_ptr()), windows::core::w!("Komando"), MB_OK | MB_ICONERROR);
                }
                return;
            }
        }
    };
    komando::run(komando::Options { data_dir, startup });
}
