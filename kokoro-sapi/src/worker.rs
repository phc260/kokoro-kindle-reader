//! Client side of the synthesis pipe (see `protocol`). Connect-only: the running
//! kokoro-host serves the pipe and synthesizes; if it isn't up, `ensure_connected`
//! fails and the utterance is silently skipped. Mirrors `WorkerClient.cpp`.

use core::ffi::c_void;
use core::sync::atomic::{AtomicPtr, Ordering};

use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING,
};
use windows_core::PCWSTR;

use kokoro_protocol::{
    mark_is_valid, CHUNK_ALIGNED, CHUNK_INFO, CMD_SYNTH, CMD_SYNTH_ALIGNED, MARK_BYTES,
    MAX_FRAME_SAMPLES, MAX_MARKS_PER_CHUNK, MAX_TEXT_BYTES, PIPE_NAME, STREAM_END, SYNTH_ERROR,
};

/// One word's stretch of audio, as read off a [`CHUNK_ALIGNED`] header: where the word sits
/// in the request text (absolute, UTF-16) and which samples of *this chunk* speak it.
#[derive(Clone, Copy)]
pub struct Mark {
    pub char_start: u32,
    pub char_len: u32,
    pub sample_start: u32,
    pub sample_end: u32,
}

/// Result of reading one frame of a synthesis response stream.
pub enum Frame {
    /// A chunk's PCM (24 kHz float, [-1, 1]) + its fresh gain.
    Data { samples: Vec<f32>, gain: f32 },
    /// Start of a new chunk: its length in UTF-16 units of the request text + its total
    /// sample count. Precedes the chunk's `Data` sub-frames so the engine can place
    /// word/bookmark events at true audio offsets.
    ///
    /// `start_u16` is the chunk's ABSOLUTE start in the request text. A [`CHUNK_INFO`]
    /// header doesn't carry one, so it arrives as `None` and the engine falls back to
    /// accumulating lengths; a [`CHUNK_ALIGNED`] one always does, which is what removes the
    /// per-chunk drift that accumulation causes.
    ///
    /// `marks` empty means "no timing for this chunk" — a stock model, or timing that
    /// didn't validate at either end. It is never a claim that the chunk has no words, and
    /// the engine must fall back to interpolation for it rather than firing nothing.
    Chunk { start_u16: Option<u32>, u16_len: u32, samples: u32, marks: Vec<Mark> },
    /// Clean end of the utterance.
    End,
    /// The host reported [`SYNTH_ERROR`]: it understood the request and could not finish it.
    /// The stream is over but **the pipe is still open** — this is a live host answering.
    Failed,
    /// Broken stream: the pipe could not be read and is now closed.
    ///
    /// Distinct from [`Frame::Failed`] because the two are **opposite evidence** for
    /// capability negotiation. An old host rejects an unknown command byte by dropping the
    /// client, so `Error` with no frames yet is the one signal that means "downgrade";
    /// `Failed` proves the command was understood. Collapsing them would let a single
    /// transient synthesis failure on the first chunk disable marks for the rest of the
    /// process — and re-send the whole utterance for a second synthesis.
    Error,
}

/// The pipe handle, stored atomically so `close` can interrupt a blocked read from
/// another thread (cancel-by-close). `INVALID_HANDLE_VALUE.0` is the "no pipe" state.
///
/// **Holds no capability state.** Whether to ask for [`CMD_SYNTH_ALIGNED`] is an argument to
/// [`Worker::begin_synth`], decided per request by the caller, and there is deliberately
/// nowhere to cache the answer. The evidence for "old host" is a connection that closed with
/// zero frames — which *destroys the connection it is evidence about*, so there is no live
/// thing to attach it to. Caching it on the replacement connection would be attributing C0's
/// behaviour to C1, and C1 may be a restarted, capable host: that is a permanent loss of
/// marks bought to save one failed probe per page. A probe costs a `CreateFile`, a write and
/// a failed read, all before any audio exists; a page costs seconds of synthesis.
pub struct Worker {
    pipe: AtomicPtr<c_void>,
}

impl Worker {
    pub const fn new() -> Self {
        Worker { pipe: AtomicPtr::new(INVALID_HANDLE_VALUE.0) }
    }

    fn handle(&self) -> HANDLE {
        HANDLE(self.pipe.load(Ordering::Acquire))
    }

    fn is_open(&self) -> bool {
        self.pipe.load(Ordering::Acquire) != INVALID_HANDLE_VALUE.0
    }

    /// Connect to the host's pipe. Returns false if nothing is serving it.
    pub fn ensure_connected(&self) -> bool {
        if self.is_open() {
            return true;
        }
        let name: Vec<u16> = PIPE_NAME.encode_utf16().chain(core::iter::once(0)).collect();
        let h = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                (GENERIC_READ.0 | GENERIC_WRITE.0) as u32,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        };
        match h {
            Ok(h) if !h.is_invalid() => {
                self.pipe.store(h.0, Ordering::Release);
                true
            }
            _ => false,
        }
    }

    /// Send the whole utterance for synthesis (one request). `rate` is the host's
    /// rate-derived speed multiplier. Returns false (and closes) if it can't be written.
    ///
    /// `aligned` picks [`CMD_SYNTH_ALIGNED`] over [`CMD_SYNTH`]. The two requests have
    /// identical bodies, so the only thing that changes is the chunk header the host writes
    /// back — which is why a caller can retry the same utterance the other way for free.
    pub fn begin_synth(&self, text: &[u8], rate: f32, aligned: bool) -> bool {
        if !self.is_open() || text.len() as u64 > MAX_TEXT_BYTES as u64 {
            return false;
        }
        let text_bytes = text.len() as u32;
        let cmd = if aligned { CMD_SYNTH_ALIGNED } else { CMD_SYNTH };
        let ok = self.write_all(&[cmd])
            && self.write_all(&rate.to_le_bytes())
            && self.write_all(&text_bytes.to_le_bytes())
            && (text_bytes == 0 || self.write_all(text));
        if !ok {
            self.close();
        }
        ok
    }

    /// Read the next frame of a stream started by `begin_synth`.
    pub fn read_frame(&self) -> Frame {
        if !self.is_open() {
            return Frame::Error;
        }
        let mut n = [0u8; 4];
        if !self.read_all(&mut n) {
            self.close();
            return Frame::Error;
        }
        let n = u32::from_le_bytes(n);
        if n == STREAM_END {
            return Frame::End;
        }
        if n == SYNTH_ERROR {
            return Frame::Failed; // host keeps the stream open
        }
        if n == CHUNK_INFO {
            // Chunk header: [u32 utf16Len][u32 nSamples].
            let mut a = [0u8; 4];
            let mut b = [0u8; 4];
            if !self.read_all(&mut a) || !self.read_all(&mut b) {
                self.close();
                return Frame::Error;
            }
            return Frame::Chunk {
                start_u16: None,
                u16_len: u32::from_le_bytes(a),
                samples: u32::from_le_bytes(b),
                marks: Vec::new(),
            };
        }
        if n == CHUNK_ALIGNED {
            return self.read_aligned_chunk();
        }
        // Bound what we'll allocate off a pipe-supplied header: the real host never
        // sends frames this large, so anything over the cap means a corrupt/hostile
        // stream (e.g. a squatted pipe). Reject rather than allocate n*4 bytes.
        if n > MAX_FRAME_SAMPLES {
            self.close();
            return Frame::Error;
        }

        let mut g = [0u8; 4];
        if !self.read_all(&mut g) {
            self.close();
            return Frame::Error;
        }
        let gain = f32::from_le_bytes(g);

        let mut bytes = vec![0u8; n as usize * 4];
        if n != 0 && !self.read_all(&mut bytes) {
            self.close();
            return Frame::Error;
        }
        let samples = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        Frame::Data { samples, gain }
    }

    /// Read the body of a [`CHUNK_ALIGNED`] header:
    /// `[u32 charStart][u32 charLen][u32 nSamples][u32 markCount]` then `markCount` marks.
    ///
    /// **A malformed mark stream costs the marks, not the page.** Every rejection below
    /// returns the chunk with `marks` empty, and the engine then interpolates through it
    /// exactly as it did before this frame existed — the bytes have already been consumed,
    /// so the audio that follows is still readable. Dropping the connection instead would
    /// silence a page over a highlight, and a mid-page disconnect is what makes Kindle race
    /// through a book. Only a short read (a genuinely broken pipe) is fatal.
    fn read_aligned_chunk(&self) -> Frame {
        let mut h = [0u8; 16];
        if !self.read_all(&mut h) {
            self.close();
            return Frame::Error;
        }
        let word = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
        let (start_u16, u16_len, samples, count) = (word(0), word(4), word(8), word(12));

        // An implausible count is a garbage header, and there is nothing on the far side of
        // it worth resynchronizing to — so close, don't drain. Draining it would mean up to
        // `u32::MAX` blocking 16-byte reads on the thread inside Kindle that `Speak` is
        // running on, which a process squatting `PIPE_NAME` (openable by anything running as
        // this user) could feed slowly to hold Read Aloud indefinitely.
        //
        // A mark needs at least one code unit, so the chunk's own `charLenUtf16` is the tight
        // bound and the constant is only the ceiling. Both are pipe-supplied, so both are
        // applied: `u16_len` alone could itself be `u32::MAX`.
        if count > MAX_MARKS_PER_CHUNK || count > u16_len {
            self.close();
            return Frame::Error;
        }
        // `start + len` must not overflow. `mark_is_valid` checks this with `checked_add`,
        // but ONLY while validating a mark - a header with `markCount = 0` is perfectly
        // legal (a stock model produces it) and reaches the engine unchecked. On i686
        // `usize` is 32 bits, so `start = 0xFFFF_FFFF, len = 1` wraps the mapper's chunk-end
        // to 0 and every event is treated as belonging to a later chunk; and because both
        // profiles set `panic = "abort"`, a debug build takes Kindle down instead.
        if start_u16.checked_add(u16_len).is_none() {
            self.close();
            return Frame::Error;
        }
        // Past here the count is bounded by the chunk's character length, so draining what
        // was announced is bounded work — and worth doing, because a *plausible* header whose
        // marks fail validation still has good audio behind it.
        let mut ok = true;
        let mut marks: Vec<Mark> = Vec::with_capacity(count as usize);
        let mut prev = (0u32, 0u32);
        let mut buf = [0u8; MARK_BYTES as usize];
        for _ in 0..count {
            if !self.read_all(&mut buf) {
                self.close();
                return Frame::Error;
            }
            if !ok {
                continue; // still draining; the marks are already forfeit
            }
            let w = |i: usize| u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
            let m = (w(0), w(4), w(8), w(12));
            ok = mark_is_valid(m, prev, start_u16, u16_len, samples);
            if !ok {
                continue;
            }
            prev = (m.0.saturating_add(m.1), m.3);
            marks.push(Mark { char_start: m.0, char_len: m.1, sample_start: m.2, sample_end: m.3 });
        }
        if !ok {
            marks.clear();
        }
        Frame::Chunk { start_u16: Some(start_u16), u16_len, samples, marks }
    }

    /// Close the pipe. Atomic swap so a concurrent `close` (cancel-by-close) only
    /// closes the real handle once.
    pub fn close(&self) {
        let raw = self.pipe.swap(INVALID_HANDLE_VALUE.0, Ordering::AcqRel);
        if raw != INVALID_HANDLE_VALUE.0 {
            unsafe {
                let _ = CloseHandle(HANDLE(raw));
            }
        }
    }

    // Byte-mode pipes may deliver partial reads/writes; loop until exact.
    fn write_all(&self, mut buf: &[u8]) -> bool {
        let h = self.handle();
        while !buf.is_empty() {
            let mut put = 0u32;
            if unsafe { WriteFile(h, Some(buf), Some(&mut put), None) }.is_err() || put == 0 {
                return false;
            }
            buf = &buf[put as usize..];
        }
        true
    }

    fn read_all(&self, mut buf: &mut [u8]) -> bool {
        let h = self.handle();
        while !buf.is_empty() {
            let mut got = 0u32;
            if unsafe { ReadFile(h, Some(buf), Some(&mut got), None) }.is_err() || got == 0 {
                return false;
            }
            buf = &mut buf[got as usize..];
        }
        true
    }
}

// The AtomicPtr holds an OS HANDLE, which is safe to move/share across threads.
unsafe impl Send for Worker {}
unsafe impl Sync for Worker {}
