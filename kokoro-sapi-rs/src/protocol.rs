//! Wire protocol between the SAPI engine and the kokoro-host synth over a byte-mode
//! named pipe. 1:1 with `kokoro-sapi/src/WorkerProtocol.h` and `kokoro-host/src/pipe.rs`
//! — change it in all places.

// The pipe the host serves is `\\.\pipe\KokoroSapiSynth` (see `worker.rs`, which
// uses the wide literal directly for CreateFileW).

/// Command byte: synth the whole utterance.
pub const CMD_SYNTH: u8 = b'S';

/// Frame-stream markers for the 'S' response. A leading u32 >= STREAM_END is a
/// control marker, never a real sample count.
pub const STREAM_END: u32 = 0xFFFF_FFFE; // utterance complete
pub const SYNTH_ERROR: u32 = 0xFFFF_FFFF; // a chunk failed

/// Sanity cap on request text (1 MB).
pub const MAX_TEXT_BYTES: u32 = 1 << 20;
