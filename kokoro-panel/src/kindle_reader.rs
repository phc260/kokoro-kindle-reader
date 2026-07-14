// Toggle Kindle's "Assistive reader" (Read Aloud) from the panel via UI Automation.
//
// The new Kindle for Windows exposes its reader control as a UIA ToggleButton
// (AutomationId "ToggleButton-Assistive reader toggle") that lives in the Page-
// settings ("aa") menu. We find Kindle's window, find that toggle — opening the aa
// menu first if it isn't already in the tree — and flip it with the TogglePattern.
// UI Automation is an OS-level service, so the x64 panel drives the x86 Kindle
// across the bitness boundary with no hooking. Brittle across Kindle updates: if
// Amazon renames the control this fails gracefully with a message.
//
// Blocking + COM-heavy; call it on a background thread (COM is initialised per
// thread by UIAutomation::new()), never on the Slint UI thread.

use std::time::Duration;
use uiautomation::filters::FnFilter;
use uiautomation::patterns::{UIInvokePattern, UITogglePattern};
use uiautomation::types::ToggleState;
use uiautomation::{UIAutomation, UIElement};

const TOGGLE_ID: &str = "ToggleButton-Assistive reader toggle";
const AA_MENU_ID: &str = "aaMenuButton";

/// Drive Kindle's Assistive reader (Read Aloud) to `want` (true = reading). Reads
/// the control's current state and only flips it when needed, so the panel switch
/// stays correct even if Read Aloud was toggled inside Kindle directly. Returns the
/// resulting state. Err with a user-facing message on any failure.
pub fn set_read_aloud(want: bool) -> Result<bool, String> {
    let auto = UIAutomation::new().map_err(|e| format!("UI Automation init failed: {e}"))?;
    let kindle = auto
        .create_matcher()
        .name("Kindle")
        .timeout(2000)
        .find_first()
        .map_err(|_| "Kindle window not found — is Kindle running?".to_string())?;

    // The toggle is usually already in the tree; if not, open the Page-settings menu
    // (which hosts it) and look again.
    let toggle = match find_by_id(&auto, &kindle, TOGGLE_ID) {
        Some(t) => t,
        None => {
            open_aa_menu(&auto, &kindle);
            find_by_id(&auto, &kindle, TOGGLE_ID).ok_or_else(|| {
                "Couldn't find Kindle's Assistive reader control — open a book first.".to_string()
            })?
        }
    };

    let pattern: UITogglePattern = toggle
        .get_pattern()
        .map_err(|e| format!("no toggle pattern on the control: {e}"))?;
    let currently_on = matches!(pattern.get_toggle_state(), Ok(ToggleState::On));
    if currently_on != want {
        pattern
            .toggle()
            .map_err(|e| format!("toggling Read Aloud failed: {e}"))?;
    }

    // We drove it to `want`. Don't re-read to confirm: Kindle updates the toggle's
    // UIA state asynchronously, so an immediate re-read can still report the old
    // value and make the panel switch bounce back.
    Ok(want)
}


/// Best-effort read of Kindle's Assistive reader state, without disturbing the UI
/// (never opens the aa menu). Used to keep the panel switch in sync when Read Aloud
/// is toggled inside Kindle directly. Returns `None` when Kindle or the toggle isn't
/// currently in the UIA tree (e.g. the toolbar is hidden) — callers must treat that
/// as "unknown, keep the last known state", not "off".
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

fn open_aa_menu(auto: &UIAutomation, kindle: &UIElement) {
    if let Some(aa) = find_by_id(auto, kindle, AA_MENU_ID) {
        if let Ok(invoke) = aa.get_pattern::<UIInvokePattern>() {
            let _ = invoke.invoke();
            std::thread::sleep(Duration::from_millis(400)); // let the menu render
        }
    }
}
