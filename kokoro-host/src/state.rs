// The host's live view of itself: what it is playing, for whom, and what it believes
// Kindle is doing. One `Arc<HostState>` shared by every path that can change or read it —
// the pipe tasks, the loopback HTTP endpoint, and the Kindle-control thread — so a peer's
// `CMD_KINDLE` query answers from the same cell the audio writer just stamped.
//
// This exists because the host is now the *only* authority on Kindle reading: the settings
// panel sends intent over the pipe and renders what comes back, and never looks at Kindle
// itself. Everything a panel needs to draw its transport lives here.
//
// Every field is an atomic and every method is non-blocking. That's the point: a heartbeat
// or a Stop must never queue behind a long synthesis, a benchmark, or a UI Automation call
// (see the `CMD_KINDLE` contract in kokoro-protocol).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use kokoro_protocol::{
    STATE_BUSY, STATE_KINDLE_RUNNING, STATE_KINDLE_SYNTH, STATE_PAUSED, STATE_READING,
};

/// Wall-clock milliseconds since the Unix epoch (0 if the clock is before it). Used for
/// the audio stamps below: both the stamp and the elapsed math run in this one process, so
/// it needn't be monotonic — a ~1.5 s debounce tolerates minor skew.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Shared, lock-free host state. Created once in `main` and cloned as an `Arc`.
#[derive(Default)]
pub struct HostState {
    /// When the host last wrote audio to *any* client (0 = never). Answers `CMD_STATUS`.
    last_audio_ms: AtomicU64,
    /// When the host last wrote audio **for Kindle** — i.e. on a `CMD_SYNTH` stream, not a
    /// `CMD_PREVIEW` one and not the browser's HTTP path. Kept apart from `last_audio_ms`
    /// so "Kokoro is narrating" and "the panel is auditioning a narrator" are told apart by
    /// the host that knows, rather than guessed at by each client from timing.
    last_kindle_audio_ms: AtomicU64,
    /// The host's belief about Kindle's Assistive reader. Set by the commands it applies, and
    /// refined one-way by evidence — a Kindle synth stream that opened after the last command
    /// (see [`HostState::stream_proves_reading`]). Never read back off Kindle's own toggle.
    reading: AtomicBool,
    /// Live pause. Host-owned, in memory only: the pipe's streaming loop reads it per
    /// sub-frame and stalls there. Deliberately *not* persisted — it's a command, not a
    /// setting, and a host that came back up already stalled would just look broken.
    paused: AtomicBool,
    /// Whether `Kindle.exe` was running as of the last look.
    kindle_running: AtomicBool,
    /// The Kindle process the reading belief is *about* (0 = none). Identity, not just
    /// presence: see [`HostState::set_kindle_pid`].
    kindle_pid: AtomicU32,
    /// Set while a Play/Stop/Close is in flight on the Kindle-control thread.
    kindle_busy: AtomicBool,
    /// Set while a `CMD_BENCH` measurement is queued or running. Anything on the machine
    /// can open the pipe, and a bench occupies the one synth worker for tens of seconds;
    /// without this, a client could open connection after connection and queue enough
    /// measurements to starve Kindle well past the silent gap its narrator tolerates.
    /// One at a time, and a second request is refused rather than queued.
    bench_busy: AtomicBool,
    /// When a Play/Stop last set [`Self::reading`] deliberately (0 = never).
    commanded_ms: AtomicU64,
    /// When a Kindle `CMD_SYNTH` stream last *opened* (0 = never). Compared against
    /// `commanded_ms` by [`HostState::stream_proves_reading`].
    last_synth_start_ms: AtomicU64,
    /// How many `CMD_SYNTH` streams are open for Kindle. A count rather than a flag: nothing
    /// stops a second connection opening one, and with a flag the first to finish would
    /// clear it out from under the other.
    kindle_synth: AtomicU32,
}

/// Held for the life of one Kindle `CMD_SYNTH` stream; clears the mark when dropped.
///
/// A guard rather than a matched pair of calls because that stream ends at any of a dozen
/// `?`s in the middle of it — a client disconnecting mid-page is the *normal* way it ends.
/// A leaked mark would leave every client that gates on it waiting on a page that finished.
pub struct KindleSynthGuard(Arc<HostState>);

impl Drop for KindleSynthGuard {
    fn drop(&mut self) {
        self.0.kindle_synth.fetch_sub(1, Ordering::SeqCst);
    }
}

impl HostState {
    /// Record that audio just went out. `for_kindle` stamps the Kindle clock as well, and
    /// is true only on the `CMD_SYNTH` path the SAPI engine inside Kindle drives.
    pub fn stamp_audio(&self, for_kindle: bool) {
        let now = now_ms();
        self.last_audio_ms.store(now, Ordering::Relaxed);
        if for_kindle {
            self.last_kindle_audio_ms.store(now, Ordering::Relaxed);
        }
    }

    fn since(cell: &AtomicU64) -> u32 {
        let last = cell.load(Ordering::Relaxed);
        // last == 0 ("never") saturates to u32::MAX along with any long-idle host.
        now_ms().saturating_sub(last).min(u32::MAX as u64) as u32
    }

    /// Milliseconds since any audio was written (`u32::MAX` if never).
    pub fn ms_since_audio(&self) -> u32 {
        Self::since(&self.last_audio_ms)
    }

    /// Milliseconds since audio was written for Kindle (`u32::MAX` if never).
    pub fn ms_since_kindle_audio(&self) -> u32 {
        Self::since(&self.last_kindle_audio_ms)
    }

    pub fn reading(&self) -> bool {
        self.reading.load(Ordering::SeqCst)
    }

    pub fn set_reading(&self, on: bool) {
        self.reading.store(on, Ordering::SeqCst);
    }

    /// Set the belief because a command *made it so*, and stamp it — this is the one writer
    /// that knows rather than infers, so it timestamps itself and every later inference is
    /// measured against it.
    pub fn set_reading_commanded(&self, on: bool) {
        self.set_reading(on);
        self.commanded_ms.store(now_ms(), Ordering::SeqCst);
    }

    /// Positive evidence that Kindle is reading **now**: a `CMD_SYNTH` stream is open for it,
    /// and that stream opened *after* the last Play/Stop. One-way — it can conclude "reading",
    /// never "stopped".
    ///
    /// A stream open for Kindle means Kindle's own narrator asked for a page, which is
    /// stronger evidence than audio and arrives seconds earlier (before the first sample
    /// exists). The start-time comparison is what keeps it from arguing with a command: the
    /// stream a Stop interrupted opened *before* that Stop, so it proves nothing about the
    /// present no matter how long Kindle takes to abandon it.
    ///
    /// Two earlier shapes were wrong, and both failures are worth keeping:
    ///
    /// - **The audio clock alone** undid the Stop the user had just watched succeed. Audio
    ///   doesn't cease when Read Aloud does — the stream runs ahead of the ear,
    ///   the speaking debounce outlives the final sample, and Kindle may play out the rest
    ///   of the page — so a refresh in that tail concluded "reading" and bounced the switch
    ///   back ON.
    /// - **A latch cleared by observed silence** fixed the bounce and broke detection. It
    ///   needed something to *watch* the silence, and refreshes only happen while a client is
    ///   querying: close the panel after a Stop, start Read Aloud inside Kindle, reopen the
    ///   panel, and the silence in between was never observed — so the latch was still set,
    ///   the inference stayed muzzled, and the belief stayed `false` while Kindle read aloud.
    ///   Stop then took its early return and sent no Ctrl+A at all. Comparing timestamps needs
    ///   no observer: the answer is computed from what happened, not from who was watching.
    pub fn stream_proves_reading(&self) -> bool {
        self.kindle_synth()
            && self.last_synth_start_ms.load(Ordering::SeqCst)
                > self.commanded_ms.load(Ordering::SeqCst)
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn set_paused(&self, on: bool) {
        self.paused.store(on, Ordering::SeqCst);
    }

    pub fn kindle_running(&self) -> bool {
        self.kindle_running.load(Ordering::SeqCst)
    }

    /// Record *which* Kindle is running (`None` = none), and void the reading belief
    /// whenever that changes.
    ///
    /// Presence alone is not enough, and this is the whole reason the pid is stored. A
    /// **different** Kindle is as much a reset as no Kindle: `Some(old) -> Some(new)`
    /// happens whenever Kindle restarts between two looks — which the panel's own
    /// narration-toggle flow actively invites ("Kindle closed - reopen it") — and a
    /// presence-only check sees nothing at all. The belief would survive into a reader that
    /// launched idle, and since Read Aloud is a blind Ctrl+A toggle, Play would then decide
    /// it was already reading and do nothing while Stop would *start* reading.
    ///
    /// Both callers that learn the pid (the watcher tick and the control thread's refresh)
    /// route through here, so the rule lives in one place.
    pub fn set_kindle_pid(&self, pid: Option<u32>) {
        // PID 0 is never a real process, so it stands in for "none" without an extra cell.
        let now = pid.unwrap_or(0);
        self.kindle_running.store(pid.is_some(), Ordering::SeqCst);
        // Read, reset, and publish the new pid LAST — never publish first.
        //
        // Two threads call this (the watcher tick and the control thread's refresh). With a
        // `swap` up front there is a window where the new pid is already visible beside the
        // *old* belief: a Stop landing in it refreshes, finds the pid it expects, keeps the
        // dead Kindle's `reading = true`, and fires its Ctrl+A into the fresh reader —
        // starting it, while the switch reads off and every later Stop no-ops. In this order
        // the only intermediate a peer can observe is the old pid beside a cleared belief,
        // which is merely conservative. Both threads may run the reset; it is idempotent.
        if self.kindle_pid.load(Ordering::SeqCst) != now {
            self.set_reading(false);
            self.set_paused(false);
            // Void the clocks along with the belief they support. They measure a Kindle that
            // is gone, and the inference would otherwise read the dead reader's stream as
            // proof the *replacement* is already narrating — handing the fresh Kindle the
            // same wrong belief this reset exists to clear.
            self.last_kindle_audio_ms.store(0, Ordering::Relaxed);
            self.last_synth_start_ms.store(0, Ordering::SeqCst);
            self.commanded_ms.store(0, Ordering::SeqCst);
            self.kindle_pid.store(now, Ordering::SeqCst);
        }
    }

    pub fn kindle_busy(&self) -> bool {
        self.kindle_busy.load(Ordering::SeqCst)
    }

    pub fn set_kindle_busy(&self, on: bool) {
        self.kindle_busy.store(on, Ordering::SeqCst);
    }

    /// Mark a Kindle synth stream as open until the returned guard is dropped, and record
    /// *when* it opened — [`Self::stream_proves_reading`] compares that against the last
    /// command to tell a stream a Stop interrupted from one Kindle began afterwards.
    pub fn enter_kindle_synth(self: &Arc<Self>) -> KindleSynthGuard {
        self.last_synth_start_ms.store(now_ms(), Ordering::SeqCst);
        self.kindle_synth.fetch_add(1, Ordering::SeqCst);
        KindleSynthGuard(self.clone())
    }

    /// Is the host synthesizing or streaming a page for Kindle right now?
    ///
    /// True from the moment Kindle's narrator asks for a page, so unlike [`Self::kindle_speaking`]
    /// it covers the seconds of synthesis before any audio exists to time from.
    pub fn kindle_synth(&self) -> bool {
        self.kindle_synth.load(Ordering::SeqCst) != 0
    }

    /// Claim the single bench slot; `true` means someone else already holds it.
    pub fn bench_busy_swap(&self, on: bool) -> bool {
        self.bench_busy.swap(on, Ordering::SeqCst)
    }

    pub fn set_bench_busy(&self, on: bool) {
        self.bench_busy.store(on, Ordering::SeqCst);
    }

    /// The `CMD_KINDLE` state byte.
    pub fn state_flags(&self) -> u8 {
        let mut f = 0u8;
        if self.reading() {
            f |= STATE_READING;
        }
        if self.paused() {
            f |= STATE_PAUSED;
        }
        if self.kindle_running() {
            f |= STATE_KINDLE_RUNNING;
        }
        if self.kindle_busy() {
            f |= STATE_BUSY;
        }
        if self.kindle_synth() {
            f |= STATE_KINDLE_SYNTH;
        }
        f
    }
}
