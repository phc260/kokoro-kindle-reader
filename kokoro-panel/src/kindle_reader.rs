// Toggle Kindle's "Assistive reader" (Read Aloud) from the panel via UI Automation.
//
// Kindle for Windows exposes its reader control as a UIA ToggleButton (AutomationId
// "ToggleButton-Assistive reader toggle") that lives inside the Page-settings ("Aa")
// menu. UI Automation is an OS-level service, so the x64 panel drives the x86 Kindle
// across the bitness boundary with no hooking, and TogglePattern::toggle() flips the
// control cleanly.
//
// Caveat (Kindle 1.0.18632.0): that Aa flyout can't be opened programmatically. Its
// button exposes no Invoke pattern, and its ExpandCollapse pattern is a no-op (it
// reports state but won't actually open/close the menu); the flyout opens only on a
// real click. So the toggle is reachable ONLY while the Aa menu is already open. When
// it isn't in the tree we return a hint telling the user to open the menu, rather than
// silently doing nothing.
//
// Blocking + COM-heavy; call it on a background thread (COM is initialised per
// thread by UIAutomation::new()), never on the Slint UI thread.

use uiautomation::filters::FnFilter;
use uiautomation::patterns::UITogglePattern;
use uiautomation::types::ToggleState;
use uiautomation::{UIAutomation, UIElement};

const TOGGLE_ID: &str = "ToggleButton-Assistive reader toggle";

/// Drive Kindle's Assistive reader (Read Aloud) to `want` (true = reading). Reads
/// the control's current state and only flips it when needed, so the panel switch
/// stays correct even if Read Aloud was toggled inside Kindle directly. Returns the
/// resulting state. Err with a user-facing message on any failure — including when
/// Kindle's Aa menu (which hosts the toggle) isn't open, since we can't open it.
pub fn set_read_aloud(want: bool) -> Result<bool, String> {
    let auto = UIAutomation::new().map_err(|e| format!("UI Automation init failed: {e}"))?;
    let kindle = auto
        .create_matcher()
        .name("Kindle")
        .timeout(2000)
        .find_first()
        .map_err(|_| "Kindle window not found — is Kindle running?".to_string())?;

    // The toggle is only in the tree while the Aa menu is open, and we can't open the
    // menu ourselves on this Kindle build — so a miss means "open it yourself first".
    let toggle = find_by_id(&auto, &kindle, TOGGLE_ID).ok_or_else(|| {
        "Open Kindle's \"Aa\" (Page settings) menu, then flip this to start Read Aloud."
            .to_string()
    })?;

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
