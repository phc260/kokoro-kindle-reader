// The panel's whole relationship with Kindle, in one file: it asks `kokoro-host`.
//
// The panel used to drive Kindle itself — enumerate its windows, foreground it, read its UI
// Automation tree, send it keystrokes — while the host did the same thing for injection and
// neither knew what the other believed. That's gone: this module sends intent (`CMD_KINDLE`)
// over the same named pipe Preview and the speed test already use, and returns the state the
// host reports back. There is no second transport, and nothing here looks at Kindle.
//
// Health is defined by reachability, not by a reply value: an `Err` means the host isn't
// there (offline). That distinction is the one the old `unwrap_or(false)` threw away, which
// made a stopped host indistinguishable from a host sitting idle.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use kokoro_protocol::{
    CMD_KINDLE, KINDLE_OK, MAX_MSG_BYTES, PIPE_NAME, SPEAKING_DEBOUNCE_MS, STATE_BUSY,
    STATE_KINDLE_RUNNING, STATE_KINDLE_SYNTH, STATE_PAUSED, STATE_READING,
};

// ERROR_PIPE_BUSY: all pipe instances are momentarily in use; wait and retry. (A Windows
// error code, not part of the wire protocol, so it stays local.)
const ERROR_PIPE_BUSY: i32 = 231;

/// How long a connect may spend riding out `ERROR_PIPE_BUSY` on the *heartbeat* path. Short
/// enough to stay well inside the heartbeat's own budget: a busy pipe is a host that's
/// alive, so retrying briefly beats reporting it offline, but not at the cost of a late
/// answer.
pub const QUICK_BUSY_WAIT: Duration = Duration::from_millis(300);
/// The same, for a user-initiated action (Preview, Play, the speed test). The user is
/// waiting on a click, not on a 1 Hz tick, so it's worth waiting longer than failing.
pub const ACTION_BUSY_WAIT: Duration = Duration::from_millis(2000);

/// Open the host's pipe, retrying for up to `busy_wait` while every instance is in use.
/// Shared by every client in the panel (`hostlink`, `preview`, `benchmark`) so they can't
/// disagree about what "the host isn't there" means.
pub fn connect(busy_wait: Duration) -> Result<std::fs::File, String> {
    let deadline = Instant::now() + busy_wait;
    loop {
        match OpenOptions::new().read(true).write(true).open(PIPE_NAME) {
            Ok(f) => return Ok(f),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                if Instant::now() >= deadline {
                    return Err("the synthesis host is busy - try again.".to_string());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(format!(
                    "can't reach the synthesis host ({e}). Is Kokoro Kindle Reader running?"
                ))
            }
        }
    }
}

/// One `CMD_KINDLE` reply — everything the panel draws its transport from.
///
/// There is no `online` field: this type only exists when the host answered. Offline is the
/// `Err` arm of [`send`], which is the whole point of routing health through real I/O
/// rather than through a cached engine handle or a process-name lookup.
pub struct HostReport {
    /// The host applied the action. `false` means it couldn't (Kindle absent, UI Automation
    /// failed) — `message` says why, and the state fields are still current.
    pub ok: bool,
    /// The host believes Kindle's Read Aloud is on.
    pub reading: bool,
    /// Playback is stalled mid-page.
    pub paused: bool,
    /// A `Kindle.exe` process is running.
    pub kindle_running: bool,
    /// A reading command is in flight on the host's Kindle-control thread — the panel
    /// leaves its own switch alone until this clears rather than fighting the transition.
    pub busy: bool,
    /// Kokoro is narrating *Kindle* right now (debounced across page gaps). The host tells
    /// the sources apart itself, so the panel's own Preview never shows up here.
    pub kindle_speaking: bool,
    /// The host has a page in flight for Kindle — synthesizing it or streaming it. Covers the
    /// seconds between Kindle's narrator asking and the first audio existing, which is the
    /// one stretch `kindle_speaking` cannot see (it times audio, and there is none yet).
    pub kindle_synth: bool,
    /// Kokoro is producing audio for *anyone* — Kindle, the browser extension over loopback
    /// HTTP, or this panel's own Preview. Kept alongside the narrower flag rather than
    /// derived from it, because the two answer different questions: what to *display* is
    /// Kindle's narration, but what may not be interrupted is any of them. They share the
    /// one serialized synth worker, and a speed test holds it for tens of seconds.
    pub any_speaking: bool,
    /// A sentence to show the user; empty when there's nothing to say (every query).
    pub message: String,
}

/// Send one `CMD_KINDLE` action and return the host's authoritative state.
///
/// Blocking — call it on a background thread. A `KINDLE_QUERY` returns almost immediately
/// (the host answers it inline off atomics, so it never queues behind synthesis, a
/// benchmark, or an in-flight Play); Play/Stop/Close take as long as driving Kindle takes.
pub fn send(action: u8, busy_wait: Duration) -> Result<HostReport, String> {
    let mut pipe = connect(busy_wait)?;
    pipe.write_all(&[CMD_KINDLE, action]).map_err(|e| format!("pipe write: {e}"))?;
    pipe.flush().map_err(|e| format!("pipe flush: {e}"))?;

    // [u8 result][u8 state][u32 msSinceAudio][u32 msSinceKindleAudio][u16 msgLen][utf8 msg]
    let mut head = [0u8; 12];
    pipe.read_exact(&mut head).map_err(|e| format!("pipe read: {e}"))?;
    let state = head[1];
    let any_ms = u32::from_le_bytes(head[2..6].try_into().unwrap());
    let kindle_ms = u32::from_le_bytes(head[6..10].try_into().unwrap());
    let msg_len = u16::from_le_bytes(head[10..12].try_into().unwrap());
    if msg_len > MAX_MSG_BYTES {
        return Err("the host sent an oversized reply.".to_string());
    }
    let mut msg = vec![0u8; msg_len as usize];
    pipe.read_exact(&mut msg).map_err(|e| format!("pipe read: {e}"))?;

    Ok(HostReport {
        ok: head[0] == KINDLE_OK,
        reading: state & STATE_READING != 0,
        paused: state & STATE_PAUSED != 0,
        kindle_running: state & STATE_KINDLE_RUNNING != 0,
        busy: state & STATE_BUSY != 0,
        kindle_synth: state & STATE_KINDLE_SYNTH != 0,
        // The debounce lives in kokoro-protocol so the host and every client read the same
        // elapsed figure the same way.
        kindle_speaking: kindle_ms < SPEAKING_DEBOUNCE_MS,
        any_speaking: any_ms < SPEAKING_DEBOUNCE_MS,
        message: String::from_utf8_lossy(&msg).into_owned(),
    })
}
