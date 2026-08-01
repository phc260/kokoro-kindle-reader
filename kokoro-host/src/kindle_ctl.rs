// The host's Kindle-control thread: the ONE place in the project that touches Kindle's
// UI. Moved here from the settings panel, which used to drive UI Automation itself — the
// panel now sends intent over the pipe (`CMD_KINDLE`) and renders what comes back, so
// there is a single authority on what Kindle is doing and only one process that can be
// wrong about it.
//
// Kindle for PC 1.0.18632.0 exposes Read Aloud as the in-reader keyboard shortcut Ctrl+A
// ("Enable assistive reader"), which is a TOGGLE: odd presses start reading, even presses
// stop. We drive it by foregrounding the x86 Kindle window (from this x64 host, across the
// bitness boundary) and synthesizing Ctrl+A with raw Win32 SendInput.
//
// Why not the UIA ToggleButton? Kindle's assistive-reader toggle (AutomationId
// "ToggleButton-Assistive reader toggle") lives inside the Page-settings ("Aa") flyout,
// which can't be opened programmatically on this build (its button has no Invoke and a
// no-op ExpandCollapse). So the toggle is unreachable when the menu is closed — the Ctrl+A
// shortcut bypasses the menu entirely. See the read-aloud-ctrl-a project note.
//
// Landmines (each caused intermittent misses; all avoided here):
//   * Foregrounding is REQUIRED — a synthesized Ctrl+A sent to a background Kindle is
//     dropped. We use the AttachThreadInput trick (no ALT-tap: a bare ALT puts Kindle's
//     window into menu mode and swallows the next keystroke).
//   * We do NOT call SetFocus / UIA set_focus: that steals focus from Kindle's content
//     child, so the shortcut misses. SetForegroundWindow restores the window's own child
//     focus (the reader), and raw SendInput lands there.
//   * If the Aa flyout OR the Table-of-contents flyout is open, it's a focus trap that eats
//     Ctrl+A (the user may have opened either by hand). We dismiss whichever is open first:
//     both are Kindle's "SideMenu" flyout component and light-dismiss on Escape, and each
//     is observable via a distinctive child AutomationId. So we press Escape until neither
//     is present, then send Ctrl+A.
//
// Because Ctrl+A is a blind toggle, "start reading" is only as good as the host's *belief*
// about the current state — so a command refreshes that belief from evidence first
// (`refresh`) and then toggles only if the belief still disagrees with the request.
//
// That belief is NEVER read back off Kindle's own toggle. We used to: the same
// "ToggleButton-Assistive reader toggle" above was sampled through UIA and adopted as
// definite whenever it happened to be in the tree. But it is only in the tree while the Aa
// menu is open, and the Aa menu is open precisely when the user is reaching for that toggle
// themselves — so the one moment it could be read was the one moment it was changing. A
// sample landing between the user's tap and Kindle repainting recorded the stale value as
// definite, `set_reading` trusts a definite belief enough to skip its Ctrl+A altogether, and
// the wrong belief outlived the menu it came from. Reading it at all is the race; `refresh`
// works from the process list and the Kindle audio clock instead, neither of which anything
// else is mutating. The toggle's AutomationId is still used — but only to notice the flyout
// is OPEN so it can be dismissed, never to ask what it says.
//
// Threading: every UIA call here is blocking and COM-heavy, so all of it runs on one
// dedicated OS thread, off the tokio pipe runtime and off the serialized synth worker —
// which is what lets a `CMD_KINDLE` heartbeat or a Stop answer immediately while a Play is
// still working. That thread owns its `UIAutomation` (and therefore its COM apartment) for
// its lifetime. A background `refresh` no longer touches UIA at all, so a heartbeat's
// coalesced refresh costs a process enumeration rather than a matcher timeout.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use uiautomation::filters::FnFilter;
use uiautomation::{UIAutomation, UIElement};
use windows::Win32::Foundation::{HWND, LPARAM};

use crate::kindle_watch::{find_pid, TARGET};
use crate::state::{now_ms, HostState};

const TOGGLE_ID: &str = "ToggleButton-Assistive reader toggle";
// The Table-of-contents flyout has no toggle to key off (unlike the Aa flyout), but the
// flyout's own AutomationId is just as distinctive and only appears while it's open.
const TOC_ID: &str = "ToC";

/// Matcher timeout for a *command* — long enough to ride out Kindle repainting.
const CMD_TIMEOUT_MS: u64 = 2000;
/// Floor on how often a query may make the control thread go and look at Kindle. The
/// panel's heartbeat is ~1 Hz; this keeps a second panel, or a faster client, from turning
/// the refresh into a spin.
const MIN_REFRESH_MS: u64 = 750;

/// Work for the control thread.
enum Job {
    /// Drive Read Aloud to `want`.
    SetReading {
        want: bool,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// Ask Kindle to close (so a `kindle_kokoro` change lands on its next launch).
    Close {
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// Re-read Kindle's state into [`HostState`]. Advisory — nobody waits for it.
    Refresh,
}

/// Handle to the Kindle-control thread. Cheap to clone (one per pipe connection).
#[derive(Clone)]
pub struct KindleCtl {
    tx: mpsc::Sender<Job>,
    state: Arc<HostState>,
    /// A refresh is already queued; a second would only make the thread repeat itself.
    refresh_pending: Arc<AtomicBool>,
    /// When the last refresh finished, for [`MIN_REFRESH_MS`].
    last_refresh_ms: Arc<AtomicU64>,
}

impl KindleCtl {
    /// Spawn the control thread and return its handle.
    pub fn spawn(state: Arc<HostState>) -> KindleCtl {
        let (tx, rx) = mpsc::channel::<Job>();
        let refresh_pending = Arc::new(AtomicBool::new(false));
        let last_refresh_ms = Arc::new(AtomicU64::new(0));
        {
            let state = state.clone();
            let refresh_pending = refresh_pending.clone();
            let last_refresh_ms = last_refresh_ms.clone();
            std::thread::Builder::new()
                .name("kokoro-kindle-ctl".into())
                .spawn(move || worker_loop(rx, state, refresh_pending, last_refresh_ms))
                .expect("spawn kindle control thread");
        }
        KindleCtl { tx, state, refresh_pending, last_refresh_ms }
    }

    /// Drive Read Aloud to `want` and wait for the host's own verdict. Awaited from the
    /// pipe task, so the connection stays responsive while the control thread works.
    pub async fn set_reading(&self, want: bool) -> Result<(), String> {
        self.state.set_kindle_busy(true);
        let (reply, rx) = tokio::sync::oneshot::channel();
        if self.tx.send(Job::SetReading { want, reply }).is_err() {
            self.state.set_kindle_busy(false);
            return Err("the Kindle control thread has stopped.".to_string());
        }
        match rx.await {
            Ok(r) => r,
            // The worker dropped the reply without answering (it panicked, or the host is
            // shutting down). `busy` is cleared by the worker on every normal path, so
            // clear it here for the path where the worker never got that far.
            Err(_) => {
                self.state.set_kindle_busy(false);
                Err("the Kindle control thread stopped mid-command.".to_string())
            }
        }
    }

    /// Ask Kindle to close. Same round trip as [`Self::set_reading`].
    pub async fn close_kindle(&self) -> Result<(), String> {
        self.state.set_kindle_busy(true);
        let (reply, rx) = tokio::sync::oneshot::channel();
        if self.tx.send(Job::Close { reply }).is_err() {
            self.state.set_kindle_busy(false);
            return Err("the Kindle control thread has stopped.".to_string());
        }
        match rx.await {
            Ok(r) => r,
            Err(_) => {
                self.state.set_kindle_busy(false);
                Err("the Kindle control thread stopped mid-command.".to_string())
            }
        }
    }

    /// Nudge the control thread to re-read Kindle's state, if it hasn't lately. Returns
    /// immediately — the caller answers from the cached state, which is what keeps a
    /// heartbeat from ever waiting on UI Automation. Coalesced and rate-limited, so a
    /// client polling faster than [`MIN_REFRESH_MS`] costs nothing extra.
    pub fn request_refresh(&self) {
        if self.state.kindle_busy() {
            return; // a command is mid-flight; its own refresh is more current
        }
        if now_ms().saturating_sub(self.last_refresh_ms.load(Ordering::Relaxed)) < MIN_REFRESH_MS {
            return;
        }
        if self.refresh_pending.swap(true, Ordering::SeqCst) {
            return; // one is already queued
        }
        if self.tx.send(Job::Refresh).is_err() {
            self.refresh_pending.store(false, Ordering::SeqCst);
        }
    }
}

fn worker_loop(
    rx: mpsc::Receiver<Job>,
    state: Arc<HostState>,
    refresh_pending: Arc<AtomicBool>,
    last_refresh_ms: Arc<AtomicU64>,
) {
    // One `UIAutomation` for the thread's lifetime: it initializes COM on this thread, and
    // this thread is its only user. Built lazily so a machine where UIA won't start still
    // gets a running host (every command then reports the failure instead of panicking).
    let mut auto: Option<UIAutomation> = None;
    while let Ok(job) = rx.recv() {
        match job {
            Job::Refresh => {
                refresh_pending.store(false, Ordering::SeqCst);
                // No `automation()` here any more: `refresh` reads a pid and an audio clock,
                // so a background tick never builds or touches the UIA tree.
                refresh(&state);
                last_refresh_ms.store(now_ms(), Ordering::Relaxed);
            }
            Job::SetReading { want, reply } => {
                let res = match automation(&mut auto) {
                    Some(a) => set_reading(a, &state, want),
                    None => Err(uia_unavailable()),
                };
                last_refresh_ms.store(now_ms(), Ordering::Relaxed);
                // Order is load-bearing: `set_reading` has already written the new belief by
                // the time this clears `busy`. A client's heartbeat that sees `busy` false
                // therefore reads the *post*-command state, never the one it replaced — which
                // is what lets the panel trust any report captured after a command returns.
                // Clearing busy first would open a window where a query answers with the old
                // reading and nothing marks it as in-flight.
                state.set_kindle_busy(false);
                let _ = reply.send(res);
            }
            Job::Close { reply } => {
                let res = close_kindle();
                // Kindle going away changes the reading state; re-read it rather than
                // leaving the panel showing a switch for a reader that no longer exists.
                refresh(&state);
                last_refresh_ms.store(now_ms(), Ordering::Relaxed);
                state.set_kindle_busy(false);
                let _ = reply.send(res);
            }
        }
    }
}

fn uia_unavailable() -> String {
    "Windows UI Automation isn't available, so Kindle can't be controlled.".to_string()
}

/// The thread's `UIAutomation`, built on first use and retried on a later call if that
/// failed.
fn automation(slot: &mut Option<UIAutomation>) -> Option<&UIAutomation> {
    if slot.is_none() {
        match UIAutomation::new() {
            Ok(a) => *slot = Some(a),
            Err(e) => {
                eprintln!("[host] kindle-ctl: UI Automation init failed: {e}");
                return None;
            }
        }
    }
    slot.as_ref()
}

/// Re-read what Kindle is doing into [`HostState`]. Best-effort by design, and it **touches
/// no UI Automation at all**.
///
/// Order of evidence:
///   1. Kindle isn't running -> not reading, not paused. Definite.
///   2. Kokoro is streaming audio *for Kindle* -> reading. One-way only: quiet is not
///      proof of stopped (page gaps, a pause, slow synthesis all go quiet).
///
/// (2) is why the host can do this at all well: it knows which of its own streams was
/// Kindle's, because `CMD_PREVIEW` exists to keep the panel's own synth off that clock.
///
/// There used to be a step between those two: read Kindle's assistive-reader toggle through
/// UIA and adopt it as definite. It is gone, and deliberately. That element is only in the
/// tree while the **Aa menu is open**, which is exactly when the user is reaching for the
/// toggle themselves — so the one moment it could be read was the one moment it was being
/// changed underneath the read. A sample taken between the user's tap and Kindle repainting
/// the toggle recorded the *old* value as definite, and `set_reading` trusts a definite read
/// enough to skip its Ctrl+A entirely. Against a blind toggle that inverts Play and Stop, and
/// the wrong belief outlives the menu it came from. Being right rarely and confidently wrong
/// occasionally is worse here than not looking: the pid check above still voids the belief
/// whenever Kindle restarts, and the audio clock still proves reading positively.
///
/// It was also the only UIA in this function, so a background refresh now costs a process
/// enumeration instead of a matcher timeout paid in full on every tick.
fn refresh(state: &HostState) {
    let pid = find_pid(TARGET);
    state.set_kindle_pid(pid);
    if pid.is_none() {
        return;
    }
    // One-way, and it never argues with a command: the evidence is a Kindle synth stream that
    // opened *after* the last Play/Stop, so the stream a Stop interrupted proves nothing about
    // the present however long Kindle takes to abandon it.
    if state.stream_proves_reading() {
        state.set_reading(true);
    }
}

/// Drive Kindle's Assistive reader to `want`. Refreshes the host's belief from evidence
/// first, then fires exactly one Ctrl+A toggle if the belief still disagrees — so a
/// repeated Play can't toggle reading *off*, which a blind toggle would.
fn set_reading(auto: &UIAutomation, state: &HostState, want: bool) -> Result<(), String> {
    refresh(state);
    if !state.kindle_running() {
        return Err("Kindle isn't running - open it and try again.".to_string());
    }
    if state.reading() == want {
        // Already there, so no Ctrl+A — but stamp anyway. We have just asserted what the
        // state is, and without the stamp the same draining audio would talk us back out of
        // it on the next refresh.
        state.set_reading_commanded(want);
        return Ok(());
    }

    let kindle = auto
        .create_matcher()
        .name("Kindle")
        .timeout(CMD_TIMEOUT_MS)
        .find_first()
        .map_err(|_| "Kindle's window couldn't be found - is a book open?".to_string())?;
    let hwnd = kindle
        .get_native_window_handle()
        .map_err(|e| format!("couldn't get Kindle's window handle: {e}"))?;
    foreground(hwnd.into());
    // Let the foreground switch settle so the keystroke lands on Kindle's reader.
    std::thread::sleep(Duration::from_millis(300));
    // If the Aa or ToC flyout is open it traps Ctrl+A; dismiss it first (see module note).
    dismiss_open_flyout(auto, &kindle);
    send_ctrl_a();
    // `_commanded` so the audio still draining from what we just stopped can't argue with it.
    state.set_reading_commanded(want);
    Ok(())
}

/// Close Kindle so a `kindle_kokoro` change can land on its next launch (the in-memory
/// SetVoice patch has no persistence, so flipping the flag while Kindle is open does
/// nothing until it restarts). Relaunching is left to the user — MSIX/Desktop-Bridge
/// packaged apps aren't reliably relaunched via a raw CreateProcess on their exe path.
fn close_kindle() -> Result<(), String> {
    use windows::Win32::Foundation::WPARAM;
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

    let pid = find_pid(TARGET).ok_or_else(|| "Kindle isn't running.".to_string())?;
    let hwnd =
        find_window_for_pid(pid).ok_or_else(|| "couldn't find Kindle's window.".to_string())?;

    // WM_CLOSE (not TerminateProcess) so Kindle gets to save/prompt like a normal quit.
    unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }
        .map_err(|e| format!("couldn't close Kindle: {e}"))
}

/// Find a visible top-level window owned by `pid` — Kindle's main window. A process can own
/// several windows (tooltips, hidden helpers); filtering on `IsWindowVisible` picks the one
/// that's actually on screen.
fn find_window_for_pid(pid: u32) -> Option<HWND> {
    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        use windows::core::BOOL;
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindowVisible};
        let ctx = unsafe { &mut *(lparam.0 as *mut (u32, Option<HWND>)) };
        let mut window_pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut window_pid)) };
        if window_pid == ctx.0 && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            ctx.1 = Some(hwnd);
            return BOOL(0); // found it — stop enumeration
        }
        BOOL(1) // keep going
    }
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;
    let mut ctx: (u32, Option<HWND>) = (pid, None);
    unsafe {
        // EnumWindows returns an error when the callback stops early (BOOL(0)) — that's our
        // success path, not a failure, so the result is intentionally discarded.
        let _ = EnumWindows(Some(callback), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.1
}

/// Force `hwnd` to the foreground, defeating Windows' foreground-lock (which blocks a
/// background process from calling SetForegroundWindow). The AttachThreadInput trick:
/// briefly share input state with the current foreground thread + the target thread so the
/// OS treats our SetForegroundWindow as user-initiated. Deliberately does NOT touch
/// keyboard focus (no SetFocus, no ALT-tap): Windows restores the window's own last focused
/// child (Kindle's reader content), so a following Ctrl+A lands there. An ALT-tap would
/// instead put Kindle's window into menu mode and swallow the shortcut.
fn foreground(hwnd: HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic,
        SetForegroundWindow, ShowWindow, SW_RESTORE,
    };
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let fg = GetForegroundWindow();
        let target_thread = GetWindowThreadProcessId(hwnd, None);
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let cur = GetCurrentThreadId();
        let _ = AttachThreadInput(cur, target_thread, true);
        let _ = AttachThreadInput(cur, fg_thread, true);
        let _ = SetForegroundWindow(hwnd);
        let _ = BringWindowToTop(hwnd);
        let _ = AttachThreadInput(cur, fg_thread, false);
        let _ = AttachThreadInput(cur, target_thread, false);
    }
}

/// If Kindle's Page-settings ("Aa") or Table-of-contents flyout is open it swallows the
/// Ctrl+A toggle, so close it before sending the shortcut. Both are the same "SideMenu"
/// flyout component and light-dismiss on Escape; each is open exactly when its distinctive
/// child (the assistive-reader toggle, or the ToC group itself) is present in the UIA tree.
/// Press Escape until neither is present (up to a few tries), so we never send a stray
/// Escape into the reader when no flyout is open. Best-effort: if it won't close we fall
/// through and send Ctrl+A anyway.
fn dismiss_open_flyout(auto: &UIAutomation, kindle: &UIElement) {
    for _ in 0..3 {
        let open = find_by_id(auto, kindle, TOGGLE_ID, CMD_TIMEOUT_MS).is_some()
            || find_by_id(auto, kindle, TOC_ID, CMD_TIMEOUT_MS).is_some();
        if !open {
            return; // no flyout open — leave the reader untouched
        }
        send_escape();
        std::thread::sleep(Duration::from_millis(1000));
    }
}

/// Synthesize a single Escape keypress with raw SendInput (same rationale as send_ctrl_a:
/// bypass the uiautomation crate's focus-stealing send_keys). Lands on Kindle's reader,
/// light-dismissing any open flyout.
fn send_escape() {
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
    send_keys(&[(VK_ESCAPE.0, false), (VK_ESCAPE.0, true)]);
}

/// Synthesize Ctrl+A with raw SendInput (virtual keys), bypassing the uiautomation crate's
/// send_keys - which calls UIA SetFocus on the top-level window first, stealing focus from
/// Kindle's content child and making the shortcut miss. Goes to whatever has focus in the
/// (already-foregrounded) window: Kindle's reader.
fn send_ctrl_a() {
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL;
    const VK_A: u16 = 0x41;
    send_keys(&[
        (VK_CONTROL.0, false),
        (VK_A, false),
        (VK_A, true),
        (VK_CONTROL.0, true),
    ]);
}

/// Send a raw key sequence as `(virtual-key, is_key_up)` pairs.
fn send_keys(keys: &[(u16, bool)]) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VIRTUAL_KEY,
    };
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(vk, up)| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        })
        .collect();
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Find a descendant of `root` whose AutomationId equals `id` (the crate's matcher has no
/// built-in AutomationId filter, so use a closure filter).
fn find_by_id(
    auto: &UIAutomation,
    root: &UIElement,
    id: &'static str,
    timeout_ms: u64,
) -> Option<UIElement> {
    auto.create_matcher()
        .from(root.clone())
        .filter(Box::new(FnFilter {
            filter: Box::new(move |e: &UIElement| {
                Ok(e.get_automation_id().map(|got| got.as_str() == id).unwrap_or(false))
            }),
        }))
        .timeout(timeout_ms)
        .find_first()
        .ok()
}
