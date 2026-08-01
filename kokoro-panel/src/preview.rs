// Preview playback: connect to the host's SAPI pipe as a client, request synthesis
// of a short intro line (rate 1.0 — the host folds in the current controls.json
// voice/speed just like it does for Kindle), collect the streamed PCM applying the
// per-frame gain, and play it with rodio. Same engine + settings as Kindle, so
// Preview is truly WYSIWYG. Blocking; run it on a background thread.
//
// It asks with CMD_PREVIEW rather than CMD_SYNTH, and the difference matters twice. The
// host doesn't pace a preview (this collects the whole clip before playing a note of it, so
// pacing would only make it arrive as slowly as it speaks), and it doesn't count as Kindle
// audio — which is what lets the panel show "Speaking" for Kindle's narration without its
// own silent intro prefetch lighting the same indicator. That used to be a timing guess
// here; it is now a fact the host states.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

// Wire-format constants come from the shared `kokoro-protocol` crate (the same source
// kokoro-host/pipe.rs and the SAPI engine use), so Preview can't drift from them.
// CHUNK_INFO marks the per-chunk header the host emits before a chunk's audio:
// CHUNK_INFO then [u32 utf16Len][u32 nSamples]. Preview doesn't place SAPI events, so it
// just skips the 8-byte payload — but it MUST consume the marker or the next u32 read
// desyncs the whole stream.
use kokoro_protocol::{
    CHUNK_INFO, CMD_PREVIEW, MAX_FRAME_SAMPLES, SAMPLE_RATE, STREAM_END, SYNTH_ERROR,
};

use crate::hostlink::{connect, ACTION_BUSY_WAIT};

/// Ceiling on one whole preview clip, in samples. Two minutes of audio — the intro line is a
/// couple of seconds, so nothing the host would legitimately send comes near it, and a
/// squatter feeding frames forever hits it long before the panel's memory.
const MAX_PREVIEW_SAMPLES: usize = SAMPLE_RATE as usize * 120;

fn read_exact(pipe: &mut std::fs::File, n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    pipe.read_exact(&mut buf).map_err(|e| format!("pipe read: {e}"))?;
    Ok(buf)
}

/// Synthesize `text` through the pipe and return 24 kHz mono f32 samples (gain
/// already applied), or an error string. Public so the panel can pre-synthesize
/// a narrator intro into a buffer ahead of the user pressing Preview.
pub fn synth(text: &str) -> Result<Vec<f32>, String> {
    let mut pipe = connect(ACTION_BUSY_WAIT)?;

    // Request: ['P'][f32 rate=1.0][u32 textLen][utf8 text] — CMD_SYNTH's layout.
    let bytes = text.as_bytes();
    let mut req = Vec::with_capacity(9 + bytes.len());
    req.push(CMD_PREVIEW);
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
        // Every remaining `n` is a length this client is about to allocate `n * 4` bytes for,
        // so cap it exactly as the SAPI engine's reader does (`kokoro-sapi/src/worker.rs`).
        // `PIPE_NAME` is openable by any process running as this user, so a squatter that
        // owns it before the host starts can answer with a length of its choosing; without
        // this, `0xffff_fffc` is a multi-gigabyte allocation. The two readers of this frame
        // format must not disagree about what is too big.
        if n > MAX_FRAME_SAMPLES {
            return Err("the host sent an oversized audio frame.".to_string());
        }
        // Per-frame capping is not enough on its own *here*, because unlike the SAPI reader
        // this one keeps the whole clip: a squatter can stay under the frame cap forever and
        // still walk the panel out of memory one legal frame at a time. Bound the total too.
        if samples.len() + n as usize > MAX_PREVIEW_SAMPLES {
            return Err("the host sent an oversized preview clip.".to_string());
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
