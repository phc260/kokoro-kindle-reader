// Named-pipe server bridging the SAPI engine (running inside Kindle) to the native
// Dawn WebGPU synth. The x86 KokoroSapi.dll connects to \\.\pipe\KokoroSapiSynth
// and speaks the kokoro-protocol wire format ('S' = synth whole utterance); the
// settings panel connects to the same pipe for 'P' (preview synth), 'T' (status),
// 'B' (bench) and 'K' (Kindle control + the panel's heartbeat).
//
// 'K' is what makes this host the panel's sole authority for Kindle reading: the panel
// sends Play/Stop/Pause/Resume as intent and renders the state that comes back, and never
// looks at Kindle itself. Query, pause and resume are answered inline off shared atomics,
// so a heartbeat can't queue behind an in-flight synthesis, a benchmark, or the blocking UI
// Automation a Play is doing on the Kindle-control thread.
//
// The browser extension does NOT come through here — it goes over webserve.rs, which calls
// the synth worker directly.
//
// Only 'S' is a real-time sink, and only Kindle's SAPI engine sends it — so only 'S' gets
// the paced stream below. The rest are here precisely because their callers aren't sinks:
// 'B' times the model unpaced, 'P' hands back a whole clip the panel buffers before playing
// a note of it, and 'K' carries no audio at all. Pacing a caller that isn't consuming in
// real time clamps it to ~1.0x, which is why these are separate commands and not flags.
//
// This end owns all chunking: a single 'S' request carries the whole utterance; we
// split it into sentence chunks (crate::split_text), synthesize each on the
// serialized native worker with a depth-1 prefetch pipeline, and stream the PCM
// back to the engine as ~sub-frame-sized frames ([nSamples][gain][samples...], then
// a STREAM_END / SYNTH_ERROR marker), paced to ~real time. Narrator/speed/gain/chunk
// come from controls.json in the app-data dir (no webview round-trips).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::kindle_ctl::KindleCtl;
use crate::native_synth::{self, NativeSynth};
use crate::split_text::split_text;
use crate::state::HostState;
// The named-pipe wire format is shared with the SAPI engine (one source of truth).
use kokoro_protocol::{
    BENCH_BUSY, BENCH_ENGINE_CPU, BENCH_ENGINE_GPU, BENCH_FAILED, BENCH_OK, CHUNK_INFO, CMD_BENCH,
    CMD_KINDLE, CMD_PREVIEW, CMD_STATUS, CMD_SYNTH, KINDLE_CLOSE, KINDLE_ERR, KINDLE_OK,
    KINDLE_PAUSE, KINDLE_PLAY, KINDLE_QUERY, KINDLE_RESUME, KINDLE_STOP, MAX_MSG_BYTES,
    MAX_TEXT_BYTES, PIPE_NAME, STREAM_END, SYNTH_ERROR,
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
    /// The model dir (`<app_data>/<MODEL_ID>`), used to enumerate the narrators actually
    /// downloaded for `webserve`'s `/status` voice list. Kept beside `app_data` rather than
    /// re-derived here so `MODEL_ID` stays owned by one place (main.rs).
    pub model_base: PathBuf,
    pub native: NativeSynth,
    /// The host's live view of itself — audio clocks, pause, and what it believes Kindle is
    /// doing. Shared across all client tasks (the struct is cloned per connection but the
    /// `Arc` is one cell), so a `CMD_STATUS` / `CMD_KINDLE` query on the panel's connection
    /// sees audio streamed on Kindle's connection, with no handshake on the synth worker.
    pub state: Arc<HostState>,
    /// The serialized Kindle-control thread. The only route to Kindle's UI in the project.
    pub kindle: KindleCtl,
}

/// Clears the shared bench slot on the way out, so a client task that errors or is dropped
/// mid-measurement can't leave the speed test permanently refusing to run.
struct BenchGuard(Arc<HostState>);

impl Drop for BenchGuard {
    fn drop(&mut self) {
        self.0.set_bench_busy(false);
    }
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

/// Narrators actually present on disk (`<model_base>/voices/<id>.bin`), sorted. Enumerated
/// rather than read from model-manifest.json so the list is what can really be synthesized
/// right now — a half-downloaded model advertises only what it has, and a client's picker
/// never offers a voice whose .bin is missing.
pub fn available_voices(model_base: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(model_base.join("voices"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_suffix(".bin")
                .map(str::to_string)
        })
        .collect();
    v.sort();
    v
}

/// Current gain from controls.json ("gain"), read fresh per sub-frame so a volume
/// change lands within the playing chunk.
fn gain(ctx: &Ctx) -> f32 {
    native_synth::read_controls(&ctx.app_data).1.gain
}

/// A prefetched chunk synth plus the inputs it was rendered with, so the streaming
/// loop can tell whether a later controls change (narrator/speed) made it stale and
/// re-synthesize it — speed is baked into the PCM (it's a model input), so a slider
/// move can only land on a chunk not yet synthesized.
struct Prefetch {
    voice: String,
    speed: f32,
    handle: tokio::task::JoinHandle<Option<Vec<u8>>>,
}

/// Synthesize one already-cut chunk on the serialized native worker, as a detached
/// task so it overlaps the (backpressured) write of the previous chunk's frame —
/// the depth-1 prefetch. Narrator + speed come from controls.json (speed = host
/// `rate` × controls speed), returned alongside the handle so the loop can detect a
/// stale chunk. None on timeout/failure.
fn spawn_synth(ctx: &Ctx, text: String, rate: f32) -> Prefetch {
    let (voice, controls) = native_synth::read_controls(&ctx.app_data);
    let speed = rate * controls.speed;
    let engine = controls.engine;
    let native = ctx.native.clone();
    let voice2 = voice.clone();
    let handle = tokio::spawn(async move { native.synth(text, speed, voice2, engine).await });
    Prefetch { voice, speed, handle }
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
            CMD_STATUS => {
                // Milliseconds since we last wrote audio to any client (saturating to
                // u32::MAX, which also covers "never synthesized"). Runs on this client's
                // task, independent of any in-flight CMD_SYNTH.
                pipe.write_all(&ctx.state.ms_since_audio().to_le_bytes()).await?;
            }
            CMD_KINDLE => {
                let mut b1 = [0u8; 1];
                pipe.read_exact(&mut b1).await?;
                let (result, msg) = apply_kindle(&ctx, b1[0]).await;
                // Every action answers with the same layout, so a client parses one reply
                // shape however it asked. The state is read *after* the action so it's the
                // authoritative outcome, not the intent.
                let msg = msg.into_bytes();
                let msg = &msg[..msg.len().min(MAX_MSG_BYTES as usize)];
                pipe.write_all(&[result, ctx.state.state_flags()]).await?;
                pipe.write_all(&ctx.state.ms_since_audio().to_le_bytes()).await?;
                pipe.write_all(&ctx.state.ms_since_kindle_audio().to_le_bytes()).await?;
                pipe.write_all(&(msg.len() as u16).to_le_bytes()).await?;
                pipe.write_all(msg).await?;
            }
            CMD_BENCH => {
                // Time one engine so the panel can tell the user which is faster here.
                // Deliberately NOT the CMD_SYNTH path: that stream is paced to ~real
                // time, so timing it would measure the pacing. Runs on the serialized
                // synth worker — this task just waits, and the pipe server is
                // multi-instance, so other clients keep being served meanwhile.
                let mut b1 = [0u8; 1];
                pipe.read_exact(&mut b1).await?;
                // Strict: an unrecognized selector is a malformed request, not a hint to
                // pick an engine. Drop the client, as the unknown-command arm below does
                // — guessing would hand a future or hostile client an expensive run it
                // didn't ask for.
                let engine = match b1[0] {
                    BENCH_ENGINE_CPU => native_synth::Engine::Cpu,
                    BENCH_ENGINE_GPU => native_synth::Engine::Gpu,
                    _ => return Ok(()),
                };
                // One measurement at a time across all clients (see `bench_busy`). The
                // guard releases it however this arm exits, including a `?` on the reply.
                if ctx.state.bench_busy_swap(true) {
                    pipe.write_all(&BENCH_BUSY.to_le_bytes()).await?;
                    pipe.write_all(&0.0f32.to_le_bytes()).await?;
                    pipe.write_all(&0.0f32.to_le_bytes()).await?;
                    continue;
                }
                let _bench_guard = BenchGuard(ctx.state.clone());
                // The narrator comes from controls.json rather than the wire: any voice
                // times the same (it's one more model input), and taking the user's own
                // guarantees the .bin is present.
                let (voice, _c) = native_synth::read_controls(&ctx.app_data);
                let (status, audio, elapsed) = match ctx.native.bench(voice, engine).await {
                    Some(b) => (BENCH_OK, b.audio_secs, b.elapsed_secs),
                    None => (BENCH_FAILED, 0.0, 0.0),
                };
                pipe.write_all(&status.to_le_bytes()).await?;
                pipe.write_all(&audio.to_le_bytes()).await?;
                pipe.write_all(&elapsed.to_le_bytes()).await?;
            }
            // Same request layout and the same frame stream; `for_kindle` is what differs.
            // See `stream_synth`.
            which @ (CMD_SYNTH | CMD_PREVIEW) => {
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
                // Mark "a page is in flight for Kindle" for the whole stream, so a peer can
                // tell the silent head of a page (synthesis, seconds long) from an idle host.
                // Every audio clock still reads idle in there, because no audio exists yet.
                // Guard-scoped: this stream ends at any `?` below, and Kindle disconnecting
                // mid-page is the ordinary way it ends.
                let _synthing =
                    (which == CMD_SYNTH).then(|| ctx.state.enter_kindle_synth());
                stream_synth(&mut pipe, &ctx, rate, &text, which == CMD_SYNTH).await?;
            }
            _ => return Ok(()), // unknown command: drop the client
        }
    }
}

/// Apply one `CMD_KINDLE` action and return `(result, user-facing message)`. The caller
/// serializes the reply; the authoritative state is read from [`HostState`] afterwards.
///
/// Query, pause and resume are answered here on the connection's own task — they touch only
/// atomics, so a heartbeat stays prompt no matter what the synth worker or the
/// Kindle-control thread is doing. Play/Stop/Close hand off to that thread and await it.
async fn apply_kindle(ctx: &Ctx, action: u8) -> (u8, String) {
    match action {
        KINDLE_QUERY => {
            // Ask the control thread to take a fresh look at Kindle (rate-limited, and it
            // never blocks us): this is how a Read Aloud toggled inside Kindle reaches the
            // panel without the panel ever polling Kindle itself.
            ctx.kindle.request_refresh();
            (KINDLE_OK, String::new())
        }
        KINDLE_PLAY | KINDLE_STOP => {
            let want = action == KINDLE_PLAY;
            // Starting or stopping clears any pause already in effect, so reading never
            // begins stalled and stopping leaves nothing lingering. Cleared *before* the
            // command so a stream parked in the pause loop unblocks at once rather than
            // sitting there for the seconds of UI Automation this is about to take.
            ctx.state.set_paused(false);
            let res = ctx.kindle.set_reading(want).await;
            // And again after — unless this was a PLAY that actually started reading.
            //
            // The clear exists for Stop: pause is answered inline on any other connection, so
            // one arriving inside that window would outlive the Stop meant to end it, leaving
            // a stopped stream parked with no way back (the panel only offers Resume while it
            // believes reading is on).
            //
            // A *successful* Play must not clear it: that window is seconds of UI Automation
            // wide and the panel offers Pause throughout (its switch flipped optimistically),
            // so a Pause landing there is a real instruction the user watched be acknowledged.
            // Clearing it regardless acknowledged that Pause and then resumed underneath it.
            // Reading may therefore begin stalled — that is what was asked for, and Resume is
            // reachable because `reading` is now on.
            //
            // A *failed* Play is the Stop case wearing a Play's clothes: reading stays off, so
            // the panel shows Play and no Resume, and a pause left armed would strand the next
            // stream with nothing on screen able to release it.
            if !want || res.is_err() {
                ctx.state.set_paused(false);
            }
            match res {
                Ok(()) => (
                    KINDLE_OK,
                    if want { "Reading started in Kindle." } else { "Reading stopped." }
                        .to_string(),
                ),
                Err(e) => (KINDLE_ERR, e),
            }
        }
        KINDLE_PAUSE | KINDLE_RESUME => {
            let want = action == KINDLE_PAUSE;
            ctx.state.set_paused(want);
            (KINDLE_OK, if want { "Paused." } else { "Resumed." }.to_string())
        }
        KINDLE_CLOSE => match ctx.kindle.close_kindle().await {
            Ok(()) => (KINDLE_OK, "Kindle closed - reopen it to pick up the change.".to_string()),
            Err(e) => (KINDLE_ERR, e),
        },
        // Unknown action: same strictness as an unknown command. Guessing would let a
        // future or hostile client start Kindle reading by accident.
        _ => (KINDLE_ERR, "unsupported Kindle action.".to_string()),
    }
}

/// Synthesize a whole utterance and stream it back as the kokoro-protocol frame sequence.
///
/// `for_kindle` distinguishes the two callers, and it is the only difference:
///   * `CMD_SYNTH` (true) — the SAPI engine inside Kindle. A real-time sink, so the stream
///     is paced to ~real time, honours the live pause, and stamps the Kindle-audio clock
///     that "Kokoro is narrating" is read from.
///   * `CMD_PREVIEW` (false) — the settings panel. It buffers the whole clip before playing
///     a note of it, so pacing would only make the intro take as long to arrive as it does
///     to speak; and it must not register as Kindle narrating, or the panel's own silent
///     prefetch lights up every "speaking" readout in the app.
async fn stream_synth(
    pipe: &mut NamedPipeServer,
    ctx: &Ctx,
    rate: f32,
    text: &str,
    for_kindle: bool,
) -> std::io::Result<()> {
    // We own the chunking: split the whole utterance, synthesize each chunk, then stream
    // its PCM back as ~250 ms sub-frames ([nSamples][gain][samples...]).
    let (per_chunk, pacing_lead, subframe_samples) = stream_config(ctx);
    let chunks = split_text(text, per_chunk);
    if chunks.is_empty() {
        pipe.write_all(&STREAM_END.to_le_bytes()).await?;
        return Ok(());
    }

    // Depth-1 prefetch: synth chunk k+1 (detached) while we stream k. An abort shows up
    // here as a broken-pipe write error (`?`), unwinding the loop; the in-flight task is
    // dropped.
    let mut pending = Some(spawn_synth(ctx, chunks[0].clone(), rate));
    let mut failed = false;
    // Send-pacing clock (whole utterance): keep at most `pacing_lead` seconds of audio
    // ahead of real time. Starts on the first sub-frame.
    let mut clock: Option<Instant> = None;
    let mut samples_sent: u64 = 0;
    for k in 0..chunks.len() {
        let mut pf = pending.take().unwrap();
        // Freshness: if the narrator/speed changed since chunk k was prefetched, its PCM is
        // stale (speed is baked into synthesis) — abort it and re-synth at the current
        // settings so the change lands on this chunk instead of one or two chunks later.
        let (cur_voice, cur_ctrls) = native_synth::read_controls(&ctx.app_data);
        if pf.speed != rate * cur_ctrls.speed || pf.voice != cur_voice {
            pf.handle.abort();
            pf = spawn_synth(ctx, chunks[k].clone(), rate);
        }
        let pcm = pf.handle.await.ok().flatten();
        if k + 1 < chunks.len() {
            pending = Some(spawn_synth(ctx, chunks[k + 1].clone(), rate));
        }
        let pcm = match pcm {
            Some(pcm) => pcm,
            None => {
                failed = true;
                break;
            }
        };

        // Stream this chunk as sub-frames, each carrying a fresh gain (re-read ≈ when the
        // engine plays it, so a slider move isn't frozen into prefetched PCM).
        let total = pcm.len() / 4; // bytes -> f32 sample count

        // Chunk header: its UTF-16 length + sample count, so the engine can map
        // word/bookmark events to true audio offsets while streaming.
        let chunk_u16 = chunks[k].encode_utf16().count() as u32;
        pipe.write_all(&CHUNK_INFO.to_le_bytes()).await?;
        pipe.write_all(&chunk_u16.to_le_bytes()).await?;
        pipe.write_all(&(total as u32).to_le_bytes()).await?;

        let mut off = 0usize; // sample offset within the chunk
        while off < total {
            // Pause: while the host's `paused` flag is set, stall the stream — keep the
            // pipe open, send nothing — so playback pauses mid-page without Kindle deciding
            // the page is done and turning it (its narrator tolerates a long silent gap;
            // verified up to ~12 s). We re-check ~10x/s and hold the exact sample position.
            // If Kindle aborts during a pause we don't notice here, but the first real
            // write after resume fails and unwinds the stream.
            if for_kindle && ctx.state.paused() {
                let paused_at = Instant::now();
                while ctx.state.paused() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                // Don't count paused time against the pacing clock, or we'd blast a
                // catch-up burst on resume.
                if let Some(c) = clock.as_mut() {
                    *c += paused_at.elapsed();
                }
            }

            let n = subframe_samples.min(total - off);
            let g = gain(ctx);
            pipe.write_all(&(n as u32).to_le_bytes()).await?;
            pipe.write_all(&g.to_le_bytes()).await?;
            pipe.write_all(&pcm[off * 4..(off + n) * 4]).await?;
            // Stamp "audio just went out" so a peer's CMD_STATUS / CMD_KINDLE can tell what
            // Kokoro is doing. Also naturally reads idle while paused (the pause branch
            // above writes nothing).
            ctx.state.stamp_audio(for_kindle);
            off += n;

            // Pace: sleep if we're more than `pacing_lead` ahead of real time.
            // Self-correcting — if synthesis falls behind, `ahead` shrinks and we send
            // eagerly to catch up.
            samples_sent += n as u64;
            let clk = clock.get_or_insert_with(Instant::now);
            let ahead = samples_sent as f64 / SAMPLE_RATE - clk.elapsed().as_secs_f64();
            if for_kindle && ahead > pacing_lead {
                tokio::time::sleep(Duration::from_secs_f64(ahead - pacing_lead)).await;
            }
        }
    }
    let marker = if failed { SYNTH_ERROR } else { STREAM_END };
    pipe.write_all(&marker.to_le_bytes()).await?;
    Ok(())
}
