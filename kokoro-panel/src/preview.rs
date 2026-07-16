// Preview playback: connect to the host's SAPI pipe as a client, request synthesis
// of a short intro line (rate 1.0 — the host folds in the current controls.json
// voice/speed just like it does for Kindle), collect the streamed PCM applying the
// per-frame gain, and play it with rodio. Same engine + settings as Kindle, so
// Preview is truly WYSIWYG. Blocking; run it on a background thread.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Wire-format constants come from the shared `kokoro-protocol` crate (the same source
// kokoro-host/pipe.rs and the SAPI engine use), so Preview can't drift from them.
// CHUNK_INFO marks the per-chunk header the host emits before a chunk's audio:
// CHUNK_INFO then [u32 utf16Len][u32 nSamples]. Preview doesn't place SAPI events, so it
// just skips the 8-byte payload — but it MUST consume the marker or the next u32 read
// desyncs the whole stream.
use kokoro_protocol::{
    CHUNK_INFO, CMD_STATUS, CMD_SYNTH, PIPE_NAME, SAMPLE_RATE, STREAM_END, SYNTH_ERROR,
};

// ERROR_PIPE_BUSY: all pipe instances are momentarily in use; wait and retry. (A
// Windows error code, not part of the wire protocol, so it stays local.)
const ERROR_PIPE_BUSY: i32 = 231;

// How recently the host must have written audio for us to call it "speaking". Kindle
// sends one CMD_SYNTH per page, so CMD_STATUS dips between pages but not to zero — this
// grace window bridges the page gap so the indicator doesn't flicker off mid-read.
const SPEAKING_DEBOUNCE_MS: u32 = 1500;

// This panel's OWN synths (Preview + the silent narrator-intro prefetch) also stream
// audio from the host, so they stamp the very same clock CMD_STATUS reports on — the
// host can't tell them apart from Kindle. We record when we last synthesized so the
// "speaking" reading can discount our own prefetch (which plays nothing). The window
// covers the host's debounce plus slack so a stamp we caused can't linger as "speaking".
const SELF_SYNTH_GRACE_MS: u64 = SPEAKING_DEBOUNCE_MS as u64 + 500;
static LAST_SELF_SYNTH_MS: AtomicU64 = AtomicU64::new(0);

// Whether a self-synth is in flight right now. A cold first synth (model/WebGPU
// warm-up) can run well past SELF_SYNTH_GRACE_MS, so the time-window check alone
// isn't enough to cover its whole duration — this flag closes that gap regardless
// of how long the synth takes.
static SELF_SYNTH_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Marks a self-synth as active for its lifetime; stamps the grace-window clock on drop
/// (covering the host's post-synth debounce tail) whether the synth succeeded or errored.
struct SelfSynthGuard;

impl SelfSynthGuard {
    fn start() -> Self {
        SELF_SYNTH_ACTIVE.store(true, Ordering::Relaxed);
        SelfSynthGuard
    }
}

impl Drop for SelfSynthGuard {
    fn drop(&mut self) {
        LAST_SELF_SYNTH_MS.store(now_ms(), Ordering::Relaxed);
        SELF_SYNTH_ACTIVE.store(false, Ordering::Relaxed);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Connect to the pipe, retrying briefly while all instances are busy.
fn connect() -> Result<std::fs::File, String> {
    for _ in 0..20 {
        match OpenOptions::new().read(true).write(true).open(PIPE_NAME) {
            Ok(f) => return Ok(f),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(format!(
                    "can't reach the synthesis host ({e}). Is Kokoro Kindle Reader running?"
                ))
            }
        }
    }
    Err("the synthesis host is busy — try again.".to_string())
}

fn read_exact(pipe: &mut std::fs::File, n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    pipe.read_exact(&mut buf).map_err(|e| format!("pipe read: {e}"))?;
    Ok(buf)
}

/// Synthesize `text` through the pipe and return 24 kHz mono f32 samples (gain
/// already applied), or an error string. Public so the panel can pre-synthesize
/// a narrator intro into a buffer ahead of the user pressing Preview.
pub fn synth(text: &str) -> Result<Vec<f32>, String> {
    // Mark that WE are about to make the host stream audio, so a status poll that lands
    // while this runs won't mistake our own synth for Kindle. Held for the whole call
    // (guard drops on every return path, success or `?`), so a slow cold-start synth
    // stays covered instead of only the first SELF_SYNTH_GRACE_MS.
    let _guard = SelfSynthGuard::start();
    let mut pipe = connect()?;

    // Request: [0x53][f32 rate=1.0][u32 textLen][utf8 text].
    let bytes = text.as_bytes();
    let mut req = Vec::with_capacity(9 + bytes.len());
    req.push(CMD_SYNTH);
    req.extend_from_slice(&1.0f32.to_le_bytes());
    req.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    req.extend_from_slice(bytes);
    pipe.write_all(&req).map_err(|e| format!("pipe write: {e}"))?;
    pipe.flush().map_err(|e| format!("pipe flush: {e}"))?;

    // Frames: [u32 n]([f32 gain][f32*n]) until STREAM_END / SYNTH_ERROR.
    let mut samples: Vec<f32> = Vec::new();
    loop {
        let n = u32::from_le_bytes(read_exact(&mut pipe, 4)?.try_into().unwrap());
        if n == STREAM_END {
            break;
        }
        if n == SYNTH_ERROR {
            return Err("the host reported a synthesis error.".to_string());
        }
        if n == CHUNK_INFO {
            let _ = read_exact(&mut pipe, 8)?; // [u32 utf16Len][u32 nSamples] — unused here
            continue;
        }
        let gain = f32::from_le_bytes(read_exact(&mut pipe, 4)?.try_into().unwrap());
        let pcm = read_exact(&mut pipe, n as usize * 4)?;
        samples.reserve(n as usize);
        for s in pcm.chunks_exact(4) {
            let v = f32::from_le_bytes([s[0], s[1], s[2], s[3]]) * gain;
            samples.push(v);
        }
    }
    Ok(samples)
}

/// True if this panel ran its own synth (Preview or the intro prefetch) recently enough
/// that the host would still report "speaking" from it. The poll uses this to discount
/// our own buffer-warming prefetch — which streams from the host but plays nothing — so
/// it never lights the indicator. (Preview *playback* is shown via the `previewing`
/// property instead, so suppressing our synth here doesn't hide a real Preview.)
pub fn self_synth_recent() -> bool {
    SELF_SYNTH_ACTIVE.load(Ordering::Relaxed)
        || now_ms().saturating_sub(LAST_SELF_SYNTH_MS.load(Ordering::Relaxed)) < SELF_SYNTH_GRACE_MS
}

/// Ask the host whether Kokoro is producing audio right now. `Some(true)` if the host
/// wrote audio within `SPEAKING_DEBOUNCE_MS`, `Some(false)` if it's reachable but idle,
/// `None` if it can't be reached (host not running). Cheap and non-blocking: a single
/// connect attempt (no busy-retry) plus a 1-byte request and 4-byte reply, so it's safe
/// to call on a status-poll tick. The pipe server is multi-instance, so this answers
/// even while Kindle is mid-page on its own connection.
pub fn host_speaking() -> Option<bool> {
    let mut pipe = OpenOptions::new().read(true).write(true).open(PIPE_NAME).ok()?;
    pipe.write_all(&[CMD_STATUS]).ok()?;
    pipe.flush().ok()?;
    let mut b4 = [0u8; 4];
    pipe.read_exact(&mut b4).ok()?;
    Some(u32::from_le_bytes(b4) < SPEAKING_DEBOUNCE_MS)
}

/// Handle to the preview sink that's currently playing (if any), shared between the
/// playback thread and the Stop button. `stop()` takes the sink out and halts it,
/// which unblocks the playing thread's `sleep_until_end`.
pub type ActiveSink = Arc<Mutex<Option<Arc<rodio::Sink>>>>;

/// A fresh, idle handle for the app to own for the session.
pub fn new_active() -> ActiveSink {
    Arc::new(Mutex::new(None))
}

/// Play already-synthesized 24 kHz mono samples to completion. Blocking. Lets the
/// panel play a pre-synthesized intro straight from its buffer (no pipe round-trip).
/// Publishes the sink into `active` while it plays so the Stop button can halt it.
pub fn play_samples(samples: Vec<f32>, active: &ActiveSink) -> Result<(), String> {
    if samples.is_empty() {
        return Ok(());
    }
    let (_stream, handle) =
        rodio::OutputStream::try_default().map_err(|e| format!("audio device: {e}"))?;
    let sink = Arc::new(rodio::Sink::try_new(&handle).map_err(|e| format!("audio sink: {e}"))?);
    sink.append(rodio::buffer::SamplesBuffer::new(1, SAMPLE_RATE, samples));
    *active.lock().unwrap() = Some(sink.clone());
    sink.sleep_until_end(); // keep _stream alive until playback finishes (or Stop)
    // Clear the slot, but only if a newer preview hasn't already replaced us.
    let mut guard = active.lock().unwrap();
    if guard.as_ref().is_some_and(|s| Arc::ptr_eq(s, &sink)) {
        *guard = None;
    }
    Ok(())
}

/// Synthesize `text` via the pipe and play it to completion. Blocking.
pub fn play(text: &str, active: &ActiveSink) -> Result<(), String> {
    play_samples(synth(text)?, active)
}

/// Stop the preview that's currently playing, if any. Returns immediately; the
/// playing thread wakes from `sleep_until_end` and clears the `previewing` flag.
pub fn stop(active: &ActiveSink) {
    if let Some(sink) = active.lock().unwrap().take() {
        sink.stop();
    }
}
