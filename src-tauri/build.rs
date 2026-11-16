fn main() {
    // Tauri embeds Info.plist in the dev binary but doesn't watch it.
    println!("cargo:rerun-if-changed=Info.plist");
    // The passkey plugin's one command, opened to web pages in the in-app
    // browser by capabilities/browser.json (see browser_logins.rs).
    tauri_build::try_build(tauri_build::Attributes::new().plugin(
        "passkey",
        tauri_build::InlinedPlugin::new().commands(&["request"]),
    ))
    .expect("failed to run tauri-build");
}
