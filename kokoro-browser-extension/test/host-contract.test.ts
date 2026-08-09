// The four constants the extension and kokoro-host have to agree on, checked against the host's
// own source rather than restated here.
//
// None of these can be caught on either side alone, and every one of them fails as something
// other than itself:
//
//   - the extension id. The host allowlists ONE `chrome-extension://` origin. The extension does
//     not set that header - Chrome does - so no test that drives the extension can notice the two
//     have drifted. It arrives as a 403 on every request, which reads exactly like a pairing
//     problem.
//   - the port. Only used to probe for an UNPAIRED daemon, which is how "not running" is told
//     apart from "not paired" - so drift here turns a running host into "start the tray app".
//   - the sample rate. The offscreen document builds its AudioContext at a hardcoded rate and
//     copies the host's PCM straight in, so a mismatch is not an error anywhere: the book is
//     simply read at the wrong pitch and speed.
//   - the pairing string. Written by the host for a human to paste and parsed here by regex.
//
// Parsed out of the Rust with regexes on purpose: the alternative is a second copy of each value
// in a fixture, which is the drift this is meant to catch.

import { test, expect } from 'bun:test';
import path from 'node:path';
import { DEFAULT_PORT, parsePairing } from '../src/kokoro-http';

const repo = path.join(import.meta.dir, '..', '..');
const read = (p: string) => Bun.file(path.join(repo, p)).text();

const webserve = await read('kokoro-host/src/webserve.rs');
const protocol = await read('kokoro-protocol/src/lib.rs');
const offscreenSrc = await Bun.file(path.join(import.meta.dir, '..', 'src', 'offscreen.ts')).text();
const manifest = await Bun.file(path.join(import.meta.dir, '..', 'manifest.chrome.json')).json();

/** The one capture of `re` in `src`, or a failure that names what went missing. */
function only(src: string, re: RegExp, what: string): string {
  const m = re.exec(src);
  if (!m?.[1]) throw new Error(`could not find ${what} - has it been renamed or moved?`);
  return m[1];
}

/**
 * A Chrome extension's id: the first 16 bytes of the SHA-256 of its public key (DER, which is
 * what the manifest's `key` is, base64'd), each nibble mapped onto a-p.
 *
 * This is the same derivation Chrome does at load time, which is why pinning `key` pins the id
 * whatever directory the unpacked extension is loaded from.
 */
function extensionId(keyB64: string): string {
  const hasher = new Bun.CryptoHasher('sha256');
  hasher.update(Buffer.from(keyB64, 'base64'));
  const digest = hasher.digest();
  let id = '';
  for (let i = 0; i < 16; i++) {
    id += String.fromCharCode(97 + (digest[i]! >> 4)) + String.fromCharCode(97 + (digest[i]! & 15));
  }
  return id;
}

test('the id the manifest key pins is the origin kokoro-host allows', () => {
  const allowed = only(webserve, /DEFAULT_EXTENSION_ID: &str = "([a-p]{32})"/, 'DEFAULT_EXTENSION_ID in webserve.rs');
  expect(extensionId(manifest.key)).toBe(allowed);
});

test('the port the extension probes is the port kokoro-host binds', () => {
  const port = only(webserve, /pub const DEFAULT_PORT: u16 = (\d+);/, 'DEFAULT_PORT in webserve.rs');
  expect(DEFAULT_PORT).toBe(Number(port));
});

test('the offscreen AudioContext runs at the rate the host synthesizes', () => {
  // `24_000` in Rust, `24000` here; compare the numbers, not the spellings.
  const host = Number(only(protocol, /pub const SAMPLE_RATE: u32 = ([0-9_]+);/, 'SAMPLE_RATE').replace(/_/g, ''));
  const ctx = Number(only(offscreenSrc, /new AudioContext\(\{\s*sampleRate:\s*(\d+)/, "the offscreen AudioContext's rate"));
  expect(ctx).toBe(host);
});

test('the pairing string the host writes is one the extension parses', () => {
  // `Endpoint::pairing_string`, with `random_token`'s 32 bytes of hex.
  const format = only(webserve, /format!\("kwr_\{\}_\{\}", (self\.port)/, 'pairing_string in webserve.rs');
  expect(format).toBe('self.port');

  const token = 'ab12'.repeat(16); // 64 hex chars, as `random_token` emits
  expect(token).toHaveLength(64);
  const parsed = parsePairing(`kwr_${DEFAULT_PORT}_${token}`);
  expect(parsed).toEqual({ base: `http://127.0.0.1:${DEFAULT_PORT}`, token });
});

test('the fields /synth reads are the fields the extension sends', () => {
  // The request the offscreen document builds...
  const body = only(offscreenSrc, /body: JSON\.stringify\(\{([^}]*)\}\)/, 'the /synth request body');
  const sent = [...body.matchAll(/(\w+):/g)].map((m) => m[1]);

  // ...and the keys the /synth arm pulls back out of it. Scoped to that arm so `/status`'s own
  // keys cannot stand in for one that was dropped.
  const arm = only(webserve, /\("POST", "\/synth"\) => \{([\s\S]*?)\n {8}\}/, 'the /synth arm of webserve.rs');
  const read = [...arm.matchAll(/\.get\("(\w+)"\)/g)].map((m) => m[1]);

  for (const key of ['text', 'voice', 'speed']) {
    expect(sent).toContain(key);
    expect(read).toContain(key);
  }
});
