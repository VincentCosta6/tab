// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // A chat's `gh` or `git` shim: hand the command to the running app and
    // exit with its code, without starting a window.
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|a| a == "__tab_cli") {
        std::process::exit(tab_core::sandbox::broker::client_main(args.collect()));
    }
    // Opened from Finder or the Dock, the app has the system's minimal PATH:
    // take the login shell's so `claude`, `gh` and `npm` are found.
    tab_core::shell_path::import();
    tab_lib::run()
}
