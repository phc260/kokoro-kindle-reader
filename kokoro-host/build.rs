// Link the prebuilt espeak-ng import lib (for the espeak.rs FFI) and stage the Dawn
// ORT + espeak runtime DLLs + espeak-ng-data next to the host exe. The ONNX model runs
// on the `ort` crate's WebGPU EP via load-dynamic, so onnxruntime.dll is loaded at
// runtime (not linked) — no C++ compile anymore.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    // Give the exe a friendly name + icon in Task Manager / Explorer.
    embed_version_info(&manifest, "Kokoro Kindle Reader");

    // kokoro-host and kokoro-worker are both direct children of the repo root.
    let worker = manifest.parent().unwrap().join("kokoro-worker");
    let tp = worker.join("third_party");
    let espk_lib = tp
        .join("espeak-ng-src")
        .join("build-x64")
        .join("src")
        .join("libespeak-ng");
    let runtime = tp.join("runtime");
    let espk_data = tp
        .join("espeak-ng-src")
        .join("build-x64")
        .join("espeak-ng-data");

    for p in [&espk_lib, &runtime, &espk_data] {
        if !p.exists() {
            panic!(
                "kokoro-host: missing {} — run kokoro-worker/tools/fetch-deps.ps1 \
                 (which downloads the ORT/Dawn runtime and builds the espeak artifacts) first",
                p.display()
            );
        }
    }

    // espeak.rs's FFI needs the espeak-ng import lib; ort loads onnxruntime.dll itself.
    println!("cargo:rustc-link-search=native={}", espk_lib.display());
    println!("cargo:rustc-link-lib=espeak-ng");

    // Stage runtime DLLs + espeak-ng-data next to the host exe so ort finds the Dawn
    // onnxruntime.dll (+ its dxcompiler/dxil/providers_shared) and espeak the phoneme
    // data at runtime.
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let profile_dir = out.ancestors().nth(3).unwrap().to_path_buf(); // .../target/<profile>
    for dll in [
        "onnxruntime.dll",
        "onnxruntime_providers_shared.dll",
        "dxcompiler.dll",
        "dxil.dll",
        "espeak-ng.dll",
    ] {
        let _ = std::fs::copy(runtime.join(dll), profile_dir.join(dll));
    }
    copy_dir(&espk_data, &profile_dir.join("espeak-ng-data"));
}

/// Embed a Windows version resource (FileDescription/ProductName/FileVersion +
/// the app icon) so the exe isn't just a bare filename in Task Manager / Explorer.
/// No-op off Windows. The icon is the shared app icon under the repo's icons/.
fn embed_version_info(manifest: &Path, description: &str) {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = manifest
        .parent()
        .unwrap()
        .join("icons")
        .join("icon.ico");
    let mut res = winresource::WindowsResource::new();
    if icon.exists() {
        res.set_icon(icon.to_str().unwrap());
    }
    res.set("FileDescription", description);
    res.set("ProductName", "Kokoro Kindle Reader");
    res.set("FileVersion", "0.2.1.0");
    res.set("ProductVersion", "0.2.1.0");
    res.set("LegalCopyright", "MIT licensed");
    if let Err(e) = res.compile() {
        println!("cargo:warning=winresource (host): {e}");
    }
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    let _ = std::fs::create_dir_all(to);
    let Ok(entries) = std::fs::read_dir(from) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let dst = to.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &dst);
        } else {
            let _ = std::fs::copy(&p, &dst);
        }
    }
}
