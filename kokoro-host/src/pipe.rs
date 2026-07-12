// Named-pipe server bridging the SAPI engine (running inside Kindle) to the native
// Dawn WebGPU synth. The x86 KokoroSapi.dll connects to \\.\pipe\KokoroSapiSynth
// and speaks the kokoro-protocol wire format ('S' = synth whole utterance, 'I' =
// info).
//
// This end owns all chunking: a single 'S' request carries the whole utterance; we
// split it into sentence chunks (crate::split_text), synthesize each on the
// serialized native worker with a depth-1 prefetch pipeline, and stream the PCM
// back to the engine as ~sub-frame-sized frames ([nSamples][gain][samples...], then
// a STREAM_END / SYNTH_ERROR marker), paced to ~real time. Narrator/speed/gain/chunk
// come from controls.json in the app-data dir (no webview round-trips).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::native_synth::{self, NativeSynth};
use crate::split_text::split_text;
// The named-pipe wire format is shared with the SAPI engine (one source of truth).
use kokoro_protocol::{
    CHUNK_INFO, CMD_INFO, CMD_SYNTH, MAX_TEXT_BYTES, PIPE_NAME, STREAM_END, SYNTH_ERROR,
};

// Default send-pacing lead (ms): keep at most this much audio ahead of real time so
// SAPI doesn't buffer a whole chunk of gain-baked PCM ahead of the speaker — a
// volume/gain change then lands within ~this long. controls.json doesn't carry the
// lead / sub-frame knobs, so the native host always uses these defaults.
const DEFAULT_LEAD_MS: u32 = 500;
// Default sub-frame size (ms): each chunk's PCM is sliced this fine, and gain is
// re-read once per sub-frame. Smaller = finer volume granularity, more round-trips.
const DEFAULT_SUBFRAME_MS: u32 = 250;
// Kokoro's output rate (mono f32) as f64, to convert the ms knobs above to
// samples/seconds (the wire rate itself lives in kokoro_protocol::SAMPLE_RATE).
const SAMPLE_RATE: f64 = kokoro_protocol::SAMPLE_RATE as f64;

/// Everything the pipe path needs: where controls.json lives and the serialized
/// native synth worker.
#[derive(Clone)]
pub struct Ctx {
    pub app_data: PathBuf,
    pub native: NativeSynth,
}

/// Per-chunk sentence count from controls.json ("chunk"); pacing lead / sub-frame
/// size use the built-in defaults. Returns (sentences 1..=8, lead seconds, sub-frame
/// samples).
fn stream_config(ctx: &Ctx) -> (usize, f64, usize) {
    // chunk defaults to 4 sentences inside read_controls (Controls::default).
    let (_voice, c) = native_synth::read_controls(&ctx.app_data);
    let sentences = (c.chunk as usize).clamp(1, 8);
    let lead_secs = DEFAULT_LEAD_MS as f64 / 1000.0;
    let subframe_samples = (DEFAULT_SUBFRAME_MS as f64 * SAMPLE_RATE / 1000.0) as usize;
    (sentences, lead_secs, subframe_samples)
}

/// Current gain from controls.json ("gain"), read fresh per sub-frame so a volume
/// change lands within the playing chunk.
fn gain(ctx: &Ctx) -> f32 {
    native_synth::read_controls(&ctx.app_data).1.gain
}

/// Synthesize one already-cut chunk on the serialized native worker, as a detached
/// task so it overlaps the (backpressured) write of the previous chunk's frame —
/// the depth-1 prefetch. Narrator + speed come from controls.json (speed = host
/// `rate` × controls speed). None on timeout/failure.
fn spawn_synth(ctx: &Ctx, text: String, rate: f32) -> tokio::task::JoinHandle<Option<Vec<u8>>> {
    let (voice, controls) = native_synth::read_controls(&ctx.app_data);
    let speed = rate * controls.speed;
    let native = ctx.native.clone();
    tokio::spawn(async move { native.synth(text, speed, voice).await })
}

/// Serve the pipe forever. Returns only on a fatal pipe error (e.g. another server
/// already owns the name); the caller decides whether to retry.
pub async fn serve_loop(ctx: Ctx) -> std::io::Result<()> {
    let mut first = true;
    loop {
        // first_pipe_instance fails if another server already owns the name (e.g.
        // a second host instance) — surfaced via `?`.
        let server = ServerOptions::new()
            .first_pipe_instance(first)
            .create(PIPE_NAME)?;
        first = false;
        server.connect().await?; // a client (the SAPI engine) connected
        let ctx = ctx.clone();
        tokio::spawn(async move {
            // EOF / broken pipe on disconnect is normal; ignore.
            let _ = serve_client(server, ctx).await;
        });
    }
}

async fn serve_client(mut pipe: NamedPipeServer, ctx: Ctx) -> std::io::Result<()> {
    loop {
        let mut cmd = [0u8; 1];
        pipe.read_exact(&mut cmd).await?;
        match cmd[0] {
            CMD_INFO => {
                let json = br#"{"provider":"WebGPU(native)","voice":""}"#;
                pipe.write_all(&(json.len() as u16).to_le_bytes()).await?;
                pipe.write_all(json).await?;
            }
            CMD_SYNTH => {
                let mut b4 = [0u8; 4];
                pipe.read_exact(&mut b4).await?;
                let rate = f32::from_le_bytes(b4);
                pipe.read_exact(&mut b4).await?;
                let tlen = u32::from_le_bytes(b4);
                if tlen == 0 || tlen > MAX_TEXT_BYTES {
                    return Ok(());
                }
                let mut tbuf = vec![0u8; tlen as usize];
                pipe.read_exact(&mut tbuf).await?;
                let text = String::from_utf8_lossy(&tbuf).into_owned();

                // We own the chunking: split the whole utterance, synthesize each
                // chunk, then stream its PCM back as ~250 ms sub-frames
                // ([nSamples][gain][samples...]), paced to ~real time.
                let (per_chunk, pacing_lead, subframe_samples) = stream_config(&ctx);
                let chunks = split_text(&text, per_chunk);
                if chunks.is_empty() {
                    pipe.write_all(&STREAM_END.to_le_bytes()).await?;
                    continue;
                }

                // Depth-1 prefetch: synth chunk k+1 (detached) while we stream k. An
                // abort shows up here as a broken-pipe write error (`?`), unwinding
                // the loop; the in-flight task is dropped.
                let mut pending = Some(spawn_synth(&ctx, chunks[0].clone(), rate));
                let mut failed = false;
                // Send-pacing clock (whole utterance): keep at most `pacing_lead`
                // seconds of audio ahead of real time. Starts on the first sub-frame.
                let mut clock: Option<Instant> = None;
                let mut samples_sent: u64 = 0;
                for k in 0..chunks.len() {
                    let pcm = pending.take().unwrap().await.ok().flatten();
                    if k + 1 < chunks.len() {
                        pending = Some(spawn_synth(&ctx, chunks[k + 1].clone(), rate));
                    }
                    let pcm = match pcm {
                        Some(pcm) => pcm,
                        None => {
                            failed = true;
                            break;
                        }
                    };

                    // Stream this chunk as paced sub-frames, each carrying a fresh
                    // gain (re-read ≈ when the engine plays it, so a slider move
                    // isn't frozen into prefetched PCM).
                    let total = pcm.len() / 4; // bytes -> f32 sample count

                    // Chunk header: its UTF-16 length + sample count, so the engine can
                    // map word/bookmark events to true audio offsets while streaming.
                    let chunk_u16 = chunks[k].encode_utf16().count() as u32;
                    pipe.write_all(&CHUNK_INFO.to_le_bytes()).await?;
                    pipe.write_all(&chunk_u16.to_le_bytes()).await?;
                    pipe.write_all(&(total as u32).to_le_bytes()).await?;

                    let mut off = 0usize; // sample offset within the chunk
                    while off < total {
                        let n = subframe_samples.min(total - off);
                        let g = gain(&ctx);
                        pipe.write_all(&(n as u32).to_le_bytes()).await?;
                        pipe.write_all(&g.to_le_bytes()).await?;
                        pipe.write_all(&pcm[off * 4..(off + n) * 4]).await?;
                        off += n;

                        // Pace: sleep if we're more than `pacing_lead` ahead of real
                        // time. Self-correcting — if synthesis falls behind, `ahead`
                        // shrinks and we send eagerly to catch up.
                        samples_sent += n as u64;
                        let clk = clock.get_or_insert_with(Instant::now);
                        let ahead =
                            samples_sent as f64 / SAMPLE_RATE - clk.elapsed().as_secs_f64();
                        if ahead > pacing_lead {
                            tokio::time::sleep(Duration::from_secs_f64(ahead - pacing_lead))
                                .await;
                        }
                    }
                }
                let marker = if failed { SYNTH_ERROR } else { STREAM_END };
                pipe.write_all(&marker.to_le_bytes()).await?;
            }
            _ => return Ok(()), // unknown command: drop the client
        }
    }
}
