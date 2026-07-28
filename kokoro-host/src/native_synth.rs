// Native Dawn WebGPU synthesis for the Kindle pipe path — pure Rust. pipe.rs calls
// this to synthesize each chunk so Kindle can be narrated.
//
// The whole synth core is Rust now: espeak-ng phonemization (crate::espeak, a thin FFI
// to espeak-ng.dll) + the kokoro-js text normalizer (crate::text) + the Kokoro ONNX model
// on the ORT Dawn WebGPU EP via the `ort` crate (load-dynamic against the onnxruntime.dll
// staged next to the exe). espeak keeps global state and isn't thread-safe, and the ORT
// session is owned here, so all synthesis is serialized onto ONE dedicated worker thread
// that owns the session for the process lifetime. Requests arrive over an mpsc channel;
// each reply comes back on a tokio oneshot so the async pipe tasks await without blocking.
// Settings (narrator/speed/gain/chunk) come from controls.json in the app-data dir.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use ort::ep::webgpu::WebGPU;
use ort::ep::CPU;
use ort::session::{builder::GraphOptimizationLevel, Session, SessionInputValue};
use ort::value::TensorRef;
use tokio::sync::oneshot;

const STYLE_DIM: usize = 256;
const VOICE_ROWS: usize = 510;
/// Max content tokens (excluding BOS/EOS) per model run. Kokoro's ONNX graph fails the
/// BERT `Expand` node past ~510 tokens, so longer chunks are sub-split to this window.
const MAX_CONTENT_TOKENS: usize = 500;

/// The sentence [`NativeSynth::bench`] times. Fixed and owned by the host (not sent by
/// the client) for two reasons: the number stays comparable between runs and machines,
/// and a client can't make the worker chew on an arbitrarily long text. One sentence, so
/// it's one model run whatever `chunk` is set to, and long enough (~25 words, ~9 s of
/// speech) that per-run overhead doesn't dominate the timing.
const BENCH_TEXT: &str = "The keeper climbed the spiral stairs each evening to make sure \
    the lamp was burning brightly enough to guide the fishing boats safely home.";
/// Bench runs whose timing is thrown away. The first run on a fresh session pays
/// shader-compile (WebGPU) / kernel-selection (CPU) costs that later runs don't, and what
/// we're after is the sustained rate a reading session settles into.
const BENCH_WARMUP_RUNS: u32 = 1;
/// Bench runs that are actually timed. One: the model is deterministic enough that a
/// second sample rarely changes the verdict, and every extra run is another ~10-20 s the
/// user waits at a modal dialog.
const BENCH_TIMED_RUNS: u32 = 1;

/// Which execution provider synthesizes: GPU (Dawn WebGPU, the default) or the plain
/// ORT CPU EP. Benchmarked (the standalone `kokoro-bench` crate) on an Intel UHD 620
/// laptop: WebGPU there ran at 0.50x realtime (behind), CPU fp32 at 1.07x (ahead) —
/// an integrated GPU
/// can lose to plain CPU badly enough to matter. No auto-detection yet; `gpu_synth` in
/// controls.json (default `true`) is a manual escape hatch while real-world results
/// come in — unticking the panel's "Synthesize on GPU" switches to CPU.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    Gpu,
    Cpu,
}

/// The per-utterance settings the pipe host reads from controls.json (replacing the
/// webview's localStorage). `speed`/`gain` default to 1, `chunk` to 4 sentences.
/// `paused` is a live command (not really a setting): while true the pipe stalls the
/// audio stream mid-page so playback pauses without Kindle turning the page.
#[derive(Clone, Copy)]
pub struct Controls {
    pub speed: f32,
    pub gain: f32,
    pub chunk: u32,
    pub paused: bool,
    pub engine: Engine,
}

impl Default for Controls {
    fn default() -> Self {
        Controls { speed: 1.0, gain: 1.0, chunk: 4, paused: false, engine: Engine::Gpu }
    }
}

/// Read narrator + Controls from `<app_data>/controls.json`. Missing file / bad JSON
/// / missing keys fall back to defaults (voice = "af_heart"). Cheap; read per
/// utterance (voice/speed/chunk) and per sub-frame (gain) so slider moves land live.
pub fn read_controls(app_data: &Path) -> (String, Controls) {
    let mut voice = "af_heart".to_string();
    let mut c = Controls::default();
    if let Ok(txt) = std::fs::read_to_string(app_data.join("controls.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            if let Some(s) = v.get("voice").and_then(|x| x.as_str()) {
                voice = s.to_string();
            }
            if let Some(x) = v.get("speed").and_then(|x| x.as_f64()) {
                c.speed = x as f32;
            }
            if let Some(x) = v.get("gain").and_then(|x| x.as_f64()) {
                c.gain = x as f32;
            }
            if let Some(x) = v.get("chunk").and_then(|x| x.as_u64()) {
                c.chunk = x as u32;
            }
            if let Some(x) = v.get("paused").and_then(|x| x.as_bool()) {
                c.paused = x;
            }
            if let Some(x) = v.get("gpu_synth").and_then(|x| x.as_bool()) {
                c.engine = if x { Engine::Gpu } else { Engine::Cpu };
            }
        }
    }
    (voice, c)
}

struct Req {
    text: String,
    speed: f32,
    voice: String,
    engine: Engine,
    reply: oneshot::Sender<Option<Vec<u8>>>,
}

struct BenchReq {
    voice: String,
    engine: Engine,
    reply: oneshot::Sender<Option<Bench>>,
}

/// Work for the synth thread. Both variants need the session/vocab/voice state the
/// worker owns, and both must be serialized against each other — a bench that ran
/// beside a real utterance would time the contention, not the engine.
enum Job {
    Synth(Req),
    Bench(BenchReq),
}

/// One engine's timing from [`NativeSynth::bench`]: how much speech it rendered and how
/// long that took. The ratio (`audio_secs / elapsed_secs`) is the realtime factor —
/// above 1 means the machine can synthesize faster than the speech plays.
#[derive(Clone, Copy, Debug)]
pub struct Bench {
    pub audio_secs: f32,
    pub elapsed_secs: f32,
}

/// Permissive bounds on the model's `speed` input. Wider than any UI offers (the panel's slider
/// is well inside this), because the job here is to exclude the impossible, not to enforce taste.
const MIN_SPEED: f32 = 0.1;
const MAX_SPEED: f32 = 5.0;

/// Clamp a caller-supplied speed to something the model can actually use.
///
/// `speed` is a model INPUT tensor, and it arrives from three places that are all outside this
/// process's control: a raw `f32` off the pipe (`CMD_SYNTH`), a JSON number over the HTTP
/// endpoint, and `controls.json`. None of them validated it, so a NaN, an infinity (`1e300 as
/// f32` is `inf`), or a zero went straight into the graph — and anything on this machine can
/// open the pipe. Sanitizing at this one choke point rather than at each call site means the
/// next transport can't reintroduce the hole.
fn sanitize_speed(speed: f32) -> f32 {
    if speed.is_finite() {
        speed.clamp(MIN_SPEED, MAX_SPEED)
    } else {
        1.0
    }
}

/// Handle to the serialized native synth worker thread. Cloneable Sender inside.
#[derive(Clone)]
pub struct NativeSynth {
    tx: mpsc::Sender<Job>,
}

impl NativeSynth {
    /// Spawn the worker thread. `base` is the model dir (…/onnx-community/Kokoro-82M-
    /// v1.0-ONNX) holding onnx/model.onnx, tokenizer.json, voices/*.bin; `espeak_data`
    /// is the espeak-ng-data dir. The worker inits espeak + ORT eagerly, then lazily
    /// builds the ONNX session on the first request (so startup isn't blocked on the
    /// model download).
    pub fn spawn(base: PathBuf, espeak_data: PathBuf) -> NativeSynth {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("kokoro-native-synth".into())
            .spawn(move || worker_loop(rx, base, espeak_data))
            .expect("spawn native synth thread");
        NativeSynth { tx }
    }

    /// Synthesize one already-cut chunk. Returns raw little-endian f32 PCM bytes
    /// (24 kHz mono) — same shape the webview `synth_result` used, so pipe_server's
    /// framing is unchanged. None on init/synth failure (pipe host emits SYNTH_ERROR).
    pub async fn synth(&self, text: String, speed: f32, voice: String, engine: Engine) -> Option<Vec<u8>> {
        let speed = sanitize_speed(speed);
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Job::Synth(Req { text, speed, voice, engine, reply })).is_err() {
            return None; // worker thread gone
        }
        rx.await.ok().flatten()
    }

    /// Time `engine` on the fixed [`BENCH_TEXT`] and report how much speech it produced
    /// per second of wall clock, so the panel can tell the user which provider is
    /// actually faster on their machine instead of leaving them to guess. Builds a
    /// *fresh* session (an engine that happened to be warm would otherwise measure
    /// better than the one that didn't), discards [`BENCH_WARMUP_RUNS`], then times
    /// [`BENCH_TIMED_RUNS`]. Tens of seconds; None if this engine can't run here.
    ///
    /// Leaves the worker holding a session on `engine` — harmless, since a following
    /// utterance rebuilds on mismatch exactly as it does for a live `gpu_synth` flip.
    pub async fn bench(&self, voice: String, engine: Engine) -> Option<Bench> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Job::Bench(BenchReq { voice, engine, reply })).is_err() {
            return None; // worker thread gone
        }
        rx.await.ok().flatten()
    }
}

fn voice_path(base: &Path, voice: &str) -> PathBuf {
    base.join("voices").join(format!("{voice}.bin"))
}

/// tokenizer.json `model.vocab`: char-string -> id.
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
    if vocab.is_empty() {
        None
    } else {
        Some(vocab)
    }
}

/// voice .bin: VOICE_ROWS x STYLE_DIM float32.
fn load_voice(path: &Path) -> Option<Vec<f32>> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() != VOICE_ROWS * STYLE_DIM * 4 {
        return None;
    }
    Some(bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

/// Normalize -> segment -> espeak-phonemize each non-punct segment -> post-process
/// (the kokoro-js phonemize path).
fn phonemize(text: &str) -> Vec<u8> {
    let norm = crate::text::normalize(text.as_bytes());
    let segs = crate::text::split_segments(&norm);
    let mut joined: Vec<u8> = Vec::new();
    for seg in segs {
        if seg.is_punct {
            joined.extend_from_slice(&seg.text);
        } else {
            joined.extend_from_slice(&crate::espeak::phonemize_segment(&seg.text));
        }
    }
    crate::text::post_process(&joined)
}

/// BOS + per-UTF-8-char vocab lookup + EOS.
fn tokenize(phon: &[u8], vocab: &HashMap<Vec<u8>, i64>) -> Vec<i64> {
    let mut ids = vec![0i64]; // BOS
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
    ids.push(0); // EOS
    ids
}

/// Run the Kokoro model for one token sequence. Stock fp32 model.onnx: int64
/// input_ids, f32 style[1,256], f32 speed[1] -> f32 waveform.
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

fn build_session(model: &Path, engine: Engine) -> Result<Session, String> {
    let ep = match engine {
        Engine::Gpu => WebGPU::default().build(),
        Engine::Cpu => CPU::default().build(),
    };
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

fn worker_loop(rx: mpsc::Receiver<Job>, base: PathBuf, espeak_data: PathBuf) {
    let model = base.join("onnx").join("model.onnx");
    let tokenizer = base.join("tokenizer.json");
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default();

    // espeak + ORT init eagerly (neither needs the downloaded model). A failure here
    // means we can't synthesize at all — drain requests replying None.
    let mut broken = false;
    if let Err(e) = crate::espeak::init(&espeak_data.to_string_lossy()) {
        eprintln!("[native-synth] espeak init failed: {e}");
        broken = true;
    }
    // No default execution provider registered globally — each session picks its own
    // (GPU or CPU) per the `engine` control, since a running host can switch live.
    match ort::init_from(exe_dir.join("onnxruntime.dll")) {
        Ok(b) => {
            b.commit();
        }
        Err(e) => {
            eprintln!("[native-synth] ort init_from failed: {e}");
            broken = true;
        }
    }

    // Lazily built on the first request (so model download isn't blocked on them).
    let mut session: Option<Session> = None;
    let mut vocab: Option<HashMap<Vec<u8>, i64>> = None;
    let mut voice_data: Vec<f32> = Vec::new();
    let mut cur_voice = String::new();
    let mut cur_engine = Engine::Gpu;

    while let Ok(job) = rx.recv() {
        let req = match job {
            Job::Synth(req) => req,
            // Timing one engine. Shares this thread (and its session/vocab/voice state)
            // with real synthesis so the two can never overlap and skew each other.
            Job::Bench(req) => {
                // Assemble what this engine needs, then time it. A `None` from anywhere
                // in here means "this engine can't run on this machine" — the panel
                // reports that as unavailable rather than as a very slow result.
                let result = (|| -> Option<Bench> {
                    if broken {
                        return None;
                    }
                    if vocab.is_none() {
                        vocab = load_vocab(&tokenizer);
                    }
                    let vocab_ref = vocab.as_ref()?;
                    if req.voice != cur_voice || voice_data.is_empty() {
                        voice_data = load_voice(&voice_path(&base, &req.voice))?;
                        cur_voice = req.voice.clone();
                    }
                    // Always a fresh session. Reusing a warm one would flatter whichever
                    // engine the host happened to be running, which is the very thing
                    // being compared.
                    let mut s = match build_session(&model, req.engine) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!(
                                "[native-synth] bench session build failed ({:?}): {e}",
                                req.engine
                            );
                            return None;
                        }
                    };

                    let ids = tokenize(&phonemize(BENCH_TEXT), vocab_ref);
                    if ids.len() <= 2 {
                        return None; // phonemization produced nothing to run
                    }
                    let content = &ids[1..ids.len() - 1];
                    let window = &content[..content.len().min(MAX_CONTENT_TOKENS)];
                    let mut wids = Vec::with_capacity(window.len() + 2);
                    wids.push(0); // BOS
                    wids.extend_from_slice(window);
                    wids.push(0); // EOS
                    let row = (wids.len() as i64 - 2).clamp(0, VOICE_ROWS as i64 - 1) as usize;
                    let style = &voice_data[row * STYLE_DIM..(row + 1) * STYLE_DIM];

                    // Speed is pinned to 1.0, not the user's setting: it scales how much
                    // audio a run produces, so letting it vary would move the realtime
                    // factor for reasons that have nothing to do with the hardware.
                    for _ in 0..BENCH_WARMUP_RUNS {
                        if let Err(e) = run_model(&mut s, &wids, style, 1.0) {
                            eprintln!(
                                "[native-synth] bench warmup failed ({:?}): {e}",
                                req.engine
                            );
                            return None;
                        }
                    }
                    let t0 = std::time::Instant::now();
                    let mut samples = 0usize;
                    for _ in 0..BENCH_TIMED_RUNS {
                        match run_model(&mut s, &wids, style, 1.0) {
                            Ok(pcm) => samples += pcm.len(),
                            Err(e) => {
                                eprintln!(
                                    "[native-synth] bench run failed ({:?}): {e}",
                                    req.engine
                                );
                                return None;
                            }
                        }
                    }
                    let elapsed_secs = t0.elapsed().as_secs_f32();

                    // Keep the session we just built (and record its engine), so an
                    // utterance on this engine doesn't pay for another build.
                    session = Some(s);
                    cur_engine = req.engine;
                    Some(Bench {
                        audio_secs: samples as f32 / kokoro_protocol::SAMPLE_RATE as f32,
                        elapsed_secs,
                    })
                })();
                match &result {
                    Some(b) => eprintln!(
                        "[native-synth] bench {:?}: {:.1}s audio in {:.1}s ({:.2}x realtime)",
                        req.engine,
                        b.audio_secs,
                        b.elapsed_secs,
                        b.audio_secs / b.elapsed_secs.max(f32::MIN_POSITIVE)
                    ),
                    None => eprintln!("[native-synth] bench {:?}: unavailable", req.engine),
                }
                let _ = req.reply.send(result);
                continue;
            }
        };

        if broken {
            let _ = req.reply.send(None);
            continue;
        }

        // Lazy init on the first request (using its narrator), then narrator switches
        // just reload the voice matrix (keeps the session).
        if session.is_none() {
            match build_session(&model, req.engine) {
                Ok(s) => {
                    session = Some(s);
                    cur_engine = req.engine;
                }
                Err(e) => {
                    eprintln!("[native-synth] session build failed: {e}");
                    let _ = req.reply.send(None);
                    continue;
                }
            }
            match load_vocab(&tokenizer) {
                Some(v) => vocab = Some(v),
                None => {
                    eprintln!("[native-synth] tokenizer vocab load failed");
                    session = None;
                    let _ = req.reply.send(None);
                    continue;
                }
            }
            match load_voice(&voice_path(&base, &req.voice)) {
                Some(v) => {
                    voice_data = v;
                    cur_voice = req.voice.clone();
                }
                None => {
                    eprintln!("[native-synth] voice .bin load failed: {}", req.voice);
                    session = None;
                    vocab = None;
                    let _ = req.reply.send(None);
                    continue;
                }
            }
            eprintln!(
                "[native-synth] Kokoro synth ready (ONNX + {cur_engine:?}), voice={cur_voice}"
            );
        } else {
            if req.voice != cur_voice {
                match load_voice(&voice_path(&base, &req.voice)) {
                    Some(v) => {
                        voice_data = v;
                        cur_voice = req.voice.clone();
                    }
                    None => eprintln!(
                        "[native-synth] set_voice({}) failed (keeping {cur_voice})",
                        req.voice
                    ),
                }
            }
            // Engine switches need a fresh session (the EP is fixed at session-build
            // time); rare (a manual controls.json flip), so the rebuild cost is fine.
            if req.engine != cur_engine {
                match build_session(&model, req.engine) {
                    Ok(s) => {
                        *session.as_mut().unwrap() = s;
                        cur_engine = req.engine;
                        eprintln!("[native-synth] switched engine to {cur_engine:?}");
                    }
                    Err(e) => eprintln!(
                        "[native-synth] engine switch to {:?} failed (keeping {cur_engine:?}): {e}",
                        req.engine
                    ),
                }
            }
        }

        let vocab_ref = vocab.as_ref().unwrap();

        // Phonemize -> tokens.
        let phon = phonemize(&req.text);
        let ids = tokenize(&phon, vocab_ref);
        if ids.len() <= 2 {
            let _ = req.reply.send(Some(Vec::new())); // empty/punctuation-only chunk
            continue;
        }

        // Kokoro's model accepts at most ~512 tokens (510 content + BOS/EOS); a longer
        // sequence fails the BERT `Expand` node ("invalid expand shape"). Chunks can be
        // several sentences (controls `chunk`), so a long chunk must be sub-split into
        // <=MAX_CONTENT_TOKENS windows — each wrapped in its own BOS/EOS — and their PCM
        // concatenated. A window boundary lands at a token seam (rare, brief).
        let content = &ids[1..ids.len() - 1];
        let mut bytes: Vec<u8> = Vec::new();
        let mut failed = false;
        for window in content.chunks(MAX_CONTENT_TOKENS) {
            let mut wids = Vec::with_capacity(window.len() + 2);
            wids.push(0); // BOS
            wids.extend_from_slice(window);
            wids.push(0); // EOS

            // style row = clamp(nTokens-2, 0, 509) (kokoro-js generate_from_ids).
            let row = (wids.len() as i64 - 2).clamp(0, VOICE_ROWS as i64 - 1) as usize;
            let style = &voice_data[row * STYLE_DIM..(row + 1) * STYLE_DIM];

            // Small retry for a transient Dawn WebGPU device error (rebuild the session
            // before the last attempt); an over-long window is deterministic and won't be
            // rescued by retry, which is why the sub-split above matters.
            let mut window_pcm = None;
            for attempt in 0..3u32 {
                match run_model(session.as_mut().unwrap(), &wids, style, req.speed) {
                    Ok(pcm) => {
                        window_pcm = Some(pcm);
                        break;
                    }
                    Err(e) => {
                        eprintln!(
                            "[native-synth] synth attempt {} failed ({} tokens): {e}",
                            attempt + 1,
                            wids.len()
                        );
                        if attempt == 1 {
                            match build_session(&model, cur_engine) {
                                Ok(s) => *session.as_mut().unwrap() = s,
                                Err(be) => eprintln!("[native-synth] session rebuild failed: {be}"),
                            }
                        }
                        std::thread::sleep(std::time::Duration::from_millis(80));
                    }
                }
            }
            match window_pcm {
                Some(pcm) => {
                    bytes.reserve(pcm.len() * 4);
                    for s in pcm {
                        bytes.extend_from_slice(&s.to_le_bytes());
                    }
                }
                None => {
                    failed = true;
                    break;
                }
            }
        }
        let _ = req.reply.send(if failed { None } else { Some(bytes) });
    }
}
