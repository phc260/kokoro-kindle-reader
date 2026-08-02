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
//! - [`CMD_SYNTH_ALIGNED`] (`'A'`): [`CMD_SYNTH`] with model-derived word marks.
//!   - request: identical to [`CMD_SYNTH`] — `[u8 'A'][f32 rate][u32 textBytes][utf8 text]`.
//!   - response: the same paced frame stream, except each chunk opens with
//!     [`CHUNK_ALIGNED`] + `[u32 charStartUtf16][u32 charLenUtf16][u32 nSamples]
//!     [u32 markCount]` and then `markCount` fixed-width marks
//!     (`[u32 charStart][u32 charLen][u32 sampleStart][u32 sampleEnd]`, [`MARK_BYTES`]
//!     each) *before* the chunk's audio sub-frames. [`STREAM_END`] / [`SYNTH_ERROR`]
//!     terminate it unchanged.
//!
//!   Why a command of its own rather than a flag on [`CMD_SYNTH`]: that stream is what an
//!   already-installed SAPI engine parses, and it must stay byte-for-byte what it was
//!   through an upgrade in either direction. See [`CHUNK_ALIGNED`] for what the header
//!   carries that [`CHUNK_INFO`] cannot, and the notes there on negotiation.
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
/// Command byte: synthesize the whole utterance and return model-derived word marks with
/// each chunk. Same request bytes as [`CMD_SYNTH`], same paced audio; the chunk headers
/// are [`CHUNK_ALIGNED`] rather than [`CHUNK_INFO`]. See the module docs.
///
/// **Negotiation.** A host that predates this command has no arm for it and drops the
/// client (the pipe server's unknown-command case), so a client sees its connection close
/// before a single frame arrives — that, and only that, is the "old host" answer.
///
/// It surfaces in **two shapes and a client must handle both**: the host reads one command
/// byte and closes, so whether the rest of the request (`rate`/`textBytes`/`text`) is already
/// buffered is a race. Lose it and the *write* fails mid-request; win it and the write
/// succeeds and the first *read* fails. Handling only the read shape leaves the fallback
/// unreachable in practice.
///
/// In practice a client cannot separate that from "no host" or "host died" without trying, so
/// the honest rule is that **any failure to get a request onto the wire and a frame back is
/// treated the same way**: retry once as [`CMD_SYNTH`]. An absent host fails both attempts and
/// ends at the same error one `CreateFile` later; a request too large for [`MAX_TEXT_BYTES`]
/// never reaches the pipe in either form. Neither produces a wrong audible answer, which is
/// what makes the over-broad reading safe. A client may
/// fall back to [`CMD_SYNTH`] on it, reconnecting and re-sending the same utterance.
///
/// **The fallback must not be cached — not even for the connection.** Zero frames is
/// *necessary* evidence of an old host but not *sufficient*: a current host that crashes or
/// is quit between accepting the request and writing its first frame produces exactly the
/// same observation, and that window is as wide as a chunk's synthesis. And the evidence
/// destroys the very connection it is about, so the only place a client could record it is
/// the *replacement* connection — which may be a restarted, capable host. Caching it there
/// saves one failed probe per page and pays for it by losing marks for as long as that
/// connection lives. Probe every utterance: a probe is a connect, a write and a failed read,
/// all before any audio exists, against a page that costs seconds of synthesis.
///
/// A host reporting [`SYNTH_ERROR`] is **not** this signal, even as the very first frame
/// (which it is whenever the first chunk fails to synthesize). That is a live host answering,
/// and treating it as an old one costs the marks *and* re-sends the utterance.
/// A corrupt stream splits into two cases, and they are handled differently on purpose:
///
/// - **The HEADER is unusable** — a count over [`MAX_MARKS_PER_CHUNK`] or over the chunk's own
///   `charLenUtf16`, or a `charStart + charLenUtf16` that overflows. There is nothing behind
///   it worth resynchronizing to and the announced length cannot be trusted to drain, so this
///   **fails closed**: drop the connection.
/// - **The header is plausible but a MARK fails its bounds.** The announced bytes are bounded
///   by the chunk's own character count, so they can be drained and the audio behind them is
///   still good. The chunk is returned with `marks` empty and the client interpolates it —
///   the same thing it does for a chunk the host had no timing for. **Losing a page over a
///   highlight is the worse outcome**: a mid-page failure is what makes Kindle treat the page
///   as done and race through the book.
///
/// What must NOT happen is presenting an interpolated offset as a model-derived one. The
/// distinction lives in the marks being absent, not in the timing being quietly approximated.
pub const CMD_SYNTH_ALIGNED: u8 = b'A';
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

/// Frame-stream marker: the start of a new chunk of a [`CMD_SYNTH_ALIGNED`] response.
/// Followed by `[u32 charStartUtf16][u32 charLenUtf16][u32 nSamples][u32 markCount]`, then
/// `markCount` marks of [`MARK_BYTES`] each, then the chunk's audio sub-frames.
///
/// Two things it carries that [`CHUNK_INFO`] does not:
///
/// - **The chunk's absolute start**, not just its length. A client reading [`CHUNK_INFO`]
///   can only accumulate lengths, which assumes chunks abut in the request text. They do
///   not: the host trims whitespace at every chunk boundary, so an accumulated position
///   drifts a character or so per chunk — small at the top of a page and about a word wide
///   by the foot of it, which is precisely where a highlight is most visibly wrong.
/// - **The marks themselves**, so a word's audio offset is one the model predicted rather
///   than the word's character fraction of the chunk. The old mapping delivers an
///   *estimate* at an exact offset, which is not the same thing as alignment and must not
///   be described as one.
///
/// Marks are ordered, their `charStart` is absolute in the request text (the same
/// coordinate space as the header's `charStartUtf16` and as a SAPI event's position), and
/// their samples are **relative to this chunk** — a client already tracks the byte offset
/// the chunk began at, and chunk-relative samples cannot overflow a `u32` for any chunk
/// the host will send.
pub const CHUNK_ALIGNED: u32 = 0xFFFF_FFFC;

/// Wire size of one [`CHUNK_ALIGNED`] mark: `[u32 charStart][u32 charLen][u32 sampleStart]
/// [u32 sampleEnd]`.
pub const MARK_BYTES: u32 = 16;

/// Hard cap on a chunk's `markCount`, bounding what the **x86 SAPI engine inside Kindle**
/// allocates off a length it read from the pipe — the same reasoning as
/// [`MAX_FRAME_SAMPLES`], and for the same reason: [`PIPE_NAME`] is openable by anything
/// running as this user, so a squatted pipe could otherwise ask Kindle for a multi-GB
/// allocation. `65536` marks is ~1 MB of wire and far more words than any chunk holds.
///
/// This is the *ceiling*, not the test. A caller must also reject a count that exceeds the
/// chunk's own `charLenUtf16`: a mark needs at least one code unit, so a chunk can never
/// legitimately carry more marks than it has characters, and that bound is the tight one.
pub const MAX_MARKS_PER_CHUNK: u32 = 1 << 16;

/// Whether one [`CHUNK_ALIGNED`] mark is internally consistent and inside its chunk.
///
/// Shared by both ends so the host cannot emit a shape the engine rejects, and the engine
/// cannot accept one the host would not emit — the same reason the wire constants live in
/// this crate at all. `prev` is the preceding mark's `(charEnd, sampleEnd)`, or `(0, 0)`
/// for the first in a chunk: consecutive spans may touch but never overlap or run
/// backwards, in either coordinate. Character order has to hold because the engine matches
/// SAPI events to marks by position; sample order has to hold because SAPI requires event
/// offsets to be non-decreasing.
///
/// Rejects, rather than repairs: an inconsistent mark stream is a synthesis or transport
/// fault, and guessing at it puts the rest of the utterance out of step to hide one bad
/// entry.
pub fn mark_is_valid(
    mark: (u32, u32, u32, u32),
    prev: (u32, u32),
    chunk_char_start: u32,
    chunk_char_len: u32,
    chunk_samples: u32,
) -> bool {
    let (char_start, char_len, sample_start, sample_end) = mark;
    let (prev_char_end, prev_sample_end) = prev;
    // Character span: inside the chunk, non-empty, no overflow off either end.
    let Some(char_end) = char_start.checked_add(char_len) else { return false };
    let Some(chunk_char_end) = chunk_char_start.checked_add(chunk_char_len) else { return false };
    if char_len == 0 || char_start < chunk_char_start || char_end > chunk_char_end {
        return false;
    }
    // Sample span: inside this chunk's audio, non-reversed.
    if sample_end > chunk_samples || sample_start > sample_end {
        return false;
    }
    // Monotonic in both coordinates.
    if char_start < prev_char_end || sample_start < prev_sample_end {
        return false;
    }
    true
}

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
