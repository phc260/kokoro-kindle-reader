// Named-pipe server that bridges the SAPI engine (running inside Kindle) to
// WebGPU synthesis in the app's webview. The x86 KokoroSapi.dll connects to
// \\.\pipe\KokoroSapiSynth and speaks the protocol in
// kokoro-sapi/src/WorkerProtocol.h ('S' = synth, 'G' = gain, 'C' = chunk size,
// 'I' = info). Each synth request
// is relayed to the frontend (kokoro-js on WebGPU) via the `synth-request`
// event; the frontend returns raw f32 PCM through the `synth_result` command,
// which we write back over the pipe.
//
// While the app is running it owns the pipe, replacing the native worker.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::oneshot;

const PIPE_NAME: &str = r"\\.\pipe\KokoroSapiSynth";
const CMD_SYNTH: u8 = b'S';
const CMD_GAIN: u8 = b'G';
const CMD_CHUNK: u8 = b'C';
const CMD_INFO: u8 = b'I';
const SYNTH_ERROR: u32 = 0xFFFF_FFFF;
const MAX_TEXT: u32 = 1 << 20;
const SYNTH_TIMEOUT: Duration = Duration::from_secs(120);
// The gain query just reads localStorage in the webview, so it's quick; if the
// frontend doesn't answer promptly we fall back to unity rather than stall audio.
const GAIN_TIMEOUT: Duration = Duration::from_secs(2);
// Same idea for the per-chunk sentence count (kCmdChunk); fall back to the
// engine's own default of 4 if the frontend is slow/absent.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_CHUNK_SENTENCES: u32 = 4;

/// Correlates pipe requests with frontend responses. Shared (via Tauri state)
/// between the pipe-serving tasks and the `synth_result` command.
#[derive(Default)]
pub struct Bridge {
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Vec<u8>>>>,
    // Parallel map for `gain-request` round-trips (engine 'G' command). Separate
    // from `pending` because the reply is a single float, not a PCM buffer.
    gain_pending: Mutex<HashMap<u64, oneshot::Sender<f32>>>,
    // Parallel map for `chunk-request` round-trips (engine 'C' command): the
    // per-chunk sentence count, replied as a single u32.
    chunk_pending: Mutex<HashMap<u64, oneshot::Sender<u32>>>,
}

impl Bridge {
    fn register(&self) -> (u64, oneshot::Receiver<Vec<u8>>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        (id, rx)
    }
    fn fulfill(&self, id: u64, pcm: Vec<u8>) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(pcm);
        }
    }
    fn cancel(&self, id: u64) {
        self.pending.lock().unwrap().remove(&id);
    }
    fn register_gain(&self) -> (u64, oneshot::Receiver<f32>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.gain_pending.lock().unwrap().insert(id, tx);
        (id, rx)
    }
    fn fulfill_gain(&self, id: u64, gain: f32) {
        if let Some(tx) = self.gain_pending.lock().unwrap().remove(&id) {
            let _ = tx.send(gain);
        }
    }
    fn cancel_gain(&self, id: u64) {
        self.gain_pending.lock().unwrap().remove(&id);
    }
    fn register_chunk(&self) -> (u64, oneshot::Receiver<u32>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.chunk_pending.lock().unwrap().insert(id, tx);
        (id, rx)
    }
    fn fulfill_chunk(&self, id: u64, sentences: u32) {
        if let Some(tx) = self.chunk_pending.lock().unwrap().remove(&id) {
            let _ = tx.send(sentences);
        }
    }
    fn cancel_chunk(&self, id: u64) {
        self.chunk_pending.lock().unwrap().remove(&id);
    }
}

#[derive(Clone, Serialize)]
struct SynthRequest {
    id: u64,
    text: String,
    // Host's rate-derived speed multiplier (1 = host normal). The frontend owns
    // the narrator voice + the user's speed/gain (from localStorage) and folds
    // `rate` into the final synthesis speed — see bridge.ts / WorkerProtocol.h.
    rate: f32,
}

/// Backend → frontend: payload of the `gain-request` event; the webview replies
/// with the current "tts-gain" via the `gain_result` command, keyed by `id`.
#[derive(Clone, Serialize)]
struct GainRequest {
    id: u64,
}

/// Backend → frontend: payload of the `chunk-request` event; the webview replies
/// with the current "tts-chunk" via the `chunk_result` command, keyed by `id`.
#[derive(Clone, Serialize)]
struct ChunkRequest {
    id: u64,
}

/// Frontend → backend: raw little-endian f32 PCM (24 kHz mono) for request `id`.
#[tauri::command]
pub fn synth_result(app: AppHandle, id: u64, pcm: Vec<u8>) {
    app.state::<Arc<Bridge>>().fulfill(id, pcm);
}

/// Frontend → backend: answer to a `gain-request` (current "tts-gain").
#[tauri::command]
pub fn gain_result(app: AppHandle, id: u64, gain: f32) {
    app.state::<Arc<Bridge>>().fulfill_gain(id, gain);
}

/// Frontend → backend: answer to a `chunk-request` (current "tts-chunk").
#[tauri::command]
pub fn chunk_result(app: AppHandle, id: u64, sentences: u32) {
    app.state::<Arc<Bridge>>().fulfill_chunk(id, sentences);
}

/// Spawn the pipe server on the async runtime. Call once from `setup`.
pub fn start(app: AppHandle) {
    let bridge = app.state::<Arc<Bridge>>().inner().clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = serve_loop(app, bridge).await {
            eprintln!("[pipe] server stopped: {e}");
        }
    });
}

async fn serve_loop(app: AppHandle, bridge: Arc<Bridge>) -> std::io::Result<()> {
    let mut first = true;
    loop {
        // first_pipe_instance fails if another server already owns the name
        // (e.g. the native worker or a second app instance) — surfaced via `?`.
        let server = ServerOptions::new()
            .first_pipe_instance(first)
            .create(PIPE_NAME)?;
        first = false;
        server.connect().await?; // a client (the SAPI engine) connected
        let app = app.clone();
        let bridge = bridge.clone();
        tauri::async_runtime::spawn(async move {
            // EOF / broken pipe on disconnect is normal; ignore.
            let _ = serve_client(server, app, bridge).await;
        });
    }
}

async fn serve_client(
    mut pipe: NamedPipeServer,
    app: AppHandle,
    bridge: Arc<Bridge>,
) -> std::io::Result<()> {
    loop {
        let mut cmd = [0u8; 1];
        pipe.read_exact(&mut cmd).await?;
        match cmd[0] {
            CMD_INFO => {
                let json = br#"{"provider":"WebGPU(app)","voice":""}"#;
                pipe.write_all(&(json.len() as u16).to_le_bytes()).await?;
                pipe.write_all(json).await?;
            }
            CMD_GAIN => {
                // Ask the webview for the current gain; reply with one f32. Fall
                // back to unity (1.0) so a slow/absent frontend never stalls audio.
                let (id, rx) = bridge.register_gain();
                let _ = app.emit("gain-request", GainRequest { id });
                let gain = match tokio::time::timeout(GAIN_TIMEOUT, rx).await {
                    Ok(Ok(g)) => g,
                    _ => {
                        bridge.cancel_gain(id);
                        1.0f32
                    }
                };
                pipe.write_all(&gain.to_le_bytes()).await?;
            }
            CMD_CHUNK => {
                // Ask the webview for the per-chunk sentence count; reply with one
                // u32. Fall back to the default so a slow/absent frontend doesn't
                // stall the start of synthesis.
                let (id, rx) = bridge.register_chunk();
                let _ = app.emit("chunk-request", ChunkRequest { id });
                let sentences = match tokio::time::timeout(CHUNK_TIMEOUT, rx).await {
                    Ok(Ok(s)) => s,
                    _ => {
                        bridge.cancel_chunk(id);
                        DEFAULT_CHUNK_SENTENCES
                    }
                };
                pipe.write_all(&sentences.to_le_bytes()).await?;
            }
            CMD_SYNTH => {
                let mut b4 = [0u8; 4];
                pipe.read_exact(&mut b4).await?;
                let rate = f32::from_le_bytes(b4);
                pipe.read_exact(&mut b4).await?;
                let tlen = u32::from_le_bytes(b4);
                if tlen == 0 || tlen > MAX_TEXT {
                    return Ok(());
                }
                let mut tbuf = vec![0u8; tlen as usize];
                pipe.read_exact(&mut tbuf).await?;
                let text = String::from_utf8_lossy(&tbuf).into_owned();

                let (id, rx) = bridge.register();
                let _ = app.emit("synth-request", SynthRequest { id, text, rate });
                let pcm = match tokio::time::timeout(SYNTH_TIMEOUT, rx).await {
                    Ok(Ok(pcm)) => pcm,
                    _ => {
                        bridge.cancel(id);
                        pipe.write_all(&SYNTH_ERROR.to_le_bytes()).await?;
                        continue;
                    }
                };
                let n = (pcm.len() / 4) as u32; // bytes -> f32 sample count
                pipe.write_all(&n.to_le_bytes()).await?;
                if n > 0 {
                    pipe.write_all(&pcm).await?;
                }
            }
            _ => return Ok(()), // unknown command: drop the client
        }
    }
}
