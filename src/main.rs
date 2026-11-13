// No console window behind the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    komando::perf::startup("main");
    komando::run(komando::Options {
        data_dir: komando::model::data_dir(),
        startup: Default::default(),
    });
}
