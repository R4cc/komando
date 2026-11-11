// No console window behind the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    komando::run(komando::Options {
        data_dir: komando::model::data_dir(),
        skip_unlock: args.iter().any(|a| a == "--locked"),
        startup: Default::default(),
    });
}
