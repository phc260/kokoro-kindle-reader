// Toggle Kindle's "Assistive reader" (Read Aloud) from the panel, hands-free.
//
// Kindle for PC 1.0.18632.0 exposes Read Aloud as the in-reader keyboard shortcut
// Ctrl+A ("Enable assistive reader"), which is a TOGGLE: odd presses start reading,
// even presses stop. We drive it by foregrounding the x86 Kindle window (from the x64
// panel, across the bitness boundary) and synthesizing Ctrl+A with raw Win32 SendInput.
//
// Why not the UIA ToggleButton? Kindle's assistive-reader toggle
// (AutomationId "ToggleButton-Assistive reader toggle") lives inside the Page-settings
// ("Aa") flyout, which can't be opened programmatically on this build (its button has no
// Invoke and a no-op ExpandCollapse). So the toggle is unreachable when the menu is
// closed — the Ctrl+A shortcut bypasses the menu entirely. See the read-aloud-ctrl-a
// project note for the full rationale and the landmines below.
//
// Landmines (each caused intermittent misses; all avoided here):
//   * Foregrounding is REQUIRED — a synthesized Ctrl+A sent to a background Kindle is
//     dropped. We use the AttachThreadInput trick (no ALT-tap: a bare ALT puts Kindle's
//     window into menu mode and swallows the next keystroke).
//   * We do NOT call SetFocus / UIA set_focus: that steals focus from Kindle's content
//     child, so the shortcut misses. SetForegroundWindow restores the window's own child
//     focus (the reader), and raw SendInput lands there.
//   * If the Aa flyout OR the Table-of-contents flyout is open, it's a focus trap that
//     eats Ctrl+A (the user may have opened either one by hand). We dismiss whichever is
//     open first: both are Kindle's "SideMenu" flyout component (same focus-trap-sentinel
//     structure), and light-dismiss on Escape. Presence is observable via a distinctive
//     child AutomationId each hosts - the assistive-reader toggle for Aa (same signal
//     read_state() uses), the "ToC" group itself for the contents panel. So we press
//     Escape until neither is present, then send Ctrl+A.
//
// State readback is unreliable (the toggle is only in the tree while the Aa menu is
// open), so the panel switch is a blind toggle that tracks its own intent; read_state()
// is a best-effort sync used only when the menu happens to be open.
//
// Blocking + COM-heavy; call it on a background thread (COM is initialised per thread by
// UIAutomation::new()), never on the Slint UI thread.

use std::mem::size_of;
use std::time::Duration;

use uiautomation::filters::FnFilter;
use uiautomation::patterns::UITogglePattern;
use uiautomation::types::ToggleState;
use uiautomation::{UIAutomation, UIElement};
use windows::core::BOOL;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

const TOGGLE_ID: &str = "ToggleButton-Assistive reader toggle";
// The Table-of-contents flyout has no toggle to key off (unlike the Aa flyout), but the
// flyout's own AutomationId is just as distinctive and only appears while it's open.
const TOC_ID: &str = "ToC";
const TARGET_EXE: &str = "Kindle.exe";

/// PID of the first process named `name`, if running. Matches the process **image
/// name**, not a window title — locale-independent, unlike the UIA "Kindle"-named-window
/// matcher this module otherwise uses. Same technique as kokoro-host's kindle-watch and
/// kokoro-inject, which already rely on it to find Kindle.
fn find_pid(name: &str) -> Option<u32> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut pe = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        if Process32FirstW(snap, &mut pe).is_ok() {
            loop {
                let end = pe.szExeFile.iter().position(|&c| c == 0).unwrap_or(pe.szExeFile.len());
                if String::from_utf16_lossy(&pe.szExeFile[..end]).eq_ignore_ascii_case(name) {
                    found = Some(pe.th32ProcessID);
                    break;
                }
                if Process32NextW(snap, &mut pe).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// Find a visible top-level window owned by `pid` — Kindle's main window. A process can
/// own several windows (tooltips, hidden helpers); filtering on `IsWindowVisible` picks
/// the one that's actually on screen.
fn find_window_for_pid(pid: u32) -> Option<HWND> {
    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
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
        // EnumWindows returns an error when the callback stops early (BOOL(0)) — that's
        // our success path, not a failure, so the result is intentionally discarded.
        let _ = EnumWindows(Some(callback), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.1
}

/// Drive Kindle's Assistive reader (Read Aloud) to `want` (true = reading) by
/// foregrounding Kindle and sending its Ctrl+A toggle shortcut. Since Ctrl+A blindly
/// toggles (we can't read Kindle's true state), this fires exactly one toggle — correct
/// because the panel switch only calls us on an actual change. Returns `want` on success,
/// or an Err with a user-facing message if Kindle can't be reached.
pub fn set_read_aloud(want: bool) -> Result<bool, String> {
    let auto = UIAutomation::new().map_err(|e| format!("UI Automation init failed: {e}"))?;
    let kindle = auto
        .create_matcher()
        .name("Kindle")
        .timeout(2000)
        .find_first()
        .map_err(|_| "Kindle window not found - is Kindle running?".to_string())?;

    let hwnd = kindle
        .get_native_window_handle()
        .map_err(|e| format!("couldn't get Kindle's window handle: {e}"))?;
    foreground(hwnd.into());
    // Let the foreground switch settle so the keystroke lands on Kindle's reader.
    std::thread::sleep(Duration::from_millis(300));
    // If the Aa or ToC flyout is open it traps Ctrl+A; dismiss it first (see module note).
    dismiss_open_flyout(&auto, &kindle);
    send_ctrl_a();

    Ok(want)
}

/// Best-effort read of Kindle's Assistive reader state, without disturbing the UI.
/// The toggle is only in the UIA tree while the Aa menu is open, so this returns `None`
/// in normal reading (menu closed) — callers must treat `None` as "unknown, keep the
/// last known state", not "off". Used to sync the switch if the user opens the Aa menu
/// and flips Read Aloud there directly.
pub fn read_state() -> Option<bool> {
    let auto = UIAutomation::new().ok()?;
    let kindle = auto
        .create_matcher()
        .name("Kindle")
        .timeout(1000)
        .find_first()
        .ok()?;
    let toggle = find_by_id(&auto, &kindle, TOGGLE_ID)?;
    let pattern: UITogglePattern = toggle.get_pattern().ok()?;
    match pattern.get_toggle_state() {
        Ok(ToggleState::On) => Some(true),
        Ok(ToggleState::Off) => Some(false),
        _ => None,
    }
}

/// Close Kindle — for testing kindle-watch's hook re-injection, which only fires on
/// Kindle's *next launch* (the in-memory SetVoice patch has no persistence, so flipping
/// "Narrate Kindle with Kokoro" while Kindle is already open has no effect until it
/// restarts). Relaunching is left to the user — MSIX/Desktop-Bridge packaged apps aren't
/// reliably relaunched via a raw CreateProcess on their exe path, so this only closes.
/// Best-effort: returns Err with a user-facing message on any failure; never panics.
/// Blocking — call on a background thread, same as the rest of this module.
pub fn close() -> Result<(), String> {
    use windows::Win32::Foundation::WPARAM;
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

    let pid = find_pid(TARGET_EXE).ok_or_else(|| "Kindle isn't running.".to_string())?;
    let hwnd =
        find_window_for_pid(pid).ok_or_else(|| "couldn't find Kindle's window.".to_string())?;

    // WM_CLOSE (not TerminateProcess) so Kindle gets to save/prompt like a normal quit.
    unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }
        .map_err(|e| format!("couldn't close Kindle: {e}"))
}

/// Force `hwnd` to the foreground, defeating Windows' foreground-lock (which blocks a
/// background process from calling SetForegroundWindow). The AttachThreadInput trick:
/// briefly share input state with the current foreground thread + the target thread so
/// the OS treats our SetForegroundWindow as user-initiated. Deliberately does NOT touch
/// keyboard focus (no SetFocus, no ALT-tap): Windows restores the window's own last
/// focused child (Kindle's reader content), so a following Ctrl+A lands there. An
/// ALT-tap would instead put Kindle's window into menu mode and swallow the shortcut.
fn foreground(hwnd: windows::Win32::Foundation::HWND) {
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
/// flyout component and light-dismiss on Escape; each is open exactly when its
/// distinctive child (the assistive-reader toggle, or the ToC group itself) is present in
/// the UIA tree. Press Escape until neither is present (up to a few tries), so we never
/// send a stray Escape into the reader when no flyout is open. Best-effort: if it won't
/// close we fall through and send Ctrl+A anyway.
fn dismiss_open_flyout(auto: &UIAutomation, kindle: &UIElement) {
    for _ in 0..3 {
        let open = find_by_id(auto, kindle, TOGGLE_ID).is_some()
            || find_by_id(auto, kindle, TOC_ID).is_some();
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
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_ESCAPE,
    };
    fn key(vk: u16, up: bool) -> INPUT {
        INPUT {
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
        }
    }
    let inputs = [key(VK_ESCAPE.0, false), key(VK_ESCAPE.0, true)];
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Synthesize Ctrl+A with raw SendInput (virtual keys), bypassing the uiautomation
/// crate's send_keys - which calls UIA SetFocus on the top-level window first, stealing
/// focus from Kindle's content child and making the shortcut miss. Goes to whatever has
/// focus in the (already-foregrounded) window: Kindle's reader.
fn send_ctrl_a() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_CONTROL,
    };
    const VK_A: u16 = 0x41;
    fn key(vk: u16, up: bool) -> INPUT {
        INPUT {
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
        }
    }
    let inputs = [
        key(VK_CONTROL.0, false),
        key(VK_A, false),
        key(VK_A, true),
        key(VK_CONTROL.0, true),
    ];
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Find a descendant of `root` whose AutomationId equals `id` (the crate's matcher
/// has no built-in AutomationId filter, so use a closure filter).
fn find_by_id(auto: &UIAutomation, root: &UIElement, id: &'static str) -> Option<UIElement> {
    auto.create_matcher()
        .from(root.clone())
        .filter(Box::new(FnFilter {
            filter: Box::new(move |e: &UIElement| {
                Ok(e.get_automation_id().map(|got| got.as_str() == id).unwrap_or(false))
            }),
        }))
        .timeout(1000)
        .find_first()
        .ok()
}
