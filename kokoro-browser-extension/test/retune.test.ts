// Changing the speed while a page is being read.
//
// Speed is a synthesis parameter, so the audio already rendered ahead of the ear - up to
// MAX_LEAD_S of it, which is the whole point of the lead - cannot be adjusted. It has to be thrown
// away and asked for again, which makes this the one place in the streaming path that sends the
// same chunk twice. Two things break silently if it is wrong: a chunk re-sent under a NEW index
// puts every later word boundary on the wrong word, and a chunk that is dropped without being
// re-sent removes a sentence from the book.
//
// Driven through a fake offscreen document, because none of this is reachable otherwise: the real
// one needs an AudioContext and the real host needs to synthesize.

import { test, expect, beforeEach } from 'bun:test';

/** One piece of audio scheduled on the fake cursor. Mirrors `sources` in offscreen.ts. */
interface Scheduled {
  index: number;
  offset: number;
  speed: number;
  at: number;
  end: number;
}

/**
 * The audio end, modelled on offscreen.ts: one cursor, pieces scheduled back to back, and a clock
 * that only moves when asked.
 *
 * Every piece is one second of audio. The clock stands still while the page is being sent - so the
 * queue builds up the way a real one does, since synthesis outruns the ear - and the feed lets it
 * run once there is nothing left to send, which is what lets `speakAll` finish.
 */
class FakeOffscreen {
  epoch = 0;
  now = 0;
  cursor = 0;
  /** Seconds the clock jumps on each status poll. Zero until the page has all been sent. */
  tick = 0;
  scheduled: Scheduled[] = [];
  /** Every `http-synth` that was accepted, in order. */
  sent: { index: number; offset: number; speed: number; text: string }[] = [];
  /** Called as each piece is accepted, so a test can change the speed at a known moment. */
  onSynth: ((m: { index: number; offset: number; speed: number }) => void) | null = null;
  /** Set to make the next `audio-retune` report the stream gone. */
  stopOnRetune = false;

  handle(msg: Record<string, unknown>): Record<string, unknown> {
    const t = msg.t as string;
    const stale = msg.epoch !== undefined && msg.epoch !== this.epoch;

    switch (t) {
      case 'audio-start':
        this.epoch++;
        this.scheduled = [];
        this.now = 0;
        this.cursor = 0;
        return { ok: true, epoch: this.epoch };

      case 'audio-status': {
        if (stale) return { ok: true, stale: true };
        this.now += this.tick;
        this.scheduled = this.scheduled.filter((s) => s.end > this.now);
        return { ok: true, queued: this.scheduled.length, lead: Math.max(0, this.cursor - this.now) };
      }

      case 'http-synth': {
        if (stale) return { ok: true, stale: true };
        const piece = {
          index: msg.index as number,
          offset: (msg.offset as number) ?? 0,
          speed: msg.speed as number,
        };
        const at = Math.max(this.now, this.cursor);
        this.cursor = at + 1;
        this.scheduled.push({ ...piece, at, end: this.cursor });
        this.sent.push({ ...piece, text: msg.text as string });
        this.onSynth?.(piece);
        return { ok: true, lead: this.cursor - this.now };
      }

      // The flush: what is being heard survives, everything scheduled behind it does not.
      case 'audio-retune': {
        if (stale) return { ok: true, stale: true };
        if (this.stopOnRetune) {
          this.epoch++; // as a Stop would
          return { ok: true, stale: true };
        }
        const keep = this.scheduled.filter((s) => s.at <= this.now);
        const drop = this.scheduled.filter((s) => s.at > this.now);
        this.scheduled = keep;
        this.cursor = keep.reduce((end, s) => Math.max(end, s.end), this.now);
        const first = drop[0];
        return {
          ok: true,
          resume: first ? { index: first.index, offset: first.offset } : undefined,
          lead: this.cursor - this.now,
        };
      }
    }
    return { ok: true };
  }
}

let fake: FakeOffscreen;

beforeEach(() => {
  fake = new FakeOffscreen();
  (globalThis as Record<string, unknown>).chrome = {
    runtime: {
      sendMessage: (msg: Record<string, unknown>) => Promise.resolve(fake.handle(msg)),
      onMessage: { addListener: () => {}, removeListener: () => {} },
    },
  };
});

const { KokoroHttpNarrator } = await import('../src/kokoro-http');
const { waitForRoom, MAX_LEAD_S } = await import('../src/offscreen-client');
const { PLAYBACK_RAMP } = await import('../src/speak');

const narrator = () => new KokoroHttpNarrator({ base: 'http://127.0.0.1:8787', token: 'x'.repeat(32) });

/** A page's chunks. Letting the clock run at the end is what drains the queue - see the class doc. */
async function* page(texts: string[]): AsyncGenerator<string> {
  for (const t of texts) yield t;
  fake.tick = 1000;
}

const TEXTS = ['zero.', 'one.', 'two.', 'three.', 'four.', 'five.'];

/** The speed each index was LAST sent at - i.e. the one that is actually going to be heard. */
function finalSpeeds(): Map<number, number> {
  const out = new Map<number, number>();
  for (const s of fake.sent) out.set(s.index, s.speed);
  return out;
}

test('a speed change re-sends the chunks nobody has heard yet, at their original indices', async () => {
  const opts = { rate: 1 };
  // The reader moves the slider while the third chunk is being synthesized.
  fake.onSynth = (m) => {
    if (m.index === 2) opts.rate = 1.5;
  };

  await narrator().speakAll(page(TEXTS), opts);

  // Nothing is lost: every chunk of the page was sent, and the last send of each is the one that
  // ends up on the cursor.
  const speeds = finalSpeeds();
  expect([...speeds.keys()].sort((a, b) => a - b)).toEqual([0, 1, 2, 3, 4, 5]);

  // Chunk 0 was already playing when the slider moved, so it is left alone at the old speed -
  // cutting it off mid-word would be an audible click in exchange for one second.
  expect(speeds.get(0)).toBe(1);
  // Everything behind it is heard at the new speed, including the two that had already been sent.
  for (const i of [1, 2, 3, 4, 5]) expect(speeds.get(i)).toBe(1.5);

  // Re-sent BY INDEX, with the text that index already had. A re-send under a fresh index would
  // remap every later word boundary onto the wrong word (see `planStream`'s owners).
  const resent = fake.sent.filter((s) => s.index === 1);
  expect(resent).toHaveLength(2);
  expect(resent.map((s) => s.text)).toEqual([TEXTS[1]!, TEXTS[1]!]);
  expect(resent.map((s) => s.speed)).toEqual([1, 1.5]);
});

test('the tail of a page is retuned too, not just what had yet to be chunked', async () => {
  // The change lands after the last chunk was sent, while the queue is still minutes deep. This is
  // the case a "apply it to chunks not yet sent" fix would miss entirely.
  const opts = { rate: 1 };
  fake.onSynth = (m) => {
    if (m.index === TEXTS.length - 1) opts.rate = 0.5;
  };

  await narrator().speakAll(page(TEXTS), opts);

  const speeds = finalSpeeds();
  expect(speeds.get(0)).toBe(1); // being heard
  for (const i of [1, 2, 3, 4, 5]) expect(speeds.get(i)).toBe(0.5);
});

test('with the speed left alone, every chunk is sent exactly once', async () => {
  await narrator().speakAll(page(TEXTS), { rate: 1 });
  expect(fake.sent.map((s) => s.index)).toEqual([0, 1, 2, 3, 4, 5]);
  expect(fake.sent.map((s) => s.text)).toEqual(TEXTS);
});

test('a speed change is not consumed by a Stop racing it', async () => {
  // Stop lands between the slider and the flush. The utterance ends; it must not carry on sending
  // the rest of the page into a stream that is gone.
  const opts = { rate: 1 };
  fake.stopOnRetune = true;
  fake.onSynth = (m) => {
    if (m.index === 1) opts.rate = 2;
  };

  await narrator().speakAll(page(TEXTS), opts);

  expect(fake.sent.map((s) => s.index)).toEqual([0, 1]);
});

/** Poll until `cond` holds. Timing out here means a wait was never interrupted. */
async function waitFor(cond: () => boolean, what: string): Promise<void> {
  for (let i = 0; i < 200; i++) {
    if (cond()) return;
    await new Promise((r) => setTimeout(r, 10));
  }
  throw new Error(`timed out waiting for ${what}`);
}

test('a speed change during the wait for the next column is acted on, and costs no text', async () => {
  // A two-column page feeds ONE utterance, so the send loop waits on the iterator while the second
  // column is being recognized - a wait with no upper bound and, until it was raced, the one place
  // in the loop a slider move could sit unnoticed. Racing it must not consume the pull: an
  // iterator's value is gone once `next()` has produced it.
  const opts = { rate: 1 };
  let recognized!: () => void;
  const secondColumn = new Promise<void>((r) => (recognized = r));

  async function* columns(): AsyncGenerator<string> {
    yield TEXTS[0]!;
    yield TEXTS[1]!;
    await secondColumn;
    yield TEXTS[2]!;
    fake.tick = 1000;
  }

  const done = narrator().speakAll(columns(), opts);

  await waitFor(() => fake.sent.length === 2, 'the first column to be sent');
  opts.rate = 1.5;
  await waitFor(() => fake.sent.some((s) => s.speed === 1.5), 'the flush to happen during the wait');

  recognized();
  await done;

  const speeds = finalSpeeds();
  expect([...speeds.keys()].sort((a, b) => a - b)).toEqual([0, 1, 2]);
  expect(speeds.get(0)).toBe(1); // being heard
  expect(speeds.get(1)).toBe(1.5);
  // The column that arrived after the flush - the pull was held, not dropped, so it is still spoken.
  expect(speeds.get(2)).toBe(1.5);
});

// --- the gap the flush would otherwise leave --------------------------------------------------
//
// A flush hands back the lead, so synthesis is where it is at the start of a page: nothing
// buffered. A settled-size chunk takes ~5.8s to render, and whenever the chunk still playing had
// less than that left the reader heard a silence one to two sentences long. The first chunk after a
// flush therefore re-enters PLAYBACK_RAMP, exactly as the opening of a page does.

/** A settled-size chunk: several sentences, past the last ramp size. */
const CHUNK = [
  'The first sentence runs along for a while before it stops.',
  'The second one follows it closely, with a clause in the middle of it.',
  'A third arrives, and it is shorter.',
  'The fourth sentence is long enough to carry this chunk past the settled size, which is where the',
  'ramp stops growing.',
  'A fifth follows to be sure of it, and it carries a comma of its own.',
  'The sixth and last one takes the whole thing past four hundred characters, which is the size a',
  'chunk settles at once a page is running.',
].join(' ');

const ink = (s: string) => s.replace(/\s+/g, '');

test('the first chunk after a flush is re-sent in ramp-sized pieces, not whole', async () => {
  const opts = { rate: 1 };
  const texts = [CHUNK, CHUNK, CHUNK];
  fake.onSynth = (m) => {
    if (m.index === 1) opts.rate = 1.5;
  };

  await narrator().speakAll(page(texts), opts);

  const resent = fake.sent.filter((s) => s.index === 1 && s.speed === 1.5);
  expect(resent.length).toBeGreaterThan(1);

  // A sound comes out in well under a second, instead of the ~5.8s a settled chunk takes.
  expect(resent[0]!.text.length).toBeLessThan(PLAYBACK_RAMP[0]! * 2);

  // And the pieces are the chunk: no word said twice, none dropped. Compared as ink because
  // `chunk()` normalizes whitespace.
  expect(resent.map((p) => ink(p.text)).join('')).toBe(ink(CHUNK));

  // Each piece states where it starts, which is what keeps its word marks on the right words.
  for (const p of resent) expect(ink(CHUNK.slice(p.offset)).startsWith(ink(p.text))).toBe(true);

  // Everything behind the re-ramped chunk goes out whole again - by then the lead covers a chunk.
  expect(fake.sent.filter((s) => s.index === 2)).toHaveLength(1);
});

test('a second change mid-ramp resumes at the next piece, not at the start of the chunk', async () => {
  // Without the offset on the wire the flush would only know which CHUNK to resume, and a chunk
  // half-heard in pieces would be started again from the top - the reader hears a sentence twice.
  const opts = { rate: 1 };
  const texts = [CHUNK, CHUNK];
  let pieces = 0;

  fake.onSynth = (m) => {
    if (m.index === 1 && m.speed === 1) opts.rate = 1.5; // flush, then re-send chunk 1 in pieces
    if (m.index === 1 && m.speed === 1.5 && ++pieces === 2) {
      // The first piece is playing by now; only what is behind it may be discarded.
      fake.now = 1.5;
      opts.rate = 0.75;
    }
  };

  await narrator().speakAll(page(texts), opts);

  const ramped = fake.sent.filter((s) => s.index === 1 && s.speed === 1.5);
  const again = fake.sent.filter((s) => s.index === 1 && s.speed === 0.75);
  expect(again.length).toBeGreaterThan(0);
  // The piece that was already playing is not said a second time.
  expect(again.every((p) => p.offset >= ramped[1]!.offset)).toBe(true);
  expect(again.some((p) => p.offset === 0)).toBe(false);
});

test('a full buffer stops waiting the moment the speed changes', async () => {
  // Where a page spends most of its time. Waiting the throttle out first would add seconds to the
  // one change the reader is actually listening for.
  fake.epoch = 1;
  fake.cursor = MAX_LEAD_S + 10; // buffer full, and it is not going to empty on its own

  let polls = 0;
  const started = Date.now();
  const answer = await waitForRoom(1, () => ++polls > 1);

  expect(answer).toBe('interrupted');
  expect(Date.now() - started).toBeLessThan(1000); // not the 2s nap it would otherwise take
});

test('the throttle still reports room and a dead stream without an interrupt', async () => {
  fake.epoch = 1;
  expect(await waitForRoom(1)).toBe('room');
  expect(await waitForRoom(99)).toBe('stopped');
});
