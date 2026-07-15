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
//   * If the Aa flyout is open it's a focus trap that eats Ctrl+A (the user typically
//     opened it to reach the Read Aloud toggle by hand). We dismiss it first: the flyout
//     light-dismisses on Escape, and its presence is observable because the assistive-
//     reader toggle is only in the UIA tree while the flyout is open (same signal
//     read_state() uses). So we press Escape until the toggle vanishes, then send Ctrl+A.
//
// State readback is unreliable (the toggle is only in the tree while the Aa menu is
// open), so the panel switch is a blind toggle that tracks its own intent; read_state()
// is a best-effort sync used only when the menu happens to be open.
//
// Blocking + COM-heavy; call it on a background thread (COM is initialised per thread by
// UIAutomation::new()), never on the Slint UI thread.

use std::time::Duration;

use uiautomation::filters::FnFilter;
use uiautomation::patterns::UITogglePattern;
use uiautomation::types::ToggleState;
use uiautomation::{UIAutomation, UIElement};

const TOGGLE_ID: &str = "ToggleButton-Assistive reader toggle";

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
    // If the Aa flyout is open it traps Ctrl+A; dismiss it first (see module note).
    dismiss_aa_flyout(&auto, &kindle);
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
        .timeout(800)
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

/// If Kindle's Page-settings ("Aa") flyout is open it swallows the Ctrl+A toggle, so
/// close it before sending the shortcut. The flyout light-dismisses on Escape, and it's
/// open exactly when the assistive-reader toggle is present in the UIA tree (the flyout
/// hosts it). Press Escape until the toggle disappears (up to a few tries), so we never
/// send a stray Escape into the reader when no flyout is open. Best-effort: if it won't
/// close we fall through and send Ctrl+A anyway.
fn dismiss_aa_flyout(auto: &UIAutomation, kindle: &UIElement) {
    for _ in 0..3 {
        if find_by_id(auto, kindle, TOGGLE_ID).is_none() {
            return; // no flyout open — leave the reader untouched
        }
        send_escape();
        std::thread::sleep(Duration::from_millis(200));
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
