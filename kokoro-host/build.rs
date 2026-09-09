// Link the prebuilt espeak-ng library (for the espeak.rs FFI) and stage the ORT + espeak
// runtime libraries + espeak-ng-data next to the host exe. The ONNX model runs on the `ort`
// crate's execution providers via load-dynamic, so the runtime library is loaded at run time
// (not linked) — no C++ compile anymore.
//
// **Branch on the TARGET, not on `cfg!(windows)`.** A build script is compiled for the host,
// so `#[cfg(windows)]` here answers "what am I running on", and the only question that
// matters is what is being built. `CARGO_CFG_TARGET_OS` is that question. The one exception
// is `winresource` itself, which is a host-side build dependency and is absent when the host
// is not Windows (see Cargo.toml) — so that one *is* gated on the host.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Give the exe a friendly name + icon in Task Manager / Explorer.
    embed_version_info(&manifest, "Kokoro Kindle Reader");

    // kokoro-host and native-deps are both direct children of the repo root.
    let tp = manifest.parent().unwrap().join("native-deps");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let profile_dir = out.ancestors().nth(3).unwrap().to_path_buf(); // .../target/<profile>

    match target_os.as_str() {
        "windows" => windows_deps(&tp, &profile_dir),
        "linux" => linux_deps(&tp, &profile_dir),
        other => panic!(
            "kokoro-host: no native-dependency recipe for target OS '{other}'. \
             Windows and Linux are provisioned by native-deps/fetch-deps.ps1 and \
             native-deps/fetch-deps.sh respectively."
        ),
    }
}

/// Windows: the Dawn ORT runtime + the espeak import lib, and the x86 hook/injector staged
/// into `resources\` beside the exe.
fn windows_deps(tp: &Path, profile_dir: &Path) {
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

/// Linux: the CPU ORT runtime + the espeak shared library, staged beside the exe and found
/// there at run time via an `$ORIGIN` rpath.
///
/// There is no hook, no injector and no `resources\` here: those are Kindle for PC's, and
/// the Linux host's only client is the browser extension over the loopback endpoint.
fn linux_deps(tp: &Path, profile_dir: &Path) {
    let runtime = tp.join("linux").join("runtime");
    let espk_data = runtime.join("espeak-ng-data");

    // Everything comes from the ONE provisioned tree, unlike the Windows branch, which also
    // reaches into espeak's CMake build directory. That directory's internal layout has moved
    // between espeak-ng releases; `fetch-deps.sh` resolves it once, by searching, and stages
    // the result here beside its provision marker. Reading the marked tree is also what the
    // packaging rule asks for — the provisioned files, not whatever a build left lying about.
    for p in [&runtime, &espk_data] {
        if !p.exists() {
            panic!(
                "kokoro-host: missing {} — run native-deps/fetch-deps.sh                  (which downloads the ORT runtime and builds the espeak artifacts) first",
                p.display()
            );
        }
    }
    if !runtime.join("libespeak-ng.so").exists() {
        panic!(
            "kokoro-host: no libespeak-ng.so in {} — run native-deps/fetch-deps.sh first",
            runtime.display()
        );
    }

    // espeak.rs's FFI links espeak-ng; ort loads libonnxruntime.so itself.
    println!("cargo:rustc-link-search=native={}", runtime.display());
    println!("cargo:rustc-link-lib=espeak-ng");
    // Find the staged copy beside the exe at run time. Without this the loader would only
    // look in the system paths, where these deliberately are not installed: the espeak here
    // is a *modified* build pinned for phoneme parity, and a distribution's own libespeak-ng
    // is not a substitute for it. `$ORIGIN` is expanded by the loader, not the shell, so it
    // survives being run from anywhere and is what a .deb's private lib dir will rely on too.
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");

    // Stage the runtime beside the exe, as the Windows branch does. The versioned names
    // matter as much as the bare one: the loader resolves these by their SONAME.
    stage_matching(&runtime, profile_dir, "libonnxruntime.so");
    stage_matching(&runtime, profile_dir, "libespeak-ng.so");
    copy_dir(&espk_data, &profile_dir.join("espeak-ng-data"));
}

/// Copy every file in `from` whose name starts with `prefix` into `to` — `libfoo.so`,
/// `libfoo.so.1`, `libfoo.so.1.2.3` and whatever else the build produced. Copies rather than
/// re-links the symlinks, so the staged tree stands alone.
fn stage_matching(from: &Path, to: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(from) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        if name.to_string_lossy().starts_with(prefix) {
            let _ = std::fs::copy(e.path(), to.join(&name));
        }
    }
}

/// Embed a Windows version resource (FileDescription/ProductName/FileVersion +
/// the app icon) so the exe isn't just a bare filename in Task Manager / Explorer.
/// No-op off Windows. The icon is the shared app icon under the repo's icons/.
///
/// Host-gated, not target-gated: `winresource` is only a build dependency when the host is
/// Windows. The target check inside covers the cross case (Windows host, Linux target),
/// where the crate is present but must not be used.
#[cfg(windows)]
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

/// No version resource to embed when building on a non-Windows host — and no `winresource`
/// in the dependency graph to embed it with.
#[cfg(not(windows))]
fn embed_version_info(_manifest: &Path, _description: &str) {}

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
