// SPDX-License-Identifier: MIT AND Apache-2.0
//
// Mixed licence: this file is MIT (see LICENSE) except for the style-row selection rule
// `clamp(nTokens - 2, 0, 509)` (marked below), which is taken from kokoro-js's
// `generate_from_ids` (https://github.com/hexgrad/kokoro, npm `kokoro-js`), licensed
// under the Apache License, Version 2.0. Modified for Kokoro Kindle Reader: that rule is
// applied inside this native Rust/ORT synthesis pipeline. Full licence text:
// licenses/Apache-2.0.txt. See THIRD_PARTY_NOTICES.md for the complete list of files this
// notice covers.
//
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

/// A 326 MB on-disk graph earlier builds loaded in place of `model.onnx` to get the
/// duration outputs. [`crate::model_patch`] now makes the same edit in memory at session
/// build time, so this file is never loaded and never produced — it is only looked for so
/// the host can tell anyone who installed one by hand that it is dead weight.
const LEGACY_VARIANT: &str = "kokoro-claude-variant.onnx";

/// Output name carrying integer frames per token, added by [`crate::model_patch`].
/// Selected by name, never by index: the stock graph has one output and the patched graph
/// has three, and an index that silently addressed the wrong tensor would still typecheck.
///
/// The host reads only this one. The patch also exposes `duration_cumsum`, which is
/// `cumsum` of the same tensor and so carries nothing new — it is a second witness when
/// auditing the graph offline, and is never fetched here. ORT is asked for outputs by name,
/// so an unfetched one costs nothing at run time; it was measured not to disturb either EP.
const DURATIONS_OUTPUT: &str = "durations_frames";
/// The waveform output's name on both graphs. Falls back to output 0 if absent.
const WAVEFORM_OUTPUT: &str = "waveform";

/// Audio samples the length regulator emits per duration frame — 25.0 ms at 24 kHz.
///
/// Derived rather than assumed, and asserted at runtime on every run: `sum(frames) *
/// SAMPLES_PER_FRAME` must equal the waveform length exactly, which it did in every case
/// measured. If it ever doesn't, the frames are describing different audio than the one
/// being played and the marks are dropped rather than shipped.
const SAMPLES_PER_FRAME: usize = 600;

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
/// ORT CPU EP. Both ship because an integrated GPU can lose to plain CPU by enough to put
/// synthesis behind realtime — measured on a real laptop, where the GPU was the slower of
/// the two. There is no auto-detection; `gpu_synth` in controls.json (default `true`) is the
/// manual escape hatch, and the panel's "Test speed" dialog ([`bench`]) is how a user finds
/// out which way their own machine falls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    Gpu,
    Cpu,
}

/// The per-utterance settings the pipe host reads from controls.json (replacing the
/// webview's localStorage). `speed`/`gain` default to 1, `chunk` to 4 sentences.
///
/// Settings only. Pause is *not* here: it's a live command, it is owned by the host
/// (`state::HostState`), and it arrives over the pipe as `CMD_KINDLE` — a file the panel
/// wrote to was the wrong home for it, and persisting it meant a host could come back up
/// already stalled.
#[derive(Clone, Copy)]
pub struct Controls {
    pub speed: f32,
    pub gain: f32,
    pub chunk: u32,
    pub engine: Engine,
}

/// The execution provider used when `controls.json` says nothing — which is not only a
/// fresh install. `read_controls` falls back to `Controls::default()` for a missing file,
/// unparseable JSON (a UTF-8 BOM does it, silently) and a missing `gpu_synth` key alike, so
/// this constant is what actually runs in every one of those cases. Writing an initial
/// settings file would not have covered any of them.
///
/// Windows keeps the Dawn WebGPU default it shipped with. Linux defaults to CPU because
/// that is the provider its port has been built and validated for; enabling GPU selection
/// there is a later, separately validated step (native WebGPU is Vulkan on Linux, and
/// neither the packaged libraries nor the driver surface has been measured yet).
#[cfg(windows)]
const DEFAULT_ENGINE: Engine = Engine::Gpu;
#[cfg(not(windows))]
const DEFAULT_ENGINE: Engine = Engine::Cpu;

impl Default for Controls {
    fn default() -> Self {
        Controls { speed: 1.0, gain: 1.0, chunk: 4, engine: DEFAULT_ENGINE }
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
    reply: oneshot::Sender<Option<Synthesized>>,
}

/// One synthesized chunk: its audio, and where in that audio each of its words is spoken.
///
/// `marks` is **empty whenever the timing could not be established** — the stock graph is
/// installed, the durations didn't validate, the aggregation produced nothing — and never
/// approximate. A caller reading it must treat empty as "no timing for this chunk" and fall
/// back to whatever it did before, not as "this chunk has no words". Audio and timing fail
/// independently on purpose: a chunk that can't be marked is still a chunk that must be
/// spoken.
///
/// `marks` is chunk-relative in both axes: this layer is handed one chunk and knows nothing
/// about where it sat in the request, so the transport — which does — rebases the characters
/// as it writes the header.
pub struct Synthesized {
    pub pcm: Vec<u8>,
    // Read on the Kindle path (`CMD_SYNTH_ALIGNED`) and not yet on the browser's, which
    // returns PCM and nothing else — so off Windows this is computed and discarded. That is
    // deliberate: the marks are the better source and carrying them across is a change to
    // the response shape *and* the extension, not to this layer. Keep producing them.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub marks: Vec<WordMark>,
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
    // The speed test is the settings panel's, and the panel reaches the host over the named
    // pipe — which only Windows has today. The port plan brings it back on Linux over that
    // platform's own native IPC, and the GPU stage is what it exists for, so this is kept
    // rather than gated out.
    #[cfg_attr(not(windows), allow(dead_code))]
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

/// One source word's stretch of audio: the synth layer's transport-neutral timing unit.
///
/// Transport-neutral on purpose. Kindle reaches the host over the pipe and the browser
/// over loopback HTTP, and both need the same answer to "when is this word spoken"; a mark
/// shaped for either one would have to be re-derived for the other, which is how two
/// estimators end up disagreeing about the same page.
///
/// **A mark addresses the plain UTF-16 utterance the host was sent** — not the normalized
/// text, not UTF-8 byte positions, not indices into the phoneme string, not SAPI SSML
/// positions, and not the browser's OCR boxes. Every one of those is a coordinate space
/// something in this pipeline actually uses, and a mark that quietly meant one of them
/// would still look plausible while highlighting the wrong word.
///
/// Both axes are relative to **the chunk** as this layer produces them, and `pipe.rs` makes
/// `char_start_utf16` absolute as it writes the chunk header. The samples stay
/// chunk-relative on the wire too (see [`kokoro_protocol::CHUNK_ALIGNED`]) — the client
/// already tracks where the chunk's audio began.
///
/// An expansion keeps ONE mark. `1997` is spoken as several words, but it is one token on
/// the page, so its mark spans the whole of it and the highlight sits there while all of it
/// is read — which is what [`crate::text::Span`]'s collapsing rule is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WordMark {
    pub char_start_utf16: u32,
    pub char_len_utf16: u32,
    pub sample_start: u32,
    pub sample_end: u32,
}

impl WordMark {
    /// Convert the synth layer's aggregated spans into wire-ready marks.
    ///
    /// `offsets` is [`crate::text::utf16_offsets`] over the chunk this layer was handed, so
    /// the result is **chunk-relative** in both axes. Rebasing onto the request text happens
    /// in exactly one place — `pipe.rs`, as it writes the chunk header — because that is the
    /// only layer that knows where the chunk sat. A second rebase here would be a second
    /// place for the two to disagree.
    ///
    /// Marks whose character span resolves empty are dropped rather than emitted: the wire
    /// format rejects a zero-length span, and a mark that addresses no character cannot
    /// highlight anything anyway.
    pub fn from_timed(timed: &[crate::text::TimedSpan], offsets: &[u32]) -> Vec<WordMark> {
        let mut out = Vec::with_capacity(timed.len());
        for t in timed {
            let (a, b) = crate::text::span_to_utf16(t.span, offsets);
            if b <= a {
                continue;
            }
            out.push(WordMark {
                char_start_utf16: a,
                char_len_utf16: b - a,
                sample_start: t.sample_start,
                sample_end: t.sample_end,
            });
        }
        out
    }

    /// This mark as the four wire words of a [`kokoro_protocol::CHUNK_ALIGNED`] entry.
    pub fn as_wire(&self) -> (u32, u32, u32, u32) {
        (self.char_start_utf16, self.char_len_utf16, self.sample_start, self.sample_end)
    }

    /// Whether `marks` is a mark stream this host may put on the wire for a chunk of
    /// `chunk_samples` samples covering `[chunk_char_start, +chunk_char_len)`.
    ///
    /// Checked on the *producing* side as well as the consuming one, against the shared
    /// rule in `kokoro-protocol` so the two ends cannot drift. A mark list that fails its
    /// own invariants is a synthesis error to report, not something to ship and let the
    /// engine reject inside Kindle — by then the page is already silent and the reason for
    /// it is in the wrong process's log.
    pub fn stream_is_valid(
        marks: &[WordMark],
        chunk_char_start: u32,
        chunk_char_len: u32,
        chunk_samples: u32,
    ) -> bool {
        if marks.len() as u64 > kokoro_protocol::MAX_MARKS_PER_CHUNK as u64
            || marks.len() as u64 > chunk_char_len as u64
        {
            return false;
        }
        let mut prev = (0u32, 0u32);
        for m in marks {
            if !kokoro_protocol::mark_is_valid(
                m.as_wire(),
                prev,
                chunk_char_start,
                chunk_char_len,
                chunk_samples,
            ) {
                return false;
            }
            prev = (m.char_start_utf16.saturating_add(m.char_len_utf16), m.sample_end);
        }
        true
    }
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
    /// framing is unchanged — plus this chunk's word marks when the model can supply them
    /// (see [`Synthesized`]). None on init/synth failure (pipe host emits SYNTH_ERROR).
    pub async fn synth(
        &self,
        text: String,
        speed: f32,
        voice: String,
        engine: Engine,
    ) -> Option<Synthesized> {
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
    ///
    /// Unreached off Windows until the Linux panel has a transport; see the `Job::Bench`
    /// note above.
    #[cfg_attr(not(windows), allow(dead_code))]
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

/// The graph to load: always the stock, manifest-verified `model.onnx`. The duration
/// outputs are appended to its bytes on the way into the session ([`build_session`]), so
/// there is no second graph to choose between and no way for the timing source to depend on
/// what happens to be sitting in the model directory.
fn model_path(base: &Path) -> PathBuf {
    base.join("onnx").join("model.onnx")
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
    phonemize_spans(text).0
}

/// [`phonemize`] plus, for every phoneme byte, the range of the ORIGINAL utterance bytes
/// that produced it.
///
/// The chain is normalization spans (source token -> normalized text) composed with
/// espeak's phoneme attribution (normalized text -> phonemes) composed with the
/// post-processing spans. Each link already exists and is tested on its own; this is where
/// they meet, and it is what a word mark's character half will be aggregated from.
///
/// The phoneme string is identical to what [`phonemize`] has always returned — every stage
/// carries its mapping alongside the bytes rather than in place of them.
fn phonemize_spans(text: &str) -> (Vec<u8>, Vec<crate::text::Span>) {
    use crate::text::Span;

    let norm = crate::text::normalize_spans(text.as_bytes());
    let segs = crate::text::split_segments(&norm.text);
    let mut joined: Vec<u8> = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    for seg in segs {
        if seg.is_punct {
            // Punctuation passes through as itself, so each byte keeps its own span.
            for k in 0..seg.text.len() {
                joined.push(seg.text[k]);
                spans.push(norm.spans[seg.start + k]);
            }
        } else {
            let ph = crate::espeak::phonemize_segment_spans(&seg.text);
            // espeak reports in the SEGMENT's own bytes; lift those through the segment's
            // offset into normalized text, then through normalization back to the source.
            // A span that doesn't resolve means espeak pointed outside the text it was
            // given: fall back to the whole segment rather than drop the phoneme — the
            // mapping goes coarse, the audio is untouched.
            let whole = crate::text::lift(
                &norm.spans,
                seg.start,
                Span { start: 0, end: seg.text.len() as u32 },
            );
            for (k, sp) in ph.spans.iter().enumerate() {
                let src = crate::text::lift(&norm.spans, seg.start, *sp)
                    .or(whole)
                    .unwrap_or(Span { start: 0, end: 0 });
                spans.push(src);
                joined.push(ph.text[k]);
            }
        }
    }
    let (phon, mut out) = crate::text::post_process_spans(&joined, &spans);
    // Snap to whole characters of the original. Punctuation passes through byte by byte,
    // so a multi-byte character would otherwise leave one span per byte — two of an
    // em-dash's three covering no character at all.
    for sp in out.iter_mut() {
        *sp = crate::text::snap_to_chars(*sp, text.as_bytes());
    }
    (phon, out)
}

/// BOS + per-UTF-8-char vocab lookup + EOS.
///
/// Delegates so there is one tokenizer, not two that can drift over which phoneme
/// characters the vocabulary covers. The discarded span vector costs one allocation on a
/// path (`bench`) that then runs the model several times.
fn tokenize(phon: &[u8], vocab: &HashMap<Vec<u8>, i64>) -> Vec<i64> {
    tokenize_spans(phon, &[], vocab).0
}

/// [`tokenize`] plus, for each token, the source it came from — the last link in the chain
/// the model's durations are laid against.
///
/// `spans` is [`phonemize_spans`]'s per-phoneme-byte mapping; pass an empty slice to opt out
/// (the returned spans are then all `None` and the ids are unchanged). The two returned
/// vectors are the same length **and are indexed the same way as the model's duration
/// output**, which is why BOS and EOS are present as `None` rather than omitted: the graph
/// emits a frame count for them too, and dropping them here would shift every later token's
/// audio one position earlier.
///
/// A phoneme character not in the vocabulary produces no token, so this is not a
/// position-preserving map — which is exactly why the spans have to be carried through the
/// same loop that does the lookup rather than zipped on afterwards.
fn tokenize_spans(
    phon: &[u8],
    spans: &[crate::text::Span],
    vocab: &HashMap<Vec<u8>, i64>,
) -> (Vec<i64>, Vec<Option<crate::text::Span>>) {
    use crate::text::Span;
    let mut ids = vec![0i64]; // BOS
    let mut out: Vec<Option<Span>> = vec![None]; // BOS speaks no source
    let mut i = 0;
    while i < phon.len() {
        let c = phon[i];
        let n = if c < 0x80 { 1 } else if (c >> 5) == 0x6 { 2 } else if (c >> 4) == 0xE { 3 } else if (c >> 3) == 0x1E { 4 } else { 1 };
        let end = (i + n).min(phon.len());
        if let Some(&id) = vocab.get(&phon[i..end]) {
            ids.push(id);
            // Union over the character's bytes. A multi-byte phoneme is one token, and its
            // bytes can carry different spans where post-processing spliced around them.
            out.push(spans[i.min(spans.len())..end.min(spans.len())].iter().fold(
                None,
                |acc: Option<Span>, s| {
                    Some(match acc {
                        None => *s,
                        Some(a) => Span { start: a.start.min(s.start), end: a.end.max(s.end) },
                    })
                },
            ));
        }
        i += n;
    }
    ids.push(0); // EOS
    out.push(None);
    (ids, out)
}

/// One model run's outputs: the audio, and the per-token frame counts when the graph
/// exposes them (a patched session does, a stock one does not).
struct Run {
    pcm: Vec<f32>,
    /// One entry per token of `ids`, BOS and EOS included — they hold the leading and
    /// trailing silence. `None` on the stock graph.
    frames: Option<Vec<u32>>,
}

/// Run the Kokoro model for one token sequence. Stock fp32 model.onnx: int64
/// input_ids, f32 style[1,256], f32 speed[1] -> f32 waveform. [`crate::model_patch`] adds
/// [`DURATIONS_OUTPUT`], which is asked for only when the loaded graph declares it.
fn run_model(session: &mut Session, ids: &[i64], style: &[f32], speed: f32) -> Result<Run, String> {
    let input_names: Vec<String> = session.inputs().iter().map(|i| i.name().to_string()).collect();
    let out_names: Vec<String> = session.outputs().iter().map(|o| o.name().to_string()).collect();
    // By name where the name exists; index 0 only as the fallback for a graph that doesn't
    // label its waveform. Blind indexing is what would make a graph that reordered its
    // outputs play the duration tensor as audio.
    let output_name = out_names
        .iter()
        .find(|n| n.as_str() == WAVEFORM_OUTPUT)
        .cloned()
        .unwrap_or_else(|| out_names.first().cloned().unwrap_or_default());
    let want_durations = out_names.iter().any(|n| n.as_str() == DURATIONS_OUTPUT);

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
    let pcm = data.to_vec();

    // The durations are float32 holding integral values (Round and Clip preserve the
    // element type upstream of them), so they are read as f32 and rounded — not extracted
    // as an integer tensor, which would fail. `max(1)` mirrors the graph's own clamp.
    //
    // A failure here is NOT a synthesis failure: the audio above is already correct and
    // must be returned. Losing the durations costs the highlight its precision, nothing
    // more, so every problem below degrades to `None`.
    let frames = if !want_durations {
        None
    } else {
        match outputs[DURATIONS_OUTPUT].try_extract_tensor::<f32>() {
            Ok((_s, d)) if d.len() == ids.len() => {
                Some(d.iter().map(|&f| (f.round().max(1.0)) as u32).collect::<Vec<u32>>())
            }
            Ok((_s, d)) => {
                eprintln!(
                    "[native-synth] {DURATIONS_OUTPUT}: {} frames for {} tokens — no marks",
                    d.len(),
                    ids.len()
                );
                None
            }
            Err(e) => {
                eprintln!("[native-synth] {DURATIONS_OUTPUT} extract failed: {e} — no marks");
                None
            }
        }
    }
    // The arithmetic that ties frames to the audio actually produced. Asserted per run
    // rather than trusted from the offline measurement: this is the one check that would
    // notice the graph, the EP or the frame size changing under us, and the cost of missing
    // it is a highlight that drifts further from the voice with every word.
    .filter(|f: &Vec<u32>| {
        let want = f.iter().map(|&x| x as usize).sum::<usize>() * SAMPLES_PER_FRAME;
        if want != pcm.len() {
            eprintln!(
                "[native-synth] frames*{SAMPLES_PER_FRAME}={want} but {} samples — no marks",
                pcm.len()
            );
        }
        want == pcm.len()
    });

    Ok(Run { pcm, frames })
}

fn commit_session(model_bytes: &[u8], engine: Engine) -> Result<Session, String> {
    // An explicit `gpu_synth: true` off Windows is honoured as CPU, and said out loud. The
    // WebGPU EP is Vulkan here and nothing about it has been validated — provisioned
    // library, `ort` registration, driver — so registering it would trade a working
    // narrator for a session build that fails, which reads to the user as "Linux can't
    // speak" rather than "this setting isn't ready". Silence would be worse than either:
    // the panel would go on showing GPU while CPU did the work. Delete this when the GPU
    // stage lands and the provider is chosen by what is actually installed.
    #[cfg(not(windows))]
    let engine = match engine {
        Engine::Gpu => {
            eprintln!(
                "[native-synth] gpu_synth is set, but GPU synthesis is not yet validated on                  this platform — using CPU"
            );
            Engine::Cpu
        }
        Engine::Cpu => Engine::Cpu,
    };
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
        .commit_from_memory(model_bytes)
        .map_err(|e| e.to_string())
}

/// Build a session on `engine` from the stock `model.onnx`, with
/// [`crate::model_patch::duration_outputs`] appended so the graph also returns the
/// per-token durations that make [`WordMark`]s possible.
///
/// The patch is applied to a buffer, never to the file. Writing it out was the previous
/// design and it cost a 326 MB sidecar that only existed on machines where someone had run
/// a Python script; it also had to dodge the panel's startup verify, which deletes any file
/// under the model dir whose SHA-256 doesn't match the manifest. In memory there is nothing
/// to distribute, nothing to verify and nothing to delete — and the bytes ORT sees are
/// provably the manifest-verified file plus 273 bytes this crate can print.
///
/// **A rejected patch must cost the marks, not the audio.** If the patched graph doesn't
/// load — an ORT that won't merge a repeated `graph` field, an upstream export that renamed
/// the duration tensors — the same buffer is truncated back to exactly the bytes read from
/// disk and committed again. That second attempt is the stock model by construction, so the
/// host still speaks; `run_model` sees no `durations_frames` among the session's outputs and
/// the chunk falls back to interpolated word timing — the same path every host took before
/// the duration outputs existed.
fn build_session(model: &Path, engine: Engine) -> Result<Session, String> {
    use std::io::Read;

    // Read with room for the patch ALREADY reserved. `extend_from_slice` past the end of an
    // exactly-sized `Vec` grows it by *doubling*, so appending 273 bytes to a 325 MB model
    // asks for a second 325 MB and memcpys the first into it — pure waste, since the final
    // length is 273 bytes over.
    //
    // Measured on this machine, one session build: peak commit 1211 MB before, ~905 MB after
    // — a ~305 MB saving, which is the model size, so it is the doubling and nothing else.
    // Peak RSS barely moved on those runs (761 -> 754 MB), but do not read that as a
    // guarantee: during a reallocation the old buffer and the copied-into half of the new one
    // CAN both be resident. What is reliably bought is commit charge and a 325 MB memcpy, not
    // working set. Worth having because an engine switch builds the new session while the old
    // one is still alive, and commit is what a machine runs out of first.
    let patch = crate::model_patch::duration_outputs();
    let mut file =
        std::fs::File::open(model).map_err(|e| format!("open {}: {e}", model.display()))?;
    // Size from the OPEN HANDLE, never from a second lookup by path. The panel's verify
    // deletes and re-downloads any model file whose SHA-256 mismatches, so a stat that raced
    // that replacement describes a different file — and a hint that lands too small puts the
    // doubling straight back, silently, during a repair. `std::fs::read` sizes itself this way
    // for the same reason. A failed metadata call is the same trap wearing a zero, which is
    // why it is worth taking from the handle rather than defaulting.
    let hint = file.metadata().map(|m| m.len() as usize).unwrap_or(0);
    let mut bytes = Vec::with_capacity(hint + patch.len());
    file.read_to_end(&mut bytes).map_err(|e| format!("read {}: {e}", model.display()))?;
    // From what was actually read, never from the metadata — the truncation below has to
    // restore the exact bytes ORT was given, and a file that grew since the stat would leave
    // a partial graph behind instead of the stock one.
    let stock_len = bytes.len();
    bytes.extend_from_slice(&patch);

    match commit_session(&bytes, engine) {
        Ok(s) => {
            eprintln!("[native-synth] session: {engine:?}, model-derived word timing");
            Ok(s)
        }
        Err(e) => {
            // Deliberately not phrased as "the patch is bad": a session build also fails
            // when the execution provider itself is unavailable, and that failure arrives
            // here first. The retry below tells the two apart — if it succeeds, the patch
            // was the problem. For the same reason this promises only a retry: when the EP
            // is what's missing, the retry fails too and the outcome is no audio at all, not
            // estimated timing. The result is logged once it is known.
            eprintln!("[native-synth] patched graph did not load ({e}) — retrying stock");
            bytes.truncate(stock_len);
            let stock = commit_session(&bytes, engine);
            match &stock {
                Ok(_) => eprintln!(
                    "[native-synth] session: {engine:?}, stock graph — word timing estimated"
                ),
                Err(e) => eprintln!("[native-synth] stock graph did not load either ({e})"),
            }
            stock
        }
    }
}

/// Point `ort` at the `onnxruntime.dll` staged beside the exe, once for the process.
///
/// **Call this before spawning anything that might build a session**, which is why `main` does
/// it first and this worker only re-runs it as a backstop. Two things use ORT now — the synth
/// here and `kokoro-ocr`'s worker — and whichever touches a session first is what decides which
/// library the process loads. They do not decide it the same way: this names the staged DLL by
/// absolute path, while `ort`'s lazy path honours `ORT_DYLIB_PATH` first and only then falls
/// back to searching beside the exe. With that variable set, "whoever got there first" is the
/// difference between two different runtimes, and the window is a few milliseconds of startup —
/// short enough never to be hit on purpose and long enough to be real.
///
/// Idempotent by construction: loading an already-loaded dylib is a no-op, and `commit`
/// returning false means an environment was already configured, which is the outcome this
/// wants. No default execution provider is registered globally — each session picks its own
/// (GPU or CPU) per the `engine` control, since a running host can switch live.
pub fn init_ort(exe_dir: &Path) -> Result<(), String> {
    match ort::init_from(exe_dir.join(ORT_LIBRARY)) {
        Ok(b) => {
            b.commit();
            Ok(())
        }
        Err(e) => Err(format!("ort init_from failed: {e}")),
    }
}

/// The ONNX Runtime shared library, by the name the provisioning recipe stages beside the
/// exe. Named here rather than left to `ort`'s own search because [`init_ort`] must settle
/// which library the process loads before either worker can start one — and `ort`'s lazy
/// fallback honours `ORT_DYLIB_PATH` first, so "whichever ran first" is not a stable answer.
#[cfg(windows)]
const ORT_LIBRARY: &str = "onnxruntime.dll";
#[cfg(not(windows))]
const ORT_LIBRARY: &str = "libonnxruntime.so";

/// The directory the exe lives in, which is where its runtime libraries are staged.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default()
}

fn worker_loop(rx: mpsc::Receiver<Job>, base: PathBuf, espeak_data: PathBuf) {
    let model = model_path(&base);
    eprintln!("[native-synth] model: {}", model.display());
    // A leftover from when the duration outputs shipped as a separate 326 MB graph. It is
    // never loaded now, so say so once rather than let it sit there looking load-bearing.
    if model.with_file_name(LEGACY_VARIANT).is_file() {
        eprintln!(
            "[native-synth] {LEGACY_VARIANT} is present and no longer used \
             (the duration outputs are added in memory) — it can be deleted"
        );
    }
    let tokenizer = base.join("tokenizer.json");
    let exe_dir = exe_dir();

    // espeak + ORT init eagerly (neither needs the downloaded model). A failure here
    // means we can't synthesize at all — drain requests replying None.
    let mut broken = false;
    if let Err(e) = crate::espeak::init(&espeak_data.to_string_lossy()) {
        eprintln!("[native-synth] espeak init failed: {e}");
        broken = true;
    }
    // Ordinarily a no-op: `main` has already done this before anything was spawned. Kept
    // because this worker cannot function without it and must set `broken` if it failed.
    if let Err(e) = init_ort(&exe_dir) {
        eprintln!("[native-synth] {e}");
        broken = true;
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
                    // style row = clamp(nTokens-2, 0, 509) (kokoro-js generate_from_ids; Apache-2.0).
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
                            Ok(r) => samples += r.pcm.len(),
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
                        session = Some(s);
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

        // Phonemize -> tokens, carrying each phoneme's source span through so the model's
        // durations have something to be laid against.
        let (phon, phon_spans) = phonemize_spans(&req.text);
        let (ids, id_spans) = tokenize_spans(&phon, &phon_spans, vocab_ref);
        if ids.len() <= 2 {
            // Empty/punctuation-only chunk: no audio and nothing to mark.
            let _ = req.reply.send(Some(Synthesized { pcm: Vec::new(), marks: Vec::new() }));
            continue;
        }

        // Kokoro's model accepts at most ~512 tokens (510 content + BOS/EOS); a longer
        // sequence fails the BERT `Expand` node ("invalid expand shape"). Chunks can be
        // several sentences (controls `chunk`), so a long chunk must be sub-split into
        // <=MAX_CONTENT_TOKENS windows — each wrapped in its own BOS/EOS — and their PCM
        // concatenated. A window boundary lands at a token seam (rare, brief).
        let content = &ids[1..ids.len() - 1];
        let content_spans = &id_spans[1..ids.len() - 1];
        let mut bytes: Vec<u8> = Vec::new();
        let mut failed = false;
        // Per-token source and audio length, accumulated across every window so the
        // aggregation sees one continuous chunk. A sub-split is a fact about the model's
        // token limit, not about the page, and a word straddling a window seam is still one
        // word — running totals here rather than per-window aggregation is what keeps it so.
        let mut unit_spans: Vec<Option<crate::text::Span>> = Vec::with_capacity(ids.len());
        let mut unit_samples: Vec<u32> = Vec::with_capacity(ids.len());
        // One window without durations forfeits the whole chunk's marks. Marking part of a
        // chunk would leave the rest of the page silent-but-highlighted at whatever the last
        // mark said, which reads as a stuck highlight rather than as an absent one.
        //
        // Starts optimistic rather than being read off a capability flag: the session either
        // declares `durations_frames` or it doesn't, `run_model` answers that question per
        // run from the session's own outputs, and a `None` here clears the chunk. So a host
        // that fell back to the stock graph needs no separate bookkeeping to reach the
        // interpolated path — it simply never produces frames.
        let mut timing = true;
        for (window, wspans) in
            content.chunks(MAX_CONTENT_TOKENS).zip(content_spans.chunks(MAX_CONTENT_TOKENS))
        {
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
                    Ok(r) => {
                        window_pcm = Some(r);
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
                Some(run) => {
                    // `frames` is indexed exactly like `wids` (BOS/EOS included), which is
                    // why `wspans` is padded the same way rather than zipped to `window`.
                    match run.frames {
                        Some(f) if timing => {
                            unit_spans.push(None); // BOS
                            unit_spans.extend_from_slice(wspans);
                            unit_spans.push(None); // EOS
                            unit_samples
                                .extend(f.iter().map(|&x| x.saturating_mul(SAMPLES_PER_FRAME as u32)));
                        }
                        _ => timing = false,
                    }
                    bytes.reserve(run.pcm.len() * 4);
                    for s in run.pcm {
                        bytes.extend_from_slice(&s.to_le_bytes());
                    }
                }
                None => {
                    failed = true;
                    break;
                }
            }
        }
        if failed {
            let _ = req.reply.send(None);
            continue;
        }

        // Durations -> per-word marks. Every step past here can only lose the timing, never
        // the audio: `marks` empty means "no timing for this chunk", which is exactly what a
        // stock model produces, so the transport needs no separate signal for the two.
        let marks = if timing && unit_spans.len() == unit_samples.len() {
            let src = req.text.as_bytes();
            let timed = crate::text::aggregate_spans(&unit_spans, &unit_samples, src);
            let offsets = crate::text::utf16_offsets(src);
            let m = WordMark::from_timed(&timed, &offsets);
            let chunk_u16 = offsets.last().copied().unwrap_or(0);
            let samples = (bytes.len() / 4) as u32;
            // Validated here, on the producing side, against the same rule the consumer
            // applies. A stream that fails its own invariants is a bug to see in this log
            // now, not a frame the engine rejects inside Kindle after the page went quiet.
            if WordMark::stream_is_valid(&m, 0, chunk_u16, samples) {
                m
            } else {
                eprintln!("[native-synth] {} marks failed validation — dropped", m.len());
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let _ = req.reply.send(Some(Synthesized { pcm: bytes, marks }));
    }
}

#[cfg(test)]
mod mark_tests {
    use super::*;

    fn mark(cs: u32, cl: u32, ss: u32, se: u32) -> WordMark {
        WordMark { char_start_utf16: cs, char_len_utf16: cl, sample_start: ss, sample_end: se }
    }

    // A chunk of 20 characters starting at 100, 1000 samples long.
    fn ok(marks: &[WordMark]) -> bool {
        WordMark::stream_is_valid(marks, 100, 20, 1000)
    }

    #[test]
    fn accepts_ordered_touching_marks() {
        assert!(ok(&[mark(100, 5, 0, 400), mark(105, 6, 400, 900)]));
        assert!(ok(&[])); // a chunk with nothing to mark is not an error
        assert!(ok(&[mark(119, 1, 999, 1000)])); // flush against both ends
    }

    #[test]
    fn rejects_spans_outside_the_chunk() {
        assert!(!ok(&[mark(99, 5, 0, 100)])); // starts before the chunk
        assert!(!ok(&[mark(118, 5, 0, 100)])); // runs past its end
        assert!(!ok(&[mark(100, 5, 0, 1001)])); // past the chunk's audio
        assert!(!ok(&[mark(100, 0, 0, 100)])); // empty character span
    }

    #[test]
    fn rejects_reversed_and_overlapping() {
        assert!(!ok(&[mark(100, 5, 400, 100)])); // sample span reversed
        assert!(!ok(&[mark(105, 5, 0, 400), mark(100, 5, 400, 500)])); // characters go back
        assert!(!ok(&[mark(100, 6, 0, 400), mark(105, 5, 400, 500)])); // characters overlap
        assert!(!ok(&[mark(100, 5, 0, 400), mark(105, 5, 300, 500)])); // samples overlap
    }

    #[test]
    fn from_timed_produces_a_stream_the_wire_accepts() {
        use crate::text::{aggregate_spans, utf16_offsets, Span};
        // A multi-byte character before the words, so a byte-counting conversion would
        // place every later mark one unit early.
        let src = "\u{00A3}5 costs 1,250 pounds".as_bytes();
        let spans = vec![
            None,
            Some(Span { start: 0, end: 3 }),   // "£5"
            Some(Span { start: 4, end: 9 }),   // "costs"
            Some(Span { start: 10, end: 11 }), // "1"     — split report
            Some(Span { start: 12, end: 15 }), // "250"   — of one word
            Some(Span { start: 16, end: 22 }), // "pounds"
            None,
        ];
        let samples = vec![100, 400, 300, 200, 300, 500, 100];
        let timed = aggregate_spans(&spans, &samples, src);
        let offsets = utf16_offsets(src);
        let marks = WordMark::from_timed(&timed, &offsets);

        assert_eq!(marks.len(), 4, "1,250 must be one mark");
        // "£5" is 3 bytes but 2 UTF-16 units, so a byte count would start it at 0 and give
        // it length 3. Chunk-relative, so the first word begins the chunk.
        assert_eq!(marks[0].char_start_utf16, 0);
        assert_eq!(marks[0].char_len_utf16, 2);
        // "costs" then starts one unit earlier than its byte offset would suggest.
        assert_eq!(marks[1].char_start_utf16, 3);
        // The merged mark spans the whole page word, including the comma.
        assert_eq!(marks[2].char_len_utf16, 5);

        // And the whole stream passes the shared wire rule for its chunk.
        let total: u32 = samples.iter().sum();
        assert!(WordMark::stream_is_valid(&marks, 0, 21, total));
    }

    #[test]
    fn rejects_overflow_and_unbounded_counts() {
        assert!(!ok(&[mark(u32::MAX, 2, 0, 100)])); // char_start + char_len overflows
        // More marks than the chunk has characters: a mark needs a character, so this
        // cannot be a real stream — and it is the bound that keeps the x86 engine's
        // allocation tied to what it actually asked for.
        let many: Vec<WordMark> = (0..21).map(|i| mark(100 + i, 1, i * 10, i * 10 + 10)).collect();
        assert!(!WordMark::stream_is_valid(&many, 100, 20, 1000));
    }
}
