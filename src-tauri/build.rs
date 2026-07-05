use std::env;
use std::path::PathBuf;

fn main() {
    // When the `native-synth` feature is on, compile the kokoro-worker C++ synth
    // core (KokoroSynth WebGPU + espeak) into this app and link the prebuilt ORT +
    // espeak import libs, so pipe_server.rs can synthesize natively instead of in
    // the webview. Runtime DLLs + espeak-ng-data are staged next to the dev exe.
    if env::var("CARGO_FEATURE_NATIVE_SYNTH").is_ok() {
        build_native_synth();
    }
    tauri_build::build();
}

fn build_native_synth() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let worker = manifest.parent().unwrap().join("kokoro-worker"); // ../kokoro-worker
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
                "native-synth: missing {} — run kokoro-worker/tools/fetch-deps.ps1 \
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

    // Stage runtime DLLs + espeak-ng-data next to the (dev) exe so the app finds
    // the Dawn onnxruntime.dll / espeak-ng.dll and the phoneme data at runtime.
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
