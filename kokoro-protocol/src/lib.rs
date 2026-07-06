//! The named-pipe wire protocol between clients (the x86 SAPI engine) and the x64
//! synthesis host (`kokoro-host`). One canonical source of truth, shared by both ends
//! so the format can't drift — replacing the old `WorkerProtocol.h` ⇆ `pipe.rs`
//! "change it in both places" duplication.
//!
//! Every request starts with a one-byte command:
//!
//! - [`CMD_SYNTH`] (`'S'`): synth the whole utterance.
//!   - request:  `[u8 'S'][f32 rate][u32 textBytes][utf8 text]`
//!   - response: a STREAM of frames, one per synthesized chunk —
//!     `[u32 nSamples][f32 gain][f32 samples...]` (24 kHz mono, [-1, 1]) —
//!     terminated by a marker whose leading u32 is [`STREAM_END`] (complete) or
//!     [`SYNTH_ERROR`] (a chunk failed). `rate` is the host's rate-derived speed
//!     multiplier; the host owns the narrator + folds in the user's own speed, so
//!     those don't cross the wire. `gain` (the user's volume, fresh per chunk) rides
//!     along in each frame and the engine applies it when converting to int16.
//!
//! - [`CMD_INFO`] (`'I'`): `-> [u16 jsonBytes][utf8 json]`.

#![no_std]

/// The pipe the host serves and clients connect to.
pub const PIPE_NAME: &str = r"\\.\pipe\KokoroSapiSynth";

/// Command byte: synthesize the whole utterance.
pub const CMD_SYNTH: u8 = b'S';
/// Command byte: return a small JSON info blob.
pub const CMD_INFO: u8 = b'I';

/// Frame-stream marker: the utterance is complete (no gain/samples follow). A leading
/// u32 >= [`STREAM_END`] is always a control marker, never a real sample count.
pub const STREAM_END: u32 = 0xFFFF_FFFE;
/// Frame-stream marker: a chunk failed; playback stops.
pub const SYNTH_ERROR: u32 = 0xFFFF_FFFF;

/// Sanity cap on a single request's text (1 MB).
pub const MAX_TEXT_BYTES: u32 = 1 << 20;

/// Kokoro's native output rate (Hz); the stream is 24 kHz mono f32.
pub const SAMPLE_RATE: u32 = 24_000;
