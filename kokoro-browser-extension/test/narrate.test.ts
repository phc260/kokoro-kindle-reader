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
      lastError: undefined,
      connect: () => {
        const p = new FakePort();
        FakePort.live.push(p);
        return p;
      },
    },
  };
});

const { PortNarrator } = await import('../src/content/narrate');

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
