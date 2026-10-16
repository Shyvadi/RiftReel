fn main() {
    // Listing the app's commands makes each one need an explicit grant in
    // `capabilities/` (an allowlist), instead of being callable from any window.
    let manifest = tauri_build::AppManifest::new().commands(&[
        "get_status",
        "list_matches",
        "match_filters",
        "request_thumbnails",
        "get_match",
        "save_clip",
        "set_clip_favorite",
        "reveal_clip",
        "delete_clip",
        "copy_clip",
        "export_clip",
        "set_match_favorite",
        "reveal_video",
        "storage_overview",
        "set_cleanup",
        "run_cleanup",
        "retry_cleanup",
        "outplayed_survey",
        "import_outplayed",
        "set_import_cleanup",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("tauri-build failed");
}
