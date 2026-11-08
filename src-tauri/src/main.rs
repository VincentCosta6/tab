// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // A chat's `gh` or `git` shim: hand the command to the running app and
    // exit with its code, without starting a window.
    let mut args = std::env::args_os().skip(1);
    match args.next() {
        Some(a) if a == "__tab_cli" => {
            std::process::exit(tab_core::sandbox::broker::client_main(args.collect()));
        }
        // Inside a Linux sandbox with a domain list: the way to tab's proxy.
        Some(a) if a == tab_core::sandbox::bridge::SUBCOMMAND => {
            std::process::exit(tab_core::sandbox::bridge::main(args.collect()));
        }
        _ => {}
    }
    // Other processes of this user (agents included) can't read tab's
    // memory or environment through /proc.
    tab_core::sandbox::harden_process(false);
    if let Ok(exe) = std::env::current_exe() {
        tab_core::sandbox::bridge::set_helper(exe);
    }
    // Opened from Finder or the Dock, the app has the system's minimal PATH:
    // take the login shell's so `claude`, `gh` and `npm` are found.
    tab_core::shell_path::import();
    tab_lib::run()
}
