// Standalone benchmark: times one fixed paragraph through three engine configs —
// WebGPU EP + fp32 model.onnx (today's shipping path), CPU EP + fp32 model.onnx, and
// CPU EP + int8 model_quantized.onnx (already downloaded by every install but unused).
// Exploratory tool for deciding whether a CPU/quantized fallback is worth building for
// GPU-less or integrated-GPU-only laptops — not part of the shipping product, so it
// duplicates a little of native_synth.rs's model-run plumbing rather than exposing it.
// A standalone crate (not a kokoro-host bin) so it never compiles as part of a normal
// `cargo build`/`check` on the shipping tray daemon.
//
// Usage: cargo run --release [-- --model-dir <dir>] [--voice <id>]
// Defaults to the model dir the panel actually downloads into
// (%APPDATA%\com.phc260.kokoro-kindle-reader\onnx-community\Kokoro-82M-v1.0-ONNX) and
// voice af_heart, so a normal install needs no flags. Needs the runtime DLLs +
// espeak-ng-data staged next to the exe (build.rs does this).

#[path = "../../kokoro-host/src/text.rs"]
mod text;
#[path = "../../kokoro-host/src/espeak.rs"]
mod espeak;

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ort::ep::webgpu::WebGPU;
use ort::ep::CPU;
use ort::session::{builder::GraphOptimizationLevel, Session, SessionInputValue};
use ort::value::TensorRef;

const STYLE_DIM: usize = 256;
const VOICE_ROWS: usize = 510;
const MAX_CONTENT_TOKENS: usize = 500;
const APP_IDENTIFIER: &str = "com.phc260.kokoro-kindle-reader";
const MODEL_ID: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";

// A realistic Kindle-page-sized paragraph (2 sentences, ~50 words) so the timing
// reflects a real chunk rather than a trivially short phrase.
const BENCH_TEXT: &str = "The old lighthouse stood at the edge of the cliff, its \
    beam sweeping slowly across the dark water every ten seconds. Even after all \
    these years, the keeper still climbed the spiral stairs each evening to make \
    sure the lamp was burning bright enough to guide the fishing boats home.";

const TIMED_RUNS: u32 = 3;

fn main() {
    let mut model_dir: Option<PathBuf> = None;
    let mut voice = "af_heart".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--model-dir" => model_dir = args.next().map(PathBuf::from),
            "--voice" => voice = args.next().unwrap_or(voice),
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }
    let base = model_dir.unwrap_or_else(default_model_dir);
    let espeak_data = espeak_data_dir();

    println!("model dir   : {}", base.display());
    println!("espeak data : {}", espeak_data.display());
    println!("voice       : {voice}");
    println!();

    if let Err(e) = espeak::init(&espeak_data.to_string_lossy()) {
        eprintln!("espeak init failed: {e}");
        std::process::exit(1);
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    match ort::init_from(exe_dir.join("onnxruntime.dll")) {
        Ok(b) => {
            b.commit();
        }
        Err(e) => {
            eprintln!("ort init_from failed: {e}");
            std::process::exit(1);
        }
    }

    let tokenizer = base.join("tokenizer.json");
    let vocab = load_vocab(&tokenizer).unwrap_or_else(|| {
        eprintln!("failed to load tokenizer vocab from {}", tokenizer.display());
        std::process::exit(1);
    });
    let voice_data = load_voice(&base.join("voices").join(format!("{voice}.bin"))).unwrap_or_else(|| {
        eprintln!("failed to load voice {voice}.bin under {}", base.display());
        std::process::exit(1);
    });

    // Phonemization/tokenization is device-independent — do it once and reuse the
    // same token sequence for every config so we're only timing the model run.
    let phon = phonemize(BENCH_TEXT);
    let ids = tokenize(&phon, &vocab);
    let content = &ids[1..ids.len() - 1];
    println!("bench text  : {} chars, {} tokens\n", BENCH_TEXT.len(), content.len());

    struct Config {
        label: &'static str,
        model_file: &'static str,
        ep: fn() -> ort::ep::ExecutionProviderDispatch,
    }
    let configs = [
        Config { label: "WebGPU  fp32 (shipping)", model_file: "model.onnx", ep: || WebGPU::default().build() },
        Config { label: "CPU     fp32", model_file: "model.onnx", ep: || CPU::default().build() },
        Config { label: "CPU     int8 (quantized)", model_file: "model_quantized.onnx", ep: || CPU::default().build() },
    ];

    println!("{:<28} {:>12} {:>14} {:>16}", "config", "cold (ms)", "warm avg (ms)", "realtime factor");
    for cfg in &configs {
        let model_path = base.join("onnx").join(cfg.model_file);
        if !model_path.exists() {
            println!("{:<28} -- model file not found: {}", cfg.label, model_path.display());
            continue;
        }
        let mut session = match build_session(&model_path, (cfg.ep)()) {
            Ok(s) => s,
            Err(e) => {
                println!("{:<28} -- session build failed: {e}", cfg.label);
                continue;
            }
        };

        // First run per session pays shader-compile (WebGPU) / kernel-selection (CPU)
        // cost that later runs don't — report it separately as "cold" (first-page feel).
        let style = style_row(&voice_data, content.len());
        let cold_start = Instant::now();
        let cold_result = run_model(&mut session, &wrap(content), style, 1.0);
        let cold_ms = cold_start.elapsed().as_secs_f64() * 1000.0;
        let mut audio_secs = 0.0;
        if let Ok(pcm) = &cold_result {
            audio_secs = pcm.len() as f64 / 24_000.0;
        }
        if cold_result.is_err() {
            println!("{:<28} -- run failed: {}", cfg.label, cold_result.unwrap_err());
            continue;
        }

        let mut warm_total_ms = 0.0;
        for _ in 0..TIMED_RUNS {
            let t = Instant::now();
            let _ = run_model(&mut session, &wrap(content), style, 1.0);
            warm_total_ms += t.elapsed().as_secs_f64() * 1000.0;
        }
        let warm_avg_ms = warm_total_ms / TIMED_RUNS as f64;
        let realtime_factor = audio_secs / (warm_avg_ms / 1000.0);

        println!(
            "{:<28} {:>12.0} {:>14.0} {:>15.2}x",
            cfg.label, cold_ms, warm_avg_ms, realtime_factor
        );
    }
}

fn default_model_dir() -> PathBuf {
    let roaming = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    roaming.join(APP_IDENTIFIER).join(MODEL_ID)
}

fn espeak_data_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default()
        .join("espeak-ng-data")
}

fn load_vocab(tokenizer: &Path) -> Option<HashMap<Vec<u8>, i64>> {
    let txt = std::fs::read_to_string(tokenizer).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    let obj = v
        .get("model")
        .and_then(|m| m.get("vocab"))
        .or_else(|| v.get("vocab"))
        .and_then(|x| x.as_object())?;
    let mut vocab = HashMap::new();
    for (k, val) in obj {
        if let Some(id) = val.as_i64() {
            vocab.insert(k.clone().into_bytes(), id);
        }
    }
    if vocab.is_empty() { None } else { Some(vocab) }
}

fn load_voice(path: &Path) -> Option<Vec<f32>> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() != VOICE_ROWS * STYLE_DIM * 4 {
        return None;
    }
    Some(bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

fn phonemize(t: &str) -> Vec<u8> {
    let norm = text::normalize(t.as_bytes());
    let segs = text::split_segments(&norm);
    let mut joined: Vec<u8> = Vec::new();
    for seg in segs {
        if seg.is_punct {
            joined.extend_from_slice(&seg.text);
        } else {
            joined.extend_from_slice(&espeak::phonemize_segment(&seg.text));
        }
    }
    text::post_process(&joined)
}

fn tokenize(phon: &[u8], vocab: &HashMap<Vec<u8>, i64>) -> Vec<i64> {
    let mut ids = vec![0i64];
    let mut i = 0;
    while i < phon.len() {
        let c = phon[i];
        let n = if c < 0x80 { 1 } else if (c >> 5) == 0x6 { 2 } else if (c >> 4) == 0xE { 3 } else if (c >> 3) == 0x1E { 4 } else { 1 };
        let end = (i + n).min(phon.len());
        if let Some(&id) = vocab.get(&phon[i..end]) {
            ids.push(id);
        }
        i += n;
    }
    ids.push(0);
    ids
}

// Truncate to MAX_CONTENT_TOKENS and wrap in BOS/EOS, mirroring native_synth.rs's
// sub-split — the bench paragraph is well under the limit, so this is a no-op safety net.
fn wrap(content: &[i64]) -> Vec<i64> {
    let window = &content[..content.len().min(MAX_CONTENT_TOKENS)];
    let mut wids = Vec::with_capacity(window.len() + 2);
    wids.push(0);
    wids.extend_from_slice(window);
    wids.push(0);
    wids
}

fn style_row(voice_data: &[f32], n_content_tokens: usize) -> &[f32] {
    let row = ((n_content_tokens + 2) as i64 - 2).clamp(0, VOICE_ROWS as i64 - 1) as usize;
    &voice_data[row * STYLE_DIM..(row + 1) * STYLE_DIM]
}

fn build_session(model: &Path, ep: ort::ep::ExecutionProviderDispatch) -> Result<Session, String> {
    Session::builder()
        .map_err(|e| e.to_string())?
        .with_execution_providers([ep])
        .map_err(|e| e.to_string())?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| e.to_string())?
        .with_memory_pattern(false)
        .map_err(|e| e.to_string())?
        .commit_from_file(model)
        .map_err(|e| e.to_string())
}

fn run_model(session: &mut Session, ids: &[i64], style: &[f32], speed: f32) -> Result<Vec<f32>, String> {
    let input_names: Vec<String> = session.inputs().iter().map(|i| i.name().to_string()).collect();
    let output_name = session.outputs()[0].name().to_string();

    let speed_arr = [speed];
    let mut feeds: Vec<(Cow<str>, SessionInputValue)> = Vec::new();
    for name in &input_names {
        let v = match name.as_str() {
            "input_ids" | "tokens" => SessionInputValue::from(
                TensorRef::from_array_view((vec![1i64, ids.len() as i64], ids)).map_err(|e| e.to_string())?,
            ),
            "style" | "ref_s" => SessionInputValue::from(
                TensorRef::from_array_view((vec![1i64, STYLE_DIM as i64], style)).map_err(|e| e.to_string())?,
            ),
            _ => SessionInputValue::from(
                TensorRef::from_array_view((vec![1i64], speed_arr.as_slice())).map_err(|e| e.to_string())?,
            ),
        };
        feeds.push((Cow::from(name.clone()), v));
    }

    let outputs = session.run(feeds).map_err(|e| e.to_string())?;
    let (_shape, data) = outputs[output_name.as_str()].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
    Ok(data.to_vec())
}
