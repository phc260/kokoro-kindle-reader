// Compile the kokoro-worker C++ synth core (KokoroSynth WebGPU + espeak) into the
// headless host and link the prebuilt ORT + espeak import libs, so native_synth.rs
// can synthesize natively. Runtime DLLs + espeak-ng-data are staged next to the
// host exe.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    // Give the exe a friendly name + icon in Task Manager / Explorer.
    embed_version_info(&manifest, "Kokoro Kindle Reader");

    // kokoro-host and kokoro-worker are both direct children of the repo root.
    let worker = manifest.parent().unwrap().join("kokoro-worker");
    let src = worker.join("src");
    let tp = worker.join("third_party");
    let ort_inc = tp.join("onnxruntime").join("include");
    let ort_lib = tp.join("onnxruntime").join("lib");
    let espk_inc = tp.join("espeak-ng-src").join("src").join("include");
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

    for p in [&ort_inc, &ort_lib, &espk_inc, &espk_lib, &runtime, &espk_data] {
        if !p.exists() {
            panic!(
                "kokoro-host: missing {} — run kokoro-worker/tools/fetch-deps.ps1 \
                 (which downloads the ORT/Dawn runtime and builds the espeak artifacts) first",
                p.display()
            );
        }
    }

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .include(&ort_inc)
        .include(&espk_inc)
        .include(&src)
        .file(src.join("KokoroText.cpp"))
        .file(src.join("KokoroSynth.cpp"))
        .file(src.join("kokoro_ffi.cpp"))
        .compile("kokoro_synth");

    println!("cargo:rustc-link-search=native={}", ort_lib.display());
    println!("cargo:rustc-link-search=native={}", espk_lib.display());
    println!("cargo:rustc-link-lib=onnxruntime");
    println!("cargo:rustc-link-lib=espeak-ng");

    // Stage runtime DLLs + espeak-ng-data next to the host exe so it finds the
    // Dawn onnxruntime.dll / espeak-ng.dll and the phoneme data at runtime.
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

    for f in [
        "KokoroText.cpp",
        "KokoroSynth.cpp",
        "kokoro_ffi.cpp",
        "KokoroSynth.h",
        "KokoroText.h",
        "kokoro_ffi.h",
    ] {
        println!("cargo:rerun-if-changed={}", src.join(f).display());
    }
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
