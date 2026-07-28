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

test('wasm-unsafe-eval is granted, or Tesseract cannot compile in the offscreen document', () => {
  expect(chrome.content_security_policy?.extension_pages ?? '').toContain('wasm-unsafe-eval');
});

// Still load-bearing without native messaging: an unpacked extension's id is derived from its
// path, and kokoro-host's HTTP endpoint allowlists the id as an origin. Unpinned, the id changes
// whenever the folder moves and the daemon answers 403.
test('the extension id is pinned, so the origin allowlist cannot go stale', () => {
  expect(typeof chrome.key).toBe('string');
  expect(chrome.key.length).toBeGreaterThan(300);
});
