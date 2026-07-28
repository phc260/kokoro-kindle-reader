// Narrator backed by kokoro-host's loopback HTTP endpoint (kokoro-host/src/webserve.rs).
//
// The ONLY route to the backend. A native-messaging bridge was tried ahead of this and removed:
// it needs per-browser registry registration, two manifest dialects and a browser restart, it
// cannot serve Firefox at all, and every symptom arrives as the same one string. This needs no
// registration, works in any browser, and can be reproduced with curl.
//
// What replaces the browser's gating lives in the host: 127.0.0.1 bind + origin allowlist +
// bearer token + Host check (see kokoro-host/src/webserve.rs). The pairing code is the cost of
// that - one paste, from the tray menu.
//
// The audio never passes through the service worker. The offscreen document does the fetch
// itself, so PCM arrives as an ArrayBuffer and goes straight into an AudioBuffer - extension
// messaging is JSON-only, so routing it through the worker would mean base64 and a 33% tax on
// every frame.
//
//   worker:    /status, orchestration          (small JSON)
//   offscreen: POST /synth -> ArrayBuffer -> AudioContext

import type { Narrator, SpeakOptions, VoiceInfo, WordBoundary } from './speak';
import { langOf } from './voices';
import { sendToOffscreen, sleep, startStream, tellOffscreen, waitForRoom } from './offscreen-client';

export interface Pairing {
  base: string;
  token: string;
}

const STORAGE_KEY = 'kwr.endpoint';

/** Matches `webserve::DEFAULT_PORT` in kokoro-host. Only used to probe for an unpaired daemon. */
export const DEFAULT_PORT = 8787;

export type Probe =
  /** Nothing accepted the connection. */
  | { state: 'absent'; base: string }
  /** Answered 401: alive, and waiting for a token it has not been given. */
  | { state: 'running'; base: string }
  /** Answered 403: alive, but this extension id is not in its allowlist. */
  | { state: 'origin-rejected'; base: string };

/**
 * Is a daemon listening, without a token to prove it?
 *
 * `host_permissions` covers `http://127.0.0.1/*`, so a worker fetch there is not subject to CORS
 * and the status code is readable. That is the whole trick: *any* HTTP reply means something is
 * listening, and the code says which kind of not-working this is. A refused connection throws,
 * and on loopback it throws immediately.
 */
export async function probeDaemon(port: number = DEFAULT_PORT): Promise<Probe> {
  const base = `http://127.0.0.1:${port}`;
  try {
    const res = await fetch(`${base}/status`, { signal: AbortSignal.timeout(1500) });
    if (res.status === 403) return { state: 'origin-rejected', base };
    return { state: 'running', base };
  } catch {
    return { state: 'absent', base };
  }
}

/** One sentence naming the actual next action, for the panel's status line. */
export function describeProbe(p: Probe): string {
  switch (p.state) {
    case 'absent':
      return `no Kokoro daemon on ${p.base} - start Kokoro Kindle Reader (the tray app)`;
    case 'origin-rejected':
      return `Kokoro daemon is running on ${p.base} but does not allow this extension id (${chrome.runtime.id})`;
    case 'running':
      return `Kokoro daemon is running on ${p.base} but this extension is not paired - open its options page and paste the pairing code`;
  }
}

/** `kwr_<port>_<token>` - one opaque string is easier to paste correctly than two fields. */
export function parsePairing(s: string): Pairing | null {
  const m = /^kwr_(\d{1,5})_([0-9a-f]{32,128})$/.exec(s.trim());
  if (!m) return null;
  return { base: `http://127.0.0.1:${m[1]}`, token: m[2]! };
}

export async function savePairing(p: Pairing | null): Promise<void> {
  if (p) await chrome.storage.local.set({ [STORAGE_KEY]: p });
  else await chrome.storage.local.remove(STORAGE_KEY);
}

export async function loadPairing(): Promise<Pairing | null> {
  const got = await chrome.storage.local.get(STORAGE_KEY);
  return (got[STORAGE_KEY] as Pairing | undefined) ?? null;
}


export class KokoroHttpNarrator implements Narrator {
  readonly kind = 'kokoro (http)';
  #pairing: Pairing;

  constructor(pairing: Pairing) {
    this.#pairing = pairing;
  }

  get pairing(): Pairing {
    return this.#pairing;
  }

  /** Handshake. Also the warm-up: the daemon has the model loaded before the first page. */
  async status(): Promise<{ voice: string; voices: string[]; sampleRate: number }> {
    const res = await fetch(`${this.#pairing.base}/status`, {
      headers: { authorization: `Bearer ${this.#pairing.token}` },
    });
    if (res.status === 401) throw new Error('token rejected - re-pair from the options page');
    if (res.status === 403) throw new Error('origin rejected - the daemon does not allow this extension id');
    if (!res.ok) throw new Error(`status ${res.status}`);
    return await res.json();
  }

  async voices(): Promise<VoiceInfo[]> {
    const s = await this.status();
    // Local by construction - that is the entire point of running our own backend. The tag comes
    // from the id's first letter, so the `bf_*`/`bm_*` voices stop claiming to be American.
    return s.voices.map((name) => ({ name, lang: langOf(name) ?? 'en-US', remote: false }));
  }

  async speak(text: string, opts: SpeakOptions = {}, _onWord?: (b: WordBoundary) => void): Promise<void> {
    await this.speakAll([text], opts);
  }

  /**
   * Synthesize ahead of playback, scheduling every chunk onto one AudioContext cursor.
   *
   * The daemon renders ~3.4x faster than the ear consumes, so after the first chunk there is
   * always more audio queued than time to play it - which is exactly what hides the next
   * chunk's synthesis. Two things keep that honest: the epoch, so Stop discards work already in
   * flight, and the lead cap, so a page does not render five minutes ahead and throw it away.
   */
  async speakAll(chunks: string[], opts: SpeakOptions = {}, _onWord?: (b: WordBoundary, i: number) => void): Promise<void> {
    const epoch = await startStream();

    for (const text of chunks) {
      if (await waitForRoom(epoch)) return; // stopped
      const r = await sendToOffscreen({
        t: 'http-synth',
        epoch,
        base: this.#pairing.base,
        token: this.#pairing.token,
        text,
        voice: opts.voiceName,
        speed: opts.rate ?? 1,
      });
      if (r.stale) return;
    }

    // Everything is scheduled; speak() must not resolve until it has actually been heard.
    for (;;) {
      const s = await sendToOffscreen({ t: 'audio-status', epoch });
      if (s.stale) return;
      if ((s.queued ?? 0) === 0 && (s.lead ?? 0) <= 0.05) return;
      await sleep(Math.min(1000, (s.lead ?? 0) * 1000 + 100));
    }
  }

  stop(): void {
    tellOffscreen('audio-stop');
  }
  pause(): void {
    tellOffscreen('audio-pause');
  }
  resume(): void {
    tellOffscreen('audio-resume');
  }
}
