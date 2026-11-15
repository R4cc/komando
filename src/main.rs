// No console window behind the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    komando::perf::startup("main");
    komando::run(komando::Options {
        data_dir: komando::model::data_dir(),
        startup: komando::ui::workspace::Startup {
            // Set when another Komando asked for this one to run a shell as administrator.
            open_shell: komando::elevate::shell_from_args(std::env::args().skip(1)),
            ..Default::default()
        },
    });
}
