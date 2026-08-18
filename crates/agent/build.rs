fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest_dir = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"),
        );
        let icon = manifest_dir.join("../../apps/desktop/src-tauri/icons/icon.ico");
        winresource::WindowsResource::new()
            .set_icon(icon.to_str().expect("icon path is not valid UTF-8"))
            .compile()
            .expect("failed to embed the EasyDeployMesh icon");
    }
}
