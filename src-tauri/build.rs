fn main() {
    // Listing the app's commands makes each one need an explicit grant in
    // `capabilities/` (an allowlist), instead of being callable from any window.
    let manifest = tauri_build::AppManifest::new().commands(&["get_status"]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("tauri-build failed");
}
