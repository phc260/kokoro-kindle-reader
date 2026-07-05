// Headless synthesis host for Kokoro Kindle Reader — the SAPI pipe + native Dawn
// WebGPU synth with NO WebView2. This is M1: run the pipe server from a plain tokio
// runtime and block. Tray, autostart, and the egui settings panel come next.
//
// Reuses, verbatim, the exact code the Tauri app's `native-synth` build proved:
//   - native_synth.rs (serialized C++ WebGPU worker + controls.json reader)
//   - split_text.rs   (the sentence-chunk splitter)
// via #[path] include so there is one source of truth. The C++ core is compiled and
// its runtime DLLs staged by build.rs.

use std::path::PathBuf;

// Shared, Tauri-free modules pulled from the Tauri crate's src/ (single source of
// truth). native_synth's `extern "C"` symbols resolve to the C++ compiled in build.rs.
#[path = "../../src-tauri/src/native_synth.rs"]
mod native_synth;
#[path = "../../src-tauri/src/split_text.rs"]
mod split_text;

mod pipe;

// Must match src-tauri/tauri.conf.json `identifier` — Tauri stores the model +
// controls.json under %APPDATA%\<identifier> on Windows, and we read the same dir.
const APP_IDENTIFIER: &str = "com.phc260.kokoro-kindle-reader";
// The pinned model's repo id (from src-tauri/model-manifest.json); the model files
// live under <app_data>/<MODEL_ID>/. Embedded so we don't parse the manifest at
// runtime just for this one string.
const MODEL_ID: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";

/// Tauri's app_data_dir on Windows: %APPDATA% (Roaming) \ <identifier>.
fn app_data_dir() -> PathBuf {
    let roaming = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default();
    roaming.join(APP_IDENTIFIER)
}

/// espeak-ng-data staged next to this exe by build.rs.
fn espeak_data_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default()
        .join("espeak-ng-data")
}

fn main() {
    let app_data = app_data_dir();
    let base = app_data.join(MODEL_ID);
    let espeak = espeak_data_dir();

    eprintln!("[host] app_data = {}", app_data.display());
    eprintln!("[host] model base = {}", base.display());
    eprintln!("[host] espeak-ng-data = {}", espeak.display());
    if !base.join("onnx").join("model.onnx").exists() {
        eprintln!("[host] WARNING: model.onnx not found under the model base — synthesis will fail until the model is downloaded.");
    }

    // Serialized native WebGPU synth worker (lazily inits the ORT/WebGPU session on
    // the first request). Cloneable handle shared into the pipe tasks.
    let native = native_synth::NativeSynth::spawn(base, espeak);
    let ctx = pipe::Ctx { app_data, native };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    eprintln!("[host] serving \\\\.\\pipe\\KokoroSapiSynth — Ctrl+C to quit");
    rt.block_on(async move {
        if let Err(e) = pipe::serve_loop(ctx).await {
            eprintln!("[host] pipe server stopped: {e}");
        }
    });
}
