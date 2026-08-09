// The port between the content script and the service worker, under the conditions that actually
// break it.
//
// MV3 workers get evicted, crash, and are torn down by extension reloads, so "the port is dead"
// is a normal event rather than an edge case - and a dead port does not announce itself in time:
// `postMessage` throws first and `onDisconnect` arrives afterwards. Every test here is a sequence
// that really happened or nearly did, driven through a fake chrome.runtime because none of it is
// reachable from a unit test otherwise.

import { test, expect, beforeEach } from 'bun:test';

/** Minimal stand-in for a chrome.runtime.Port, with the disconnect timing made explicit. */
class FakePort {
  static live: FakePort[] = [];
  readonly sent: unknown[] = [];
  /** Set to make postMessage throw the way a port whose worker has gone does. */
  dead = false;
  #onMessage: ((m: unknown) => void)[] = [];
  #onDisconnect: (() => void)[] = [];

  onMessage = { addListener: (f: (m: unknown) => void) => this.#onMessage.push(f), removeListener: (f: (m: unknown) => void) => { const i = this.#onMessage.indexOf(f); if (i >= 0) this.#onMessage.splice(i, 1); } };
  onDisconnect = { addListener: (f: () => void) => this.#onDisconnect.push(f) };

  postMessage(m: unknown): void {
    if (this.dead) throw new Error('Attempting to use a disconnected port object');
    this.sent.push(m);
  }

  /** Deliver a reply from the "worker". */
  reply(m: unknown): void {
    for (const f of [...this.#onMessage]) f(m);
  }

  /** Fire the disconnect event - separately from `dead`, because the delay is the whole bug. */
  disconnect(): void {
    for (const f of [...this.#onDisconnect]) f();
  }
}

beforeEach(() => {
  FakePort.live = [];
  (globalThis as Record<string, unknown>).chrome = {
    runtime: {
      // An attached content script has one; Chrome removes the whole of `chrome.runtime` when the
      // extension is reloaded out from under the page, which is what `assertAttached` tests for.
      id: 'test',
      lastError: undefined,
      connect: () => {
        const p = new FakePort();
        FakePort.live.push(p);
        return p;
      },
    },
  };
});

const { PortNarrator, narrateStream, engineKind, engineError, useEngine } = await import('../src/content/narrate');

test('a reply resolves speak, and the listener is cleaned up after', async () => {
  const n = new PortNarrator();
  const done = n.speak('hello');
  const p = FakePort.live[0]!;
  expect(p.sent).toHaveLength(1);

  const id = (p.sent[0] as { id: string }).id;
  p.reply({ t: 'end', id });
  await done;

  // A second utterance must not be resolved by the first one's leftover listener.
  const second = n.speak('again');
  p.reply({ t: 'end', id }); // stale id
  let settled = false;
  void second.then(() => (settled = true));
  await Promise.resolve();
  expect(settled).toBe(false);
  p.reply({ t: 'end', id: (p.sent[1] as { id: string }).id });
  await second;
});

test('a dead cached port is retried on a fresh one rather than failing', async () => {
  const n = new PortNarrator();
  // First call establishes the port; the worker then goes away.
  const first = n.speak('one');
  const p1 = FakePort.live[0]!;
  p1.reply({ t: 'end', id: (p1.sent[0] as { id: string }).id });
  await first;

  p1.dead = true;
  const second = n.speak('two');
  expect(FakePort.live).toHaveLength(2); // reconnected
  const p2 = FakePort.live[1]!;
  p2.reply({ t: 'end', id: (p2.sent[0] as { id: string }).id });
  await second;
});

test("a stale port's late disconnect does not kill the replacement", async () => {
  // The exact race: postMessage throws BEFORE onDisconnect is delivered, so the retry has already
  // built a healthy port by the time the old event lands. Sharing one rejector set across
  // connections meant that event rejected the retry and narration fell back to speechSynthesis
  // for the rest of the session.
  const n = new PortNarrator();
  const first = n.speak('one');
  const p1 = FakePort.live[0]!;
  p1.reply({ t: 'end', id: (p1.sent[0] as { id: string }).id });
  await first;

  p1.dead = true;
  const second = n.speak('two');
  const p2 = FakePort.live[1]!;

  p1.disconnect(); // late, and belongs to nobody

  let rejected: unknown = null;
  void second.catch((e) => (rejected = e));
  await Promise.resolve();
  expect(rejected).toBeNull();

  p2.reply({ t: 'end', id: (p2.sent[0] as { id: string }).id });
  await second; // resolves - the replacement survived
});

test('a disconnect while genuinely in flight rejects, rather than hanging the page loop', async () => {
  // readBook awaits this promise. Silence here is a permanent wedge, not a lost message.
  const n = new PortNarrator();
  const speaking = n.speak('a page');
  FakePort.live[0]!.disconnect();
  await expect(speaking).rejects.toThrow(/disconnect/i);
});

test('stop/pause/resume never throw on a dead port', () => {
  const n = new PortNarrator();
  void n.speak('x'); // establishes the port
  const p = FakePort.live[0]!;
  p.dead = true;

  // These used to throw straight out of a click handler as an uncaught error.
  expect(() => n.stop()).not.toThrow();
  expect(() => n.pause()).not.toThrow();
  expect(() => n.resume()).not.toThrow();
});

test('stop before anything has been spoken does not wake a worker', () => {
  const n = new PortNarrator();
  n.stop();
  expect(FakePort.live).toHaveLength(0);
});

// --- when the host is the thing that failed ---------------------------------------------------
//
// `speakPage` asks for the voice list before it captures, and for Kokoro that is a real request to
// kokoro-host. So these two are what a tray app that was closed mid-book actually looks like from
// the page.

test('a voices failure is reported at once rather than waiting out the timeout', async () => {
  // A voices request carries no id, so the worker's error reply carries none either. Matching
  // nothing, it used to leave the request on its 60 s timer - once per page - for a host that had
  // already said what was wrong.
  const n = new PortNarrator();
  const asking = n.voices();
  FakePort.live[0]!.reply({ t: 'error', message: 'kokoro unavailable: connection refused' });
  await expect(asking).rejects.toThrow(/connection refused/);
});

test("an utterance's error does not settle a voices request", async () => {
  // The two are told apart by the id, and only one of them has one.
  const n = new PortNarrator();
  const asking = n.voices();
  const p = FakePort.live[0]!;

  p.reply({ t: 'error', id: 'u7', message: 'a page failed' });
  let settled = false;
  void asking.then(
    () => (settled = true),
    () => (settled = true),
  );
  await Promise.resolve();
  expect(settled).toBe(false);

  p.reply({ t: 'voices', voices: [{ name: 'af_heart' }], engine: 'kokoro (http)' });
  expect(await asking).toHaveLength(1);
});

test('a page that fell back to speechSynthesis does not keep the whole book there', async () => {
  // The fallback finishes the page the host abandoned - stopping mid-paragraph would be worse -
  // but it used to replace the narrator for the life of the tab. One failed chunk and the rest of
  // the book was read by a system voice, with the reason only in the service worker's console and
  // Kokoro never tried again however long the host had been back.
  const spoken: string[] = [];
  (globalThis as Record<string, unknown>).speechSynthesis = {
    getVoices: () => [],
    cancel: () => {},
    pause: () => {},
    resume: () => {},
    speak: (u: { text: string; onend?: () => void }) => {
      spoken.push(u.text);
      setTimeout(() => u.onend?.(), 0);
    },
  };
  (globalThis as Record<string, unknown>).SpeechSynthesisUtterance = class {
    onend: (() => void) | null = null;
    onerror: unknown = null;
    onboundary: unknown = null;
    constructor(public text: string) {}
  };

  const kind = useEngine('chrome-tts'); // a PortNarrator, as a Chrome content script gets
  const page = narrateStream(
    (async function* () {
      yield { text: 'The page the host gave up on.', base: 0 };
    })(),
    { rate: 1 },
  );

  // The first part is pulled from the generator before anything is sent, so the opening message
  // exists a turn later.
  await Bun.sleep(0);
  const p = FakePort.live[0]!;
  const id = (p.sent[0] as { id: string }).id;
  p.reply({ t: 'error', id, message: 'synth 500: synthesis failed' });

  // The page ends rather than throwing: `readBook` is holding this, and one bad chunk must not
  // end the book.
  await page;
  // The reason is reachable from the panel, which only ever had the console before...
  expect(engineError()).toMatch(/synth 500/);
  // ...and the next page asks the worker again instead of being read by a system voice.
  expect(engineKind()).toBe(kind);
  expect(spoken.join(' ')).not.toContain('gave up'); // nothing was left to re-speak
});

test('an extension reloaded out from under the page says so, rather than naming a property', async () => {
  // Exactly what Chrome leaves behind: the content script keeps running in the page, with its
  // panel and its captured state intact, and `chrome.runtime` gone. Every route back to the
  // extension then throws `Cannot read properties of undefined (reading '<whatever>')`, which
  // names the line that touched it rather than the reason, and reads as a bug in that line.
  // There is nothing to retry - this script can never reach the extension again - so the only
  // useful thing to say is the one action that fixes it.
  (globalThis as Record<string, unknown>).chrome = {};

  const n = new PortNarrator();
  await expect(n.speak('anything', {})).rejects.toThrow(/reloaded.*refresh this page/i);
  expect(FakePort.live.length).toBe(0); // and nothing was sent into the void first
});

test('a disconnect caused by the reload itself still rejects, instead of hanging the page', async () => {
  // The nastiest ordering of the three ways a port dies. An extension reload disconnects the port
  // AND removes `chrome.runtime`, and the disconnect handler read `chrome.runtime.lastError` to
  // name the reason - so it threw on the property access BEFORE rejecting anything, the throw
  // escaped into an event listener where nothing catches it, and `readBook` was left awaiting a
  // promise that could never settle. A hang, from the handler whose comment says silence here is
  // a permanent hang.
  const n = new PortNarrator();
  const speaking = n.speak('a page');
  const p = FakePort.live[0]!;

  (globalThis as Record<string, unknown>).chrome = {}; // what the reload leaves behind
  p.disconnect();

  await expect(speaking).rejects.toThrow(/reloaded.*refresh this page/i);
});
