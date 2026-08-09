// The manifest declares capabilities the code assumes. When those two drift, the failure is
// silent: Chrome omits the API, the feature-detect returns false, and a fallback engages with
// no error. That cost an hour once - hence these.

import { test, expect } from 'bun:test';
import path from 'node:path';

const root = path.join(import.meta.dir, '..');
const chrome = await Bun.file(path.join(root, 'manifest.chrome.json')).json();
const firefox = await Bun.file(path.join(root, 'manifest.firefox.json')).json();

test('chrome manifest declares every permission the code uses', () => {
  // chrome.tts (speak.ts), chrome.offscreen (background.ts), chrome.storage (kokoro-http.ts)
  expect(chrome.permissions).toContain('tts');
  expect(chrome.permissions).toContain('offscreen');
  expect(chrome.permissions).toContain('storage');
});

test('firefox manifest claims no chrome-only APIs', () => {
  expect(firefox.permissions).toContain('storage');
  // No chrome.tts or chrome.offscreen in Firefox; claiming them warns on load.
  expect(firefox.permissions).not.toContain('tts');
  expect(firefox.permissions).not.toContain('offscreen');
});

// The extension reaches kokoro-host over loopback HTTP and nothing else. `nativeMessaging` was
// declared while a native-messaging bridge was tried first; that route is gone, and an unused
// permission is both a scarier install prompt and a claim the code no longer backs.
test('neither manifest still asks for nativeMessaging', () => {
  expect(chrome.permissions).not.toContain('nativeMessaging');
  expect(firefox.permissions).not.toContain('nativeMessaging');
});

// host_permissions covers the loopback origin, which is what makes a fetch to 127.0.0.1 exempt
// from CORS - without it probeDaemon cannot read the status code and every failure looks alike.
test('both manifests grant the loopback origin the narrator fetches', () => {
  expect(chrome.host_permissions).toContain('http://127.0.0.1/*');
  expect(firefox.host_permissions).toContain('http://127.0.0.1/*');
});

// The reverse of what this used to assert. `wasm-unsafe-eval` was here so the in-page OCR engine
// could compile its wasm; recognition is `POST /ocr` on the host now, so the package contains no
// wasm at all - and a CSP relaxation kept for an engine that left is a standing invitation with
// nothing behind it. Same for the vendored worker/core/language assets the package used to carry.
test('the package grants nothing the departed OCR engine needed', () => {
  const csp = chrome.content_security_policy?.extension_pages ?? '';
  expect(csp).not.toContain('wasm-unsafe-eval');
  expect(csp).toContain("script-src 'self'");
  for (const m of [chrome, firefox]) {
    const entries = (m.web_accessible_resources ?? []) as { resources?: string[] }[];
    const exposed = entries.flatMap((r) => r.resources ?? []);
    expect(exposed).not.toContain('vendor/*');
  }
});

// The offscreen document outlived the worker it was created for - the host's origin allowlist is
// what keeps it - but the REASON it declares has to match what it actually does.
test('the offscreen document no longer claims a WORKERS reason', async () => {
  const background = await Bun.file(new URL('../src/background.ts', import.meta.url)).text();
  expect(background).not.toContain('Reason.WORKERS');
  expect(background).toContain('Reason.AUDIO_PLAYBACK');
});

// Still load-bearing without native messaging: an unpacked extension's id is derived from its
// path, and kokoro-host's HTTP endpoint allowlists the id as an origin. Unpinned, the id changes
// whenever the folder moves and the daemon answers 403.
test('the extension id is pinned, so the origin allowlist cannot go stale', () => {
  expect(typeof chrome.key).toBe('string');
  expect(chrome.key.length).toBeGreaterThan(300);
});
