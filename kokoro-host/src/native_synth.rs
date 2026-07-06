// Native Dawn WebGPU synthesis for the Kindle pipe path. pipe.rs calls this to
// synthesize each chunk natively so Kindle can be narrated.
//
// The C++ core (kokoro-worker/src, linked via build.rs) owns the ORT/WebGPU session
// + espeak. espeak keeps global state and a temp-file phoneme trace, so it is NOT
// thread-safe: all synthesis is serialized onto ONE dedicated worker thread that
// owns the KokoroWorker for the process lifetime. Requests arrive over an mpsc
// channel; each reply comes back on a tokio oneshot so the async pipe tasks await
// without blocking. Settings (narrator/speed/gain/chunk) come from controls.json in
// the app-data dir.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::mpsc;

use tokio::sync::oneshot;

#[repr(C)]
struct KokoroWorker {
    _private: [u8; 0],
}

extern "C" {
    fn kokoro_worker_create(
        model: *const u16,
        voice: *const u16,
        tokenizer: *const u16,
        espeak_data: *const c_char,
        errbuf: *mut c_char,
        errcap: c_int,
    ) -> *mut KokoroWorker;
    fn kokoro_worker_synth(
        w: *mut KokoroWorker,
        text: *const c_char,
        speed: f32,
        out_pcm: *mut *mut f32,
        errbuf: *mut c_char,
        errcap: c_int,
    ) -> i64;
    fn kokoro_worker_set_voice(
        w: *mut KokoroWorker,
        voice: *const u16,
        errbuf: *mut c_char,
        errcap: c_int,
    ) -> c_int;
    fn kokoro_worker_free(pcm: *mut f32);
    fn kokoro_worker_destroy(w: *mut KokoroWorker);
}

fn u16z(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

fn err_of(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf.iter().take_while(|&&b| b != 0).map(|&b| b as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The per-utterance settings the pipe host reads from controls.json (replacing the
/// webview's localStorage). `speed`/`gain` default to 1, `chunk` to 4 sentences.
#[derive(Clone, Copy)]
pub struct Controls {
    pub speed: f32,
    pub gain: f32,
    pub chunk: u32,
}

impl Default for Controls {
    fn default() -> Self {
        Controls { speed: 1.0, gain: 1.0, chunk: 4 }
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
        }
    }
    (voice, c)
}

struct Req {
    text: String,
    speed: f32,
    voice: String,
    reply: oneshot::Sender<Option<Vec<u8>>>,
}

/// Handle to the serialized native synth worker thread. Cloneable Sender inside.
#[derive(Clone)]
pub struct NativeSynth {
    tx: mpsc::Sender<Req>,
}

impl NativeSynth {
    /// Spawn the worker thread. `base` is the model dir (…/onnx-community/Kokoro-82M-
    /// v1.0-ONNX) holding onnx/model.onnx, tokenizer.json, voices/*.bin; `espeak_data`
    /// is the espeak-ng-data dir. The worker lazily inits the ORT/WebGPU session on
    /// the first request (so startup/model-download isn't blocked on it).
    pub fn spawn(base: PathBuf, espeak_data: PathBuf) -> NativeSynth {
        let (tx, rx) = mpsc::channel::<Req>();
        std::thread::Builder::new()
            .name("kokoro-native-synth".into())
            .spawn(move || worker_loop(rx, base, espeak_data))
            .expect("spawn native synth thread");
        NativeSynth { tx }
    }

    /// Synthesize one already-cut chunk. Returns raw little-endian f32 PCM bytes
    /// (24 kHz mono) — same shape the webview `synth_result` used, so pipe_server's
    /// framing is unchanged. None on init/synth failure (pipe host emits kSynthError).
    pub async fn synth(&self, text: String, speed: f32, voice: String) -> Option<Vec<u8>> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Req { text, speed, voice, reply }).is_err() {
            return None; // worker thread gone
        }
        rx.await.ok().flatten()
    }
}

fn voice_path(base: &Path, voice: &str) -> PathBuf {
    base.join("voices").join(format!("{voice}.bin"))
}

fn worker_loop(rx: mpsc::Receiver<Req>, base: PathBuf, espeak_data: PathBuf) {
    let model = base.join("onnx").join("model.onnx");
    let tokenizer = base.join("tokenizer.json");
    let espeak_c = CString::new(espeak_data.to_string_lossy().into_owned())
        .unwrap_or_else(|_| CString::new("").unwrap());

    let mut worker: *mut KokoroWorker = ptr::null_mut();
    let mut cur_voice = String::new();

    while let Ok(req) = rx.recv() {
        let mut err = [0 as c_char; 512];

        // Lazy init on first request (using its narrator), then narrator switches
        // via SetVoice (keeps the session).
        if worker.is_null() {
            let (mw, vw, tw) = (
                u16z(&model),
                u16z(&voice_path(&base, &req.voice)),
                u16z(&tokenizer),
            );
            worker = unsafe {
                kokoro_worker_create(
                    mw.as_ptr(),
                    vw.as_ptr(),
                    tw.as_ptr(),
                    espeak_c.as_ptr(),
                    err.as_mut_ptr(),
                    err.len() as c_int,
                )
            };
            if worker.is_null() {
                eprintln!("[native-synth] create failed: {}", err_of(&err));
                let _ = req.reply.send(None);
                continue;
            }
            cur_voice = req.voice.clone();
            eprintln!("[native-synth] KokoroSynth ready (ONNX + WebGPU), voice={cur_voice}");
        } else if req.voice != cur_voice {
            let vw = u16z(&voice_path(&base, &req.voice));
            let rc = unsafe {
                kokoro_worker_set_voice(worker, vw.as_ptr(), err.as_mut_ptr(), err.len() as c_int)
            };
            if rc == 0 {
                cur_voice = req.voice.clone();
            } else {
                eprintln!("[native-synth] set_voice({}) failed: {} (keeping {cur_voice})",
                    req.voice, err_of(&err));
            }
        }

        // Synthesize this chunk -> raw f32 LE bytes.
        let mut pcm_ptr: *mut f32 = ptr::null_mut();
        let n = unsafe {
            kokoro_worker_synth(
                worker,
                CString::new(req.text.as_str()).unwrap_or_default().as_ptr(),
                req.speed,
                &mut pcm_ptr,
                err.as_mut_ptr(),
                err.len() as c_int,
            )
        };
        let out = if n < 0 {
            eprintln!("[native-synth] synth failed: {}", err_of(&err));
            None
        } else if pcm_ptr.is_null() || n == 0 {
            Some(Vec::new()) // empty/punctuation-only chunk
        } else {
            let samples = unsafe { std::slice::from_raw_parts(pcm_ptr, n as usize) };
            let mut bytes = Vec::with_capacity(samples.len() * 4);
            for &s in samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            Some(bytes)
        };
        if !pcm_ptr.is_null() {
            unsafe { kokoro_worker_free(pcm_ptr) };
        }
        let _ = req.reply.send(out);
    }

    if !worker.is_null() {
        unsafe { kokoro_worker_destroy(worker) };
    }
}
