fn main() {
    // tauri-build doesn't track the icon files: without this, a new icon.ico leaves the exe resource
    // and the tray icon (baked in by generate_context!) stale until a clean build.
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
