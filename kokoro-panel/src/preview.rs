// Preview playback: connect to the host's SAPI pipe as a client, request synthesis
// of a short intro line (rate 1.0 — the host folds in the current controls.json
// voice/speed just like it does for Kindle), collect the streamed PCM applying the
// per-frame gain, and play it with rodio. Same engine + settings as Kindle, so
// Preview is truly WYSIWYG. Blocking; run it on a background thread.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::time::Duration;

const PIPE_NAME: &str = r"\\.\pipe\KokoroSapiSynth";
const CMD_SYNTH: u8 = b'S';
const STREAM_END: u32 = 0xFFFF_FFFE;
const SYNTH_ERROR: u32 = 0xFFFF_FFFF;
const SAMPLE_RATE: u32 = 24_000;
// ERROR_PIPE_BUSY: all pipe instances are momentarily in use; wait and retry.
const ERROR_PIPE_BUSY: i32 = 231;

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
/// already applied), or an error string.
fn synth(text: &str) -> Result<Vec<f32>, String> {
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

/// Synthesize `text` via the pipe and play it to completion. Blocking.
pub fn play(text: &str) -> Result<(), String> {
    let samples = synth(text)?;
    if samples.is_empty() {
        return Ok(());
    }
    let (_stream, handle) =
        rodio::OutputStream::try_default().map_err(|e| format!("audio device: {e}"))?;
    let sink = rodio::Sink::try_new(&handle).map_err(|e| format!("audio sink: {e}"))?;
    sink.append(rodio::buffer::SamplesBuffer::new(1, SAMPLE_RATE, samples));
    sink.sleep_until_end(); // keep _stream alive until playback finishes
    Ok(())
}
