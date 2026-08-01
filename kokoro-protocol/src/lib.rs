//! The named-pipe wire protocol between clients (the x86 SAPI engine) and the x64
//! synthesis host (`kokoro-host`). One canonical source of truth, shared by both ends
//! so the format can't drift — replacing the old `WorkerProtocol.h` ⇆ `pipe.rs`
//! "change it in both places" duplication.
//!
//! Every request starts with a one-byte command:
//!
//! - [`CMD_SYNTH`] (`'S'`): synth the whole utterance.
//!   - request:  `[u8 'S'][f32 rate][u32 textBytes][utf8 text]`
//!   - response: a STREAM of frames. Each synthesized chunk begins with a
//!     [`CHUNK_INFO`] marker + `[u32 utf16Len][u32 nSamples]`, then that chunk's audio
//!     sub-frames `[u32 nSamples][f32 gain][f32 samples...]` (24 kHz mono, [-1, 1]). The
//!     stream ends with a marker whose leading u32 is [`STREAM_END`] (complete) or
//!     [`SYNTH_ERROR`] (a chunk failed). `rate` is the host's rate-derived speed
//!     multiplier; the host owns the narrator + folds in the user's own speed, so
//!     those don't cross the wire. `gain` (the user's volume, fresh per chunk) rides
//!     along in each frame and the engine applies it when converting to int16.
//!
//! - [`CMD_BENCH`] (`'B'`): `[u8 engine] -> [u32 status][f32 audioSecs][f32 elapsedSecs]`.
//!   Times one execution provider ([`BENCH_ENGINE_CPU`] / [`BENCH_ENGINE_GPU`]) on a fixed
//!   sample the *host* owns, so the answer can't be skewed by what a client sends and the
//!   work is bounded. `status` is [`BENCH_OK`] or [`BENCH_FAILED`] (the other two fields
//!   are then 0). The caller's figure of merit is `audioSecs / elapsedSecs` — how many
//!   seconds of speech the machine renders per second of wall clock, so `> 1` keeps up
//!   with reading. This exists *because* [`CMD_SYNTH`] can't answer the question: that
//!   stream is deliberately paced to ~real time, so timing it measures the pacing, not the
//!   engine. Runs on the serialized synth worker (queues behind an in-flight utterance)
//!   and takes tens of seconds; it writes no audio, so it doesn't disturb [`CMD_STATUS`].
//!
//! - [`CMD_STATUS`] (`'T'`): `-> [u32 msSinceLastAudio]`. Milliseconds since the host
//!   last wrote audio to *any* client, or [`u32::MAX`] if it never has. Answered inline
//!   (not on the serialized synth worker), so it returns immediately even while another
//!   client is mid-utterance — the pipe server is multi-instance, so a status query and
//!   an in-flight `CMD_SYNTH` ride separate connections. Lets a peer (the settings panel)
//!   tell whether Kokoro is *currently* producing audio; the caller applies its own
//!   debounce, since a reader like Kindle sends one `CMD_SYNTH` per page and this value
//!   dips between pages rather than going fully idle.
//!
//! - [`CMD_PREVIEW`] (`'P'`): the settings panel's Preview/intro synth. Byte-for-byte the
//!   same request and response as [`CMD_SYNTH`], with two differences the *host* applies:
//!   the stream is **unpaced** (the panel buffers the whole clip and then plays it, so it
//!   isn't a real-time sink — pacing it would only make the intro arrive at 1.0x), and it
//!   does **not** stamp the Kindle-audio clock. That second one is the point: without a
//!   command of its own, a panel's own silent prefetch is indistinguishable from Kindle
//!   narrating, and every consumer of "is Kokoro speaking?" has to guess.
//!
//! - [`CMD_KINDLE`] (`'K'`): `[u8 action] -> [u8 result][u8 state][u32 msSinceAudio]
//!   [u32 msSinceKindleAudio][u16 msgLen][utf8 msg]`. The host is the only process that
//!   touches Kindle; a client (the settings panel) sends *intent* and reads back the
//!   authoritative result. [`KINDLE_QUERY`] has no side effect and is answered inline from
//!   cached state — that makes it the panel's ~1 Hz **heartbeat**, and an unreachable pipe
//!   (rather than any reply value) is what "host offline" means. [`KINDLE_PLAY`] /
//!   [`KINDLE_STOP`] / [`KINDLE_CLOSE`] run on the host's serialized Kindle-control thread
//!   (blocking UI Automation, off both the async pipe runtime and the synth worker), so
//!   they take a moment; [`KINDLE_PAUSE`] / [`KINDLE_RESUME`] flip host-owned live state
//!   and return at once. `state` is the [`STATE_READING`] … [`STATE_BUSY`] bit set, and
//!   `msg` is a user-facing sentence (empty when there's nothing to say).

#![no_std]

/// The pipe the host serves and clients connect to.
pub const PIPE_NAME: &str = r"\\.\pipe\KokoroSapiSynth";

/// Command byte: synthesize the whole utterance.
pub const CMD_SYNTH: u8 = b'S';
/// Command byte: report `[u32 msSinceLastAudio]` — how long since the host last wrote
/// audio to any client (`u32::MAX` if never). See the module docs.
pub const CMD_STATUS: u8 = b'T';
/// Command byte: time one execution provider on the host's fixed sample.
/// `[u8 engine] -> [u32 status][f32 audioSecs][f32 elapsedSecs]`. See the module docs.
pub const CMD_BENCH: u8 = b'B';
/// Command byte: synthesize the whole utterance for the settings panel's Preview —
/// [`CMD_SYNTH`]'s request/response, unpaced, and not counted as Kindle audio. See the
/// module docs.
pub const CMD_PREVIEW: u8 = b'P';
/// Command byte: Kindle reading control + host state. `[u8 action] -> [u8 result]
/// [u8 state][u32 msSinceAudio][u32 msSinceKindleAudio][u16 msgLen][utf8 msg]`. See the
/// module docs.
pub const CMD_KINDLE: u8 = b'K';

/// [`CMD_KINDLE`] action: report state only — no side effect, answered inline from the
/// host's cached view. This is the panel's heartbeat.
pub const KINDLE_QUERY: u8 = 0;
/// [`CMD_KINDLE`] action: start Kindle's Read Aloud (and clear any pause).
pub const KINDLE_PLAY: u8 = 1;
/// [`CMD_KINDLE`] action: stop Kindle's Read Aloud (and clear any pause).
pub const KINDLE_STOP: u8 = 2;
/// [`CMD_KINDLE`] action: stall the audio stream mid-page, leaving Kindle on the page.
pub const KINDLE_PAUSE: u8 = 3;
/// [`CMD_KINDLE`] action: release a [`KINDLE_PAUSE`] and resume at the held sample.
pub const KINDLE_RESUME: u8 = 4;
/// [`CMD_KINDLE`] action: ask Kindle to close (`WM_CLOSE`, so it saves/prompts as usual).
/// The `kindle_kokoro` hook only lands on Kindle's *next* launch, so the panel offers this
/// when the user flips that setting.
pub const KINDLE_CLOSE: u8 = 5;

/// [`CMD_KINDLE`] result: the action was applied; `state` is authoritative.
pub const KINDLE_OK: u8 = 0;
/// [`CMD_KINDLE`] result: the action could not be applied (Kindle absent, UI Automation
/// failed). `msg` says why, and `state` still reports the host's current view.
pub const KINDLE_ERR: u8 = 1;

/// [`CMD_KINDLE`] state bit: the host believes Kindle's Read Aloud is on.
pub const STATE_READING: u8 = 1 << 0;
/// [`CMD_KINDLE`] state bit: playback is stalled mid-page.
pub const STATE_PAUSED: u8 = 1 << 1;
/// [`CMD_KINDLE`] state bit: a `Kindle.exe` process is running.
pub const STATE_KINDLE_RUNNING: u8 = 1 << 2;
/// [`CMD_KINDLE`] state bit: a reading command is in flight on the host's Kindle-control
/// thread. A client should leave its own switch alone until this clears, or it will fight
/// the transition it just asked for.
pub const STATE_BUSY: u8 = 1 << 3;
/// [`CMD_KINDLE`] state bit: a [`CMD_SYNTH`] stream is open for Kindle right now — the host
/// has been asked for a page and is synthesizing or streaming it.
///
/// Distinct from "speaking", which is derived from [`SPEAKING_DEBOUNCE_MS`] and therefore
/// cannot be true until audio has actually gone out. Between Kindle's narrator asking for a
/// page and the first frame coming back there are seconds of synthesis in which the host is
/// plainly working and every audio clock still reads idle. This bit covers exactly that gap.
///
/// It stays set for the whole page, not just the silent head of it, so it is a poor thing to
/// gate a Stop control on — see the note on [`STATE_BUSY`] for the flag that means "a command
/// is in flight".
pub const STATE_KINDLE_SYNTH: u8 = 1 << 4;

/// Sanity cap on a [`CMD_KINDLE`] reply's message (bytes) — bounds what the client
/// allocates off a length it read from the pipe.
pub const MAX_MSG_BYTES: u16 = 1024;

/// How recently the host must have written audio for a caller to call it "speaking".
/// Kindle sends one [`CMD_SYNTH`] per page, so the elapsed-ms figures dip between pages
/// without going idle; this window bridges that gap so an indicator doesn't flicker off
/// mid-read. Lives here so every reader of those figures applies the same debounce.
pub const SPEAKING_DEBOUNCE_MS: u32 = 1500;

/// [`CMD_BENCH`] engine selector: the plain ORT CPU execution provider.
pub const BENCH_ENGINE_CPU: u8 = 0;
/// [`CMD_BENCH`] engine selector: the Dawn WebGPU execution provider.
pub const BENCH_ENGINE_GPU: u8 = 1;
/// [`CMD_BENCH`] status: the timings that follow are valid.
pub const BENCH_OK: u32 = 0;
/// [`CMD_BENCH`] status: that engine couldn't run here (e.g. no working GPU adapter, or
/// the model isn't downloaded). The timing fields are 0.
pub const BENCH_FAILED: u32 = 1;
/// [`CMD_BENCH`] status: another measurement is already queued or running, and this one
/// was refused rather than queued behind it (a bench occupies the single synth worker for
/// tens of seconds). Distinct from [`BENCH_FAILED`] so a caller doesn't report a busy host
/// as an engine that doesn't work. The timing fields are 0.
pub const BENCH_BUSY: u32 = 2;

/// Frame-stream marker: the utterance is complete (no gain/samples follow). A leading
/// u32 >= [`STREAM_END`] is always a control marker, never a real sample count.
pub const STREAM_END: u32 = 0xFFFF_FFFE;
/// Frame-stream marker: a chunk failed; playback stops.
pub const SYNTH_ERROR: u32 = 0xFFFF_FFFF;
/// Frame-stream marker: the start of a new chunk, sent *before* that chunk's audio
/// sub-frames. Followed by `[u32 utf16Len][u32 nSamples]` — the chunk's length in UTF-16
/// code units of the request text and its total sample count. The SAPI engine uses this
/// to map each word/bookmark event to its true audio-stream offset while streaming (so
/// Kindle's per-word bookmark narrator stays in sync without the engine buffering the
/// whole utterance). A leading u32 >= [`STREAM_END`] is always a control marker.
pub const CHUNK_INFO: u32 = 0xFFFF_FFFD;

/// Sanity cap on a single request's text (1 MB).
pub const MAX_TEXT_BYTES: u32 = 1 << 20;

/// Sanity cap on a single response frame's sample count (~43 s of 24 kHz audio). The
/// host only ever sends ~250 ms sub-frames (~6000 samples), so this is generous
/// headroom — its purpose is to bound what the *client* (the x86 SAPI engine running
/// inside Kindle) will allocate off a frame header it read from the pipe. Without it a
/// process that squatted [`PIPE_NAME`] before the host could feed Kindle a huge
/// `nSamples` and either overflow `n * 4` on 32-bit or force a multi-GB allocation
/// (Kindle OOM/abort). Well under `u32`/`usize` range so `n * 4` can't overflow.
pub const MAX_FRAME_SAMPLES: u32 = 1 << 20;

/// Kokoro's native output rate (Hz); the stream is 24 kHz mono f32.
pub const SAMPLE_RATE: u32 = 24_000;
