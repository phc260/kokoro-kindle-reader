// An OCR'd page, all the way to kokoro-host and back, over a real socket.
//
// This is the one seam nothing else covers. `retune.test.ts` drives `speakAll` through a fake
// offscreen document, which is right for what it tests and means the request is never built and
// `fetch` is never called; `stream.test.ts` stops at the chunk offsets. So the transport itself -
// the URL, the bearer header, the JSON body, the f32 response, the sample rate - was only ever
// exercised by running Chrome against the tray app, which is exactly the thing that cannot be run
// here.
//
// So: the REAL offscreen handler (its `http-synth` case does the fetch), the REAL narrator, and a
// loopback server that enforces what `kokoro-host/src/webserve.rs` enforces - Host, Origin,
// constant-token, and the same status codes. Faithfulness of the mirror is what makes a pass
// meaningful, so keep it in step with that file; the constants it shares with the extension are
// pinned separately in `host-contract.test.ts`.
//
// The page is fed the way a two-column page really arrives: one part per column, cut at the last
// sentence end rather than at the column boundary (`readColumns` in content/index.ts), because
// the interesting failures - a lost chunk, a duplicated one, a boundary addressed to the wrong
// part - only appear when the text arrives in pieces.

import { test, expect, afterEach, afterAll } from 'bun:test';
// Types only - the value imports below are deliberately dynamic, so nothing reads `chrome` or
// `AudioContext` before this file has installed them.
import type { TextPart, WordBoundary } from '../src/speak';

/** Fake seconds per real second. The whole page is heard in about a third of the time. */
const CLOCK = 3;
const RATE = 24000;
/** Samples the fake host returns per character - about 400 characters to two seconds of speech. */
const SAMPLES_PER_CHAR = 120;

// --------------------------------------------------------------------- fake Web Audio

/**
 * Enough of an AudioContext for offscreen.ts, with a clock that runs fast and sources that really
 * end. `queued` reaching zero is how `speakAll` learns a page has been heard, and it only reaches
 * zero because `onended` fires - so a stub that never fires it hangs the page instead of failing.
 */
class FakeSource {
  buffer: { duration: number } | null = null;
  onended: (() => void) | null = null;
  #timer: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly ctx: FakeAudioContext) {}

  connect(): void {}
  disconnect(): void {}

  start(at: number): void {
    const end = at + (this.buffer?.duration ?? 0);
    this.ctx.live.add(this);
    // Assigned by pushSamples AFTER start(), so read it when it fires, never capture it here.
    this.#timer = setTimeout(
      () => {
        this.ctx.live.delete(this);
        this.onended?.();
      },
      Math.max(0, ((end - this.ctx.currentTime) / CLOCK) * 1000),
    );
  }

  stop(): void {
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
    this.ctx.live.delete(this);
  }
}

class FakeAudioContext {
  static current: FakeAudioContext | null = null;

  readonly sampleRate: number;
  readonly destination = {};
  readonly live = new Set<FakeSource>();
  /** Every sample handed to `copyToChannel`, so the test can check nothing was dropped. */
  samples = 0;
  #base = Date.now();
  #offset = 0;
  #running = true;

  constructor(opts?: { sampleRate?: number }) {
    this.sampleRate = opts?.sampleRate ?? 48000;
    FakeAudioContext.current = this;
  }

  get currentTime(): number {
    return this.#offset + (this.#running ? ((Date.now() - this.#base) / 1000) * CLOCK : 0);
  }

  createGain() {
    return { gain: { value: 1 }, connect: () => {}, disconnect: () => {} };
  }

  createBuffer(_channels: number, length: number, rate: number) {
    return {
      duration: length / rate,
      length,
      copyToChannel: (data: Float32Array) => {
        this.samples += data.length;
      },
    };
  }

  createBufferSource(): FakeSource {
    return new FakeSource(this);
  }

  async suspend(): Promise<void> {
    if (!this.#running) return;
    this.#offset = this.currentTime;
    this.#running = false;
  }

  async resume(): Promise<void> {
    if (this.#running) return;
    this.#base = Date.now();
    this.#running = true;
  }

  async close(): Promise<void> {
    for (const s of [...this.live]) s.stop();
  }
}

// ------------------------------------------------------------------ fake extension runtime

type Listener = (msg: Record<string, unknown>, sender: unknown, reply: (r: unknown) => void) => boolean | void;
const listeners = new Set<Listener>();

const sendMessage = (msg: Record<string, unknown>): Promise<unknown> =>
  new Promise((resolve) => {
    let settled = false;
    const reply = (r: unknown) => {
      if (!settled) {
        settled = true;
        resolve(r);
      }
    };
    let async = false;
    for (const l of [...listeners]) if (l(msg, {}, reply) === true) async = true;
    // Nobody is listening for the word-mark broadcasts unless a page is being read; chrome
    // resolves those with undefined rather than leaving them pending.
    if (!settled && !async) setTimeout(() => reply(undefined), 0);
  });

(globalThis as Record<string, unknown>).AudioContext = FakeAudioContext;
(globalThis as Record<string, unknown>).chrome = {
  runtime: {
    id: 'test',
    onMessage: {
      addListener: (l: Listener) => listeners.add(l),
      removeListener: (l: Listener) => listeners.delete(l),
    },
    sendMessage,
  },
};

// Imported after the globals exist: offscreen.ts registers its listener at module scope.
await import('../src/offscreen');
const { KokoroHttpNarrator } = await import('../src/kokoro-http');
const { speakStream, sentenceEnd } = await import('../src/speak');
const { tellOffscreen } = await import('../src/offscreen-client');

// ---------------------------------------------------------------------- the fake host
//
// Mirrors kokoro-host/src/webserve.rs. Anything the real one rejects, this rejects the same way.

const TOKEN = 'ab12'.repeat(16); // 64 hex chars, as `random_token` emits
const EXTENSION_ORIGIN = 'chrome-extension://acbnkbiijeckelpogcboafgllhccbngm';
const VOICES = ['af_heart', 'af_bella', 'am_michael', 'bf_emma'];

interface Seen {
  method: string;
  path: string;
  auth: string | null;
  contentType: string | null;
  body: { text?: string; voice?: string; speed?: number };
  samples: number;
}

const seen: Seen[] = [];
/** Make the next /synth answer the way a host whose synthesis failed does. */
let synthFails = false;

const server = Bun.serve({
  hostname: '127.0.0.1',
  port: 0,
  async fetch(req) {
    const url = new URL(req.url);
    const origin = req.headers.get('origin');
    const host = req.headers.get('host');

    // (4) rebinding guard.
    if (!host || !/^(127\.0\.0\.1|localhost|\[::1\])(:\d+)?$/.test(host)) {
      return new Response('bad Host\n', { status: 421 });
    }
    // (2) origin allowlist - a request with no Origin at all is a non-browser client.
    if (origin !== null && origin !== EXTENSION_ORIGIN) {
      return new Response('origin not allowed\n', { status: 403 });
    }
    if (req.method === 'OPTIONS') return new Response(null, { status: 204 });
    // (3) bearer token.
    const auth = req.headers.get('authorization');
    if (auth !== `Bearer ${TOKEN}`) return new Response('bad token\n', { status: 401 });

    if (req.method === 'GET' && url.pathname === '/status') {
      seen.push({ method: 'GET', path: '/status', auth, contentType: null, body: {}, samples: 0 });
      return Response.json({ ok: true, voice: VOICES[0], voices: VOICES, sampleRate: RATE });
    }

    if (req.method === 'POST' && url.pathname === '/synth') {
      const body = (await req.json()) as Seen['body'];
      const text = body.text ?? '';
      if (!text) {
        return Response.json({ ok: false, error: 'empty or oversized text' }, { status: 400 });
      }
      if (synthFails) {
        return Response.json({ ok: false, error: 'synthesis failed' }, { status: 500 });
      }
      const pcm = new Float32Array(text.length * SAMPLES_PER_CHAR).fill(0.1);
      seen.push({
        method: 'POST',
        path: '/synth',
        auth,
        contentType: req.headers.get('content-type'),
        body,
        samples: pcm.length,
      });
      return new Response(pcm.buffer, {
        headers: {
          'content-type': 'application/octet-stream',
          'x-sample-rate': String(RATE),
          'x-samples': String(pcm.length),
        },
      });
    }

    return new Response('no such endpoint\n', { status: 404 });
  },
});

const base = `http://127.0.0.1:${server.port}`;
const narrator = (token = TOKEN) => new KokoroHttpNarrator({ base, token });

afterEach(async () => {
  seen.length = 0;
  synthFails = false;
  // Closes the AudioContext and bumps the epoch, so nothing from one test is scheduled into the
  // next one's stream.
  tellOffscreen('audio-stop');
  await Bun.sleep(0);
});

afterAll(() => {
  void server.stop(true);
});

// ------------------------------------------------------------------------------ the page
//
// Two columns of one page, the second continuing a sentence the first started.

const COL1 = 'The lamp was still burning when he came in. He set the case down by the door and';
const COL2 = 'listened. Nothing moved in the house. He counted the seconds, and then he counted them again.';
const PAGE = `${COL1}\n${COL2}`;

/**
 * The page as `readColumns` (content/index.ts) hands it over: one part per column, cut at the last
 * sentence end, the remainder carried into the part that continues it.
 */
async function* columns(): AsyncGenerator<TextPart> {
  let text = '';
  let sent = 0;
  const cols = [COL1, COL2];
  for (let i = 0; i < cols.length; i++) {
    text = text ? `${text}\n${cols[i]}` : cols[i]!;
    const cut = i === cols.length - 1 ? text.length : sentenceEnd(text, sent);
    if (cut > sent) {
      yield { text: text.slice(sent, cut), base: sent };
      sent = cut;
    }
  }
}

/** Non-whitespace characters, in order. `chunk()` only ever drops or normalizes whitespace. */
const ink = (s: string) => s.replace(/\s+/g, '');

test('the handshake reports the host voices, tagged by accent', async () => {
  const voices = await narrator().voices();
  expect(voices.map((v) => v.name)).toEqual(VOICES);
  // Local by construction - the point of running our own backend.
  expect(voices.every((v) => v.remote === false)).toBe(true);
  // The `bf_*` voices must stop claiming to be American.
  expect(voices.find((v) => v.name === 'bf_emma')?.lang).toBe('en-GB');
  expect(seen.map((r) => r.path)).toEqual(['/status']);
});

test('a wrong token is reported as a pairing problem, not as an outage', async () => {
  await expect(narrator('00'.repeat(32)).status()).rejects.toThrow(/re-pair/i);
});

test('a page the browser could not have sent is refused - what the Origin allowlist is for', async () => {
  // Not reachable through the extension: Chrome sets this header, the extension cannot. So this
  // asserts the RULE (a random site with a stolen token is still refused) rather than the
  // extension's behaviour, and `host-contract.test.ts` is what keeps the allowed id in step.
  const res = await fetch(`${base}/status`, {
    headers: { authorization: `Bearer ${TOKEN}`, origin: 'https://read.amazon.com' },
  });
  expect(res.status).toBe(403);
});

test('an OCR page reaches the host as authenticated chunks and comes back as scheduled audio', async () => {
  const marks: WordBoundary[] = [];
  await speakStream(narrator(), columns(), { voiceName: 'af_bella', rate: 1.2 }, (b) => marks.push(b));

  const synths = seen.filter((r) => r.path === '/synth');
  expect(synths.length).toBeGreaterThan(1); // the ramp cuts the opening part up

  // Every request carried the token and the shape webserve.rs parses.
  for (const r of synths) {
    expect(r.auth).toBe(`Bearer ${TOKEN}`);
    expect(r.contentType).toContain('application/json');
    expect(r.body.voice).toBe('af_bella');
    expect(r.body.speed).toBe(1.2);
    expect(r.body.text?.length).toBeGreaterThan(0);
  }

  // The whole page was said, once. Compared as ink: `chunk()` re-joins on single spaces, so the
  // whitespace is not expected to survive and the characters are.
  expect(ink(synths.map((r) => r.body.text).join(' '))).toBe(ink(PAGE));

  // Every sample the host returned was scheduled - none dropped between the response and the
  // cursor, and none scheduled twice.
  const returned = synths.reduce((n, r) => n + r.samples, 0);
  expect(FakeAudioContext.current?.samples).toBe(returned);
  expect(FakeAudioContext.current?.sampleRate).toBe(RATE);

  // The marks fired off the audio clock, and they address the PAGE - not the chunk that produced
  // them and not the part it arrived in. A mark landing mid-word is the failure this catches: it
  // is what a base added twice, or not at all, looks like.
  expect(marks.length).toBeGreaterThan(0);
  for (const m of marks) {
    expect(PAGE.slice(m.charIndex, m.charIndex + (m.charLength ?? 1))).toMatch(/^\S/);
    expect(m.charIndex === 0 || /\s/.test(PAGE[m.charIndex - 1]!)).toBe(true);
  }
  // A highlight only ever moves forwards.
  const order = marks.map((m) => m.charIndex);
  expect([...order].sort((a, b) => a - b)).toEqual(order);
});

test('a host that fails a chunk fails the page, with the reason in the message', async () => {
  // Never a silent short page: `readBook` would turn to the next one, and a book read with the
  // odd page missing is worse than one that stops and says why.
  synthFails = true;
  await expect(speakStream(narrator(), columns(), { rate: 1 })).rejects.toThrow(/500/);
});
