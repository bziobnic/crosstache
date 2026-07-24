fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "startup_status",
            "preview_setup",
            "apply_setup",
            "retry_startup",
            "open_config",
            "copy_diagnostics",
        ]),
    ))
    .expect("failed to build Tauri application");
}
