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
    CMD_SYNTH, MAX_FRAME_SAMPLES, MAX_TEXT_BYTES, PIPE_NAME, STREAM_END, SYNTH_ERROR,
};

/// Result of reading one frame of the 'S' response stream.
pub enum Frame {
    /// A chunk's PCM (24 kHz float, [-1, 1]) + its fresh gain.
    Data { samples: Vec<f32>, gain: f32 },
    /// Clean end of the utterance.
    End,
    /// Failed / broken stream (the pipe is closed).
    Error,
}

/// The pipe handle, stored atomically so `close` can interrupt a blocked read from
/// another thread (cancel-by-close). `INVALID_HANDLE_VALUE.0` is the "no pipe" state.
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

    /// Send the whole utterance for synthesis (one 'S' request). `rate` is the host's
    /// rate-derived speed multiplier. Returns false (and closes) if it can't be written.
    pub fn begin_synth(&self, text: &[u8], rate: f32) -> bool {
        if !self.is_open() || text.len() as u64 > MAX_TEXT_BYTES as u64 {
            return false;
        }
        let text_bytes = text.len() as u32;
        let ok = self.write_all(&[CMD_SYNTH])
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
            return Frame::Error; // host keeps the stream open
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
