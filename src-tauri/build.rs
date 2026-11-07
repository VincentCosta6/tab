fn main() {
    // Tauri embeds Info.plist in the dev binary but doesn't watch it.
    println!("cargo:rerun-if-changed=Info.plist");
    tauri_build::build()
}
