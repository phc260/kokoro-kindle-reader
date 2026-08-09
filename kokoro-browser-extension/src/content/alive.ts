// Is this content script still attached to an extension?
//
// A content script does not go away when its extension is reloaded, updated or disabled. It keeps
// running in the page it was injected into, with its DOM, its panel and its captured state intact
// - and with `chrome.runtime` torn out from under it. Every route back to the extension then
// throws the same shape:
//
//   TypeError: Cannot read properties of undefined (reading 'sendMessage')
//
// which names a property, not a cause, and reads as a bug in the line that happened to touch it.
// It is not a bug and there is nothing to retry: this script can never reach the extension again,
// because the extension it belonged to no longer exists. Only a page reload injects a new one.
//
// Checked at each boundary rather than once at startup, because the invalidation happens
// mid-session by definition - a check at load time runs when the context is still valid, every
// time.

/**
 * Throw the actionable sentence if this script has been orphaned.
 *
 * `chrome.runtime` itself is what Chrome removes, so the optional chain is the test; `id` is read
 * rather than called because every method on there is gone by the same stroke.
 */
export function assertAttached(): void {
  if (typeof chrome === 'undefined' || !chrome.runtime?.id)
    throw new Error('the extension was reloaded - refresh this page to reconnect it');
}
