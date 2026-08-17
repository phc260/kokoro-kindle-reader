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

    // kokoro-host and native-deps are both direct children of the repo root.
    let tp = manifest.parent().unwrap().join("native-deps");
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
                "kokoro-host: missing {} — run native-deps/fetch-deps.ps1 \
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

    // Stage the x86 hook + injector into `resources\` beside the exe, mirroring the layout the
    // installer produces.
    //
    // Without this a RELEASE build run from the dev tree cannot inject at all:
    // `kindle_watch::resource_path` looks in `resources\`, and its dev-tree fallback is
    // `#[cfg(debug_assertions)]`, so in release it falls through to a bare relative filename
    // that never resolves. The failure is near-silent — Kindle 18632 ignores `DefaultTokenId`,
    // so it simply narrates in the WinRT default voice, which reads as "Kokoro broke" rather
    // than "the injector was not found".
    //
    // Best-effort: the x86 crates build to their own target dirs and may not have been built
    // yet, and a missing injector must not fail the host's build (the pipe and synth are
    // useful without it). Build them with:
    //   cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-hook/Cargo.toml
    //   cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-inject/Cargo.toml
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    let resources = profile_dir.join("resources");
    for (crate_dir, file) in [
        ("kokoro-inject", "kokoro-inject.exe"),
        ("kokoro-hook", "kokoro_hook.dll"),
    ] {
        let src = root
            .join(crate_dir)
            .join("target")
            .join("i686-pc-windows-msvc")
            .join("release")
            .join(file);
        if src.exists() {
            let _ = std::fs::create_dir_all(&resources);
            let _ = std::fs::copy(&src, resources.join(file));
        }
        println!("cargo:rerun-if-changed={}", src.display());
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
    // Windows version resources are four-part (MAJOR.MINOR.BUILD.REVISION); take the
    // crate's semver from Cargo and pin the unused revision to 0, so the version lives
    // in Cargo.toml alone (no hard-coded copy to keep in sync).
    let version = format!("{}.0", env::var("CARGO_PKG_VERSION").unwrap());
    res.set("FileDescription", description);
    res.set("ProductName", "Kokoro Kindle Reader");
    res.set("FileVersion", &version);
    res.set("ProductVersion", &version);
    // This exe links espeak-ng (GPL-3.0-or-later), so the binary is conveyed under GPLv3.
    // Don't put a bare license name here: the source itself is not uniformly one license
    // (most of it is MIT, but text.rs/espeak.rs/native_synth.rs port Apache-2.0 code - see
    // THIRD_PARTY_NOTICES.md), so a single-word claim in what Windows shows in the file's
    // Properties would misstate it either way. State the copyright holder and point at the
    // notice instead of asserting a license here.
    res.set("LegalCopyright", "Copyright (c) 2026 Alan P.H. Chiu; binary conveyed under GPLv3 - see THIRD_PARTY_NOTICES.md");
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
