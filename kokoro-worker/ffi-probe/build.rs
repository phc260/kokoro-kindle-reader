use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.parent().unwrap().to_path_buf(); // kokoro-worker/
    let tp = root.join("third_party");
    let src = root.join("src");
    let ort_inc = tp.join("onnxruntime").join("include");
    let ort_lib = tp.join("onnxruntime").join("lib");
    let espk_inc = tp.join("espeak-ng-src").join("src").join("include");
    let espk_lib = tp
        .join("espeak-ng-src")
        .join("build-x64")
        .join("src")
        .join("libespeak-ng");

    // Compile the C++ synth core + C ABI wrapper into a static lib Rust links.
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

    // Link the prebuilt import libs (ORT 1.27 + x64 espeak). The WebGPU EP is
    // selected at runtime; the wheel's Dawn onnxruntime.dll is staged below.
    println!("cargo:rustc-link-search=native={}", ort_lib.display());
    println!("cargo:rustc-link-search=native={}", espk_lib.display());
    println!("cargo:rustc-link-lib=onnxruntime");
    println!("cargo:rustc-link-lib=espeak-ng");

    // Stage the runtime DLLs next to the exe (target/<profile>/) so `cargo run`
    // finds the WebGPU Dawn onnxruntime.dll + espeak-ng.dll.
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let profile_dir = out.ancestors().nth(3).unwrap().to_path_buf(); // .../target/<profile>
    let runtime = tp.join("runtime");
    for dll in [
        "onnxruntime.dll",
        "onnxruntime_providers_shared.dll",
        "dxcompiler.dll",
        "dxil.dll",
        "espeak-ng.dll",
    ] {
        let _ = std::fs::copy(runtime.join(dll), profile_dir.join(dll));
    }

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
