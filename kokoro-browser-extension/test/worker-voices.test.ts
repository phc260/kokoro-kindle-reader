// What the service worker does when the tray app goes away mid-book.
//
// `speakPage` asks for the voice list before it captures a page, so for Kokoro this message is a
// real `/status` request to kokoro-host - and it is therefore the first thing that notices the
// host is gone. Two things have to hold, and neither did:
//
//   1. It must ANSWER. The generic error reply carries the request's id and a voices request has
//      none, so the page sat on its 60 s timeout instead - once per page, for a host that refuses
//      the connection instantly.
//   2. It must stop reporting the cached choice. `chosen` is a decision, not a health check, and
//      it outlives the host it was made about.
//
// Driven through the real background module, because the recovery lives in its message handler
// and there is no other way in.

import { test, expect } from 'bun:test';

const HOST_VOICES = ['af_heart', 'bf_emma'];
const TTS_VOICES = [{ voiceName: 'Microsoft David', lang: 'en-US', remote: false }];

/** Set to make every request to the host fail the way a closed tray app does. */
let hostDown = false;

(globalThis as Record<string, unknown>).fetch = async (input: string | URL) => {
  if (hostDown) throw new TypeError('Failed to fetch');
  if (String(input).endsWith('/status')) {
    return Response.json({ ok: true, voice: HOST_VOICES[0], voices: HOST_VOICES, sampleRate: 24000 });
  }
  return new Response('no such endpoint\n', { status: 404 });
};

type Connect = (port: unknown) => void;
let onConnect: Connect | null = null;

/** The worker's `chrome.runtime.onMessage` handler - the OCR relay lives on it. */
type OnMessage = (
  msg: Record<string, unknown>,
  sender: unknown,
  sendResponse: (r: unknown) => void,
) => unknown;
let onMessage: OnMessage | null = null;

/** What the offscreen document answers the relay with. Set per test. */
let offscreenReply: unknown = { ok: true };

(globalThis as Record<string, unknown>).chrome = {
  runtime: {
    id: 'test',
    onMessage: { addListener: (f: OnMessage) => (onMessage = f), removeListener: () => {} },
    onConnect: { addListener: (f: Connect) => (onConnect = f) },
    sendMessage: async () => offscreenReply,
  },
  storage: {
    local: {
      get: async () => ({ 'kwr.endpoint': { base: 'http://127.0.0.1:8787', token: 'ab12'.repeat(16) } }),
      set: async () => {},
      remove: async () => {},
    },
  },
  offscreen: {
    Reason: { WORKERS: 'WORKERS', AUDIO_PLAYBACK: 'AUDIO_PLAYBACK' },
    hasDocument: async () => true,
    createDocument: async () => {},
  },
  tts: { getVoices: async () => TTS_VOICES, speak: () => {}, stop: () => {}, pause: () => {}, resume: () => {} },
};

await import('../src/background');

/** The worker's end of the content script's `narrate` port. */
class WorkerPort {
  readonly name = 'narrate';
  readonly sent: Record<string, unknown>[] = [];
  #handlers: ((m: Record<string, unknown>) => unknown)[] = [];

  onMessage = { addListener: (f: (m: Record<string, unknown>) => unknown) => this.#handlers.push(f) };
  onDisconnect = { addListener: () => {} };

  postMessage(m: Record<string, unknown>): void {
    this.sent.push(m);
  }

  /** Deliver a message and wait for the handler to finish, so the reply is on `sent`. */
  async send(m: Record<string, unknown>): Promise<Record<string, unknown>> {
    const before = this.sent.length;
    await Promise.all(this.#handlers.map((f) => f(m)));
    return this.sent[before]!;
  }
}

const port = new WorkerPort();
onConnect!(port);

test('with the host up, the voice list is the host, and it is named as such', async () => {
  const reply = await port.send({ t: 'voices' });
  expect(reply.t).toBe('voices');
  expect(reply.engine).toBe('kokoro (http)');
  expect((reply.voices as { name: string }[]).map((v) => v.name)).toEqual(HOST_VOICES);
  expect(reply.engineError).toBeNull();
});

test('the host going away is answered, not waited out, and is not still reported as Kokoro', async () => {
  hostDown = true;
  const reply = await port.send({ t: 'voices' });

  // Answered at all - this is the assertion that was worth 60 seconds a page.
  expect(reply).toBeDefined();
  expect(reply.t).toBe('voices');
  // The cached narrator is not offered as though it still worked...
  expect(reply.engine).not.toBe('kokoro (http)');
  expect(reply.engine).toBe('chrome.tts');
  // ...the page can still be read...
  expect((reply.voices as { name: string }[])[0]?.name).toBe('Microsoft David');
  // ...and the panel is told why it is not Kokoro, in the terms the user can act on.
  //
  // Specifically NOT "Failed to fetch". A rejected fetch names neither the host, the port, nor
  // which kind of not-working this is, and it is the message a paired extension used to show for
  // every one of them; `describeProbe` exists to answer that and now actually runs on this path.
  const why = String(reply.engineError);
  expect(why).toMatch(/127\.0\.0\.1:8787/);
  expect(why).toMatch(/start Kokoro Kindle Reader|not paired|does not allow this extension id/);
});

test('an unreachable PAIRED host is not diagnosed as an unpaired one', async () => {
  const { describeUnreachable } = await import('../src/kokoro-http');
  const paired = 'http://127.0.0.1:8787';

  // Nothing listening anywhere: the tray app is the answer, and the two bases agree so only one
  // is worth naming.
  expect(describeUnreachable(paired, { state: 'absent', base: paired })).toMatch(/start Kokoro/);

  // A daemon answering 401 means "up, and it wants a token". `describeProbe` reads that as NOT
  // PAIRED, which is right for a caller that has no pairing and wrong for this one - it would
  // send someone to re-paste a code that was never the problem. On the same base the pairing is
  // not in question at all: the port is live and this request is what failed.
  const running = describeUnreachable(paired, { state: 'running', base: paired });
  expect(running).not.toMatch(/not paired/);
  expect(running).toMatch(/listening but the request did not complete/);

  // A different base IS the stale-pairing case, and it is the one that says so.
  expect(describeUnreachable('http://127.0.0.1:9999', { state: 'running', base: paired })).toMatch(
    /paired with http:\/\/127\.0\.0\.1:9999.*daemon is on .*8787/,
  );

  expect(describeUnreachable(paired, { state: 'origin-rejected', base: paired })).toMatch(
    /does not allow this extension id/,
  );
});

/**
 * Drive the REAL OCR relay - `chrome.runtime.onMessage` in background.ts - and return what it
 * answers the content script with.
 *
 * Through the relay rather than by calling `diagnosePaired` directly, because the defect this
 * guards against is a wiring one: the diagnosis was correct and simply never ran on the paired
 * path. A test that calls the helper cannot see the relay being removed.
 */
async function relayOcr(reply?: unknown): Promise<{ ok: boolean; error?: string }> {
  // Fresh each call, the way `chrome.runtime.sendMessage` really answers - a relay that wrote into
  // the object it was handed would otherwise accumulate its own diagnosis across pages.
  if (reply !== undefined) offscreenReply = structuredClone(reply);
  return await new Promise((resolve) => {
    onMessage!({ t: 'ocr', b64: 'AAAA', type: 'image/png' }, null, (r) =>
      resolve(r as { ok: boolean; error?: string }),
    );
  });
}

test('a stale token on a large POST still gets told to re-pair', async () => {
  // The case `describeProbe` and `describeUnreachable` both get wrong. An OCR post carries a page
  // image, and the host writes its 401 WITHOUT draining that body - deliberately, so no stranger
  // can make it copy megabytes - which means the reply can be lost with the connection. The
  // failure then looks identical to a host that is gone, and the user is sent to restart a tray
  // app that is running fine. A bodiless /status cannot lose its reply, so it separates them.
  hostDown = false;
  const failed = {
    ok: false,
    error: 'could not reach http://127.0.0.1:8787/ocr (posted 9.16 MiB): TypeError: Failed to fetch',
  };

  // The stub asserts what is asked of it, not just what it answers: probing `/ocr` again, or
  // dropping the bearer header, would give a diagnosis that cannot mean what it says.
  const saved = (globalThis as Record<string, unknown>).fetch;
  let asked = 0;
  (globalThis as Record<string, unknown>).fetch = async (input: string | URL, init?: RequestInit) => {
    asked++;
    expect(String(input)).toBe('http://127.0.0.1:8787/status');
    expect(init?.method ?? 'GET').toBe('GET');
    expect(String((init?.headers as Record<string, string>).authorization)).toMatch(/^Bearer /);
    return new Response('bad token\n', { status: 401 });
  };

  const reply = await relayOcr(failed);
  expect(reply.ok).toBe(false);
  expect(asked).toBe(1); // exactly one diagnostic request, on a path that already failed
  expect(reply.error).toMatch(/9\.16 MiB/); // the offscreen document's facts survive...
  expect(reply.error).toMatch(/re-pair from the options page/); // ...and the worker's are added

  // When the token is good the same failure must NOT say re-pair - and must not name a cause it
  // cannot know either, since a body that stalled past the host's timeout lands here too.
  (globalThis as Record<string, unknown>).fetch = async () =>
    Response.json({ ok: true, voice: 'af_heart', voices: ['af_heart'], sampleRate: 24000 });
  const good = (await relayOcr(failed)).error ?? '';
  expect(good).not.toMatch(/re-pair/);
  expect(good).toMatch(/pairing is not the problem/);

  // A failure the host DID answer keeps its own message and costs no diagnostic request.
  offscreenReply = { ok: false, error: 'the Kokoro host cannot do OCR: det.onnx is not there' };
  asked = 0;
  const answered = (await relayOcr()).error ?? '';
  expect(answered).toBe('the Kokoro host cannot do OCR: det.onnx is not there');
  expect(asked).toBe(0);

  (globalThis as Record<string, unknown>).fetch = saved;
  offscreenReply = { ok: true };
});
