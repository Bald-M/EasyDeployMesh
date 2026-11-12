/// Resource id of the brand mark drawn in the WinPE progress window. Keep in sync
/// with `winpe_progress::BRAND_MARK_RESOURCE_ID`.
const BRAND_MARK_RESOURCE_ID: &str = "2";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/brand-mark.ico");
    println!("cargo:rerun-if-changed=../../apps/desktop/src-tauri/icons/icon.ico");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest_dir = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"),
        );
        let icon = manifest_dir.join("../../apps/desktop/src-tauri/icons/icon.ico");
        let brand_mark = manifest_dir.join("assets/brand-mark.ico");
        winresource::WindowsResource::new()
            .set_icon(icon.to_str().expect("icon path is not valid UTF-8"))
            .set_icon_with_id(
                brand_mark
                    .to_str()
                    .expect("brand mark path is not valid UTF-8"),
                BRAND_MARK_RESOURCE_ID,
            )
            .compile()
            .expect("failed to embed the EasyDeployMesh icons");
    }
}
