// The host's live view of *itself*: whether it is putting audio out, and whether the one
// bench slot is taken. One `Arc<HostState>` shared by every path that can change or read it
// — the pipe tasks and the loopback HTTP endpoint — so a `CMD_STATUS` on the panel's
// connection sees audio streamed on Kindle's, and a browser narration is visible to a panel
// about to start a measurement.
//
// What the host believes *Kindle* is doing is deliberately not here: that is
// `kindle_state::KindleState`, which references this and adds the reading belief, the
// Kindle-only audio clock, and the pid it is all about. The split is an ownership boundary,
// not a filing decision — the browser path reaches this and can no more read Kindle's
// belief than it can call UI Automation, which is what keeps "serve a page image" from
// depending on a Windows reader being installed.
//
// Every field is an atomic and every method is non-blocking. That's the point: a heartbeat
// or a Stop must never queue behind a long synthesis, a benchmark, or a UI Automation call
// (see the `CMD_KINDLE` contract in kokoro-protocol).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock milliseconds since the Unix epoch (0 if the clock is before it). Used for
/// the audio stamps below: both the stamp and the elapsed math run in this one process, so
/// it needn't be monotonic — a ~1.5 s debounce tolerates minor skew.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Shared, lock-free general host state. Created once in `main` and cloned as an `Arc`.
#[derive(Default)]
pub struct HostState {
    /// When the host last wrote audio to *any* client (0 = never). Answers `CMD_STATUS`.
    last_audio_ms: AtomicU64,
    /// Set while a `CMD_BENCH` measurement is queued or running. Anything on the machine
    /// can open the pipe, and a bench occupies the one synth worker for tens of seconds;
    /// without this, a client could open connection after connection and queue enough
    /// measurements to starve Kindle well past the silent gap its narrator tolerates.
    /// One at a time, and a second request is refused rather than queued.
    bench_busy: AtomicBool,
}

impl HostState {
    /// Record that audio just went out, to any client.
    pub fn stamp_audio(&self) {
        self.stamp_audio_at(now_ms());
    }

    /// Stamp with a caller-supplied instant. Exists for [`crate::kindle_state::KindleState`],
    /// which stamps this clock and its own Kindle-only one from a single reading — two
    /// `now_ms()` calls would put a hair of skew between clocks that describe the same write.
    pub(crate) fn stamp_audio_at(&self, now: u64) {
        self.last_audio_ms.store(now, Ordering::Relaxed);
    }

    /// Milliseconds since a cell was stamped (`u32::MAX` if never).
    pub(crate) fn since(cell: &AtomicU64) -> u32 {
        let last = cell.load(Ordering::Relaxed);
        // last == 0 ("never") saturates to u32::MAX along with any long-idle host.
        now_ms().saturating_sub(last).min(u32::MAX as u64) as u32
    }

    /// Milliseconds since any audio was written (`u32::MAX` if never).
    pub fn ms_since_audio(&self) -> u32 {
        Self::since(&self.last_audio_ms)
    }

    /// Claim the single bench slot; `true` means someone else already holds it.
    pub fn bench_busy_swap(&self, on: bool) -> bool {
        self.bench_busy.swap(on, Ordering::SeqCst)
    }

    pub fn set_bench_busy(&self, on: bool) {
        self.bench_busy.store(on, Ordering::SeqCst);
    }
}
