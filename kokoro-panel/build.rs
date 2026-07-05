// Compile the Slint UI (ui/panel.slint) into Rust, styled with the Fluent theme for
// a modern Windows look. `slint::include_modules!()` in main.rs pulls in the result.
// Also embeds a Windows version resource so the exe shows a friendly name + icon in
// Task Manager / Explorer.

use std::env;
use std::path::PathBuf;

fn main() {
    embed_version_info();

    let config = slint_build::CompilerConfiguration::new().with_style("fluent".to_string());
    slint_build::compile_with_config("ui/panel.slint", config).expect("compile panel.slint");
}

/// Embed FileDescription/ProductName/FileVersion + the app icon. No-op off Windows.
fn embed_version_info() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest
        .parent()
        .unwrap()
        .join("icons")
        .join("icon.ico");
    let mut res = winresource::WindowsResource::new();
    if icon.exists() {
        res.set_icon(icon.to_str().unwrap());
    }
    res.set("FileDescription", "Kokoro Kindle Reader Settings");
    res.set("ProductName", "Kokoro Kindle Reader");
    res.set("FileVersion", "0.2.0.0");
    res.set("ProductVersion", "0.2.0.0");
    res.set("LegalCopyright", "Apache-2.0 licensed");
    if let Err(e) = res.compile() {
        println!("cargo:warning=winresource (panel): {e}");
    }
}
