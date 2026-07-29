// A page whose text arrives in parts.
//
// A two-column page is OCR'd a column at a time so the first word can be heard while the second
// column is still being recognized. That only works if the late text joins the utterance already
// playing and its word boundaries still address the whole page - and both are invisible when they
// break: the audio is fine and the highlight quietly points at the wrong word.

import { test, expect } from 'bun:test';
import { PartQueue, planStream, PLAYBACK_RAMP, sentenceEnd, type TextPart, type WordBoundary } from '../src/speak';

const boundary = (charIndex: number, charLength: number): WordBoundary => ({ charIndex, charLength, elapsedMs: 0 });

async function* feed(...parts: TextPart[]): AsyncGenerator<TextPart> {
  for (const p of parts) yield p;
}

async function drain<T>(it: AsyncIterable<T>): Promise<T[]> {
  const out: T[] = [];
  for await (const v of it) out.push(v);
  return out;
}

// --- offsets across parts --------------------------------------------------------------------

test('a boundary from a later part addresses the whole page', async () => {
  const page = 'One two three four. Five six seven eight. Nine ten eleven twelve.';
  const cut = 20; // just after the first sentence
  const parts = [
    { text: page.slice(0, cut), base: 0 },
    { text: page.slice(cut), base: cut },
  ];

  const plan = planStream(feed(...parts), 400, 400);
  const chunks = await drain(plan.chunks);

  // Every word of every chunk resolves to itself in the page.
  let seen = 0;
  for (let i = 0; i < chunks.length; i++) {
    for (const m of chunks[i]!.matchAll(/\S+/g)) {
      const b = plan.remap(boundary(m.index, m[0].length), i);
      expect(page.slice(b.charIndex, b.charIndex + (b.charLength ?? 0))).toBe(m[0]);
      seen++;
    }
  }
  expect(seen).toBe(page.split(/\s+/).length);
});

test('the parts need not be a running total of their own lengths', async () => {
  // A part is cut at the last sentence end, not at the column boundary, so the producer states
  // where each one sits. Summing lengths would put the second part in the wrong place entirely.
  const page = 'Alpha beta. Gamma delta epsilon.';
  const plan = planStream(feed({ text: 'Alpha beta.', base: 0 }, { text: 'Gamma delta epsilon.', base: 12 }), 400, 400);
  const chunks = await drain(plan.chunks);
  const last = chunks.length - 1;
  const gamma = [...chunks[last]!.matchAll(/\S+/g)][0]!;

  const b = plan.remap(boundary(gamma.index, gamma[0].length), last);
  expect(b.charIndex).toBe(page.indexOf('Gamma'));
});

test('only the first part ramps; later ones arrive mid-page and use the settled size', async () => {
  const long = 'This sentence is here to make the part long enough to be cut into several chunks. ';
  const first = long.repeat(12);
  const second = long.repeat(12);

  const plan = planStream(
    feed({ text: first, base: 0 }, { text: second, base: first.length + 1 }),
    PLAYBACK_RAMP,
    PLAYBACK_RAMP[PLAYBACK_RAMP.length - 1]!,
  );
  const chunks = await drain(plan.chunks);

  // The opening chunk is small, so the first word comes quickly.
  expect(chunks[0]!.length).toBeLessThan(PLAYBACK_RAMP[0]! * 2);
  // The second part starts at full size - restarting the ramp there would emit a runt chunk into
  // a stream that already has a lead built, and a runt is a chunk that finishes before the one
  // behind it is ready.
  const secondPartStarts = chunks.findIndex((_, i) => i > 0 && chunks.slice(0, i).join('').length >= first.length - 400);
  expect(chunks[secondPartStarts]!.length).toBeGreaterThan(PLAYBACK_RAMP[0]! * 2);
});

test('an empty part is skipped rather than yielding an empty chunk', async () => {
  const plan = planStream(feed({ text: '', base: 0 }, { text: 'Real text here.', base: 1 }), 400, 400);
  expect(await drain(plan.chunks)).toEqual(['Real text here.']);
});

// --- the queue -------------------------------------------------------------------------------
//
// `speakStream` parks on this between parts, so every way an utterance can end has to close it.
// A missed close is a page that never finishes, with no error anywhere.

test('parts pushed before anything reads them all come out', async () => {
  const q = new PartQueue();
  q.push({ text: 'one', base: 0 });
  q.push({ text: 'two', base: 4 });
  q.close();
  expect((await drain(q)).map((p) => p.text)).toEqual(['one', 'two']);
});

test('a reader parked on the queue wakes when a part lands', async () => {
  const q = new PartQueue();
  const reading = drain(q);
  // Nothing queued yet: the reader is parked.
  await Promise.resolve();
  q.push({ text: 'late', base: 0 });
  q.close();
  expect((await reading).map((p) => p.text)).toEqual(['late']);
});

test('closing releases a parked reader instead of hanging it', async () => {
  const q = new PartQueue();
  const reading = drain(q);
  await Promise.resolve();
  q.close();
  expect(await reading).toEqual([]);
});

test('closing does not discard parts already queued', async () => {
  // Stop closes the feed while the last column may already be in it. Those still have to come
  // out, or the tail of a page is dropped whenever the two race.
  const q = new PartQueue();
  q.push({ text: 'queued', base: 0 });
  q.close();
  expect((await drain(q)).map((p) => p.text)).toEqual(['queued']);
});

test('a push after close is ignored, not queued forever', async () => {
  const q = new PartQueue();
  q.close();
  q.push({ text: 'too late', base: 0 });
  expect(await drain(q)).toEqual([]);
  expect(q.closed).toBe(true);
});

// --- where a part may be cut -----------------------------------------------------------------

test('a part is cut after the last complete sentence', () => {
  const text = 'First one. Second one. And a trailing fragment that runs on';
  expect(sentenceEnd(text, 0)).toBe(text.indexOf('And a') - 1);
});

test('a closing quote or bracket goes with the sentence it ends', () => {
  const text = 'He said "go home." Then the rest carries on';
  expect(text.slice(0, sentenceEnd(text, 0))).toBe('He said "go home."');
});

test('an abbreviation mid-word is not a sentence end', () => {
  // The lookahead requires whitespace or the end of the string after the stop, so a decimal or a
  // mid-token dot cannot cut a part.
  const text = 'Version 3.14 shipped today and the rest runs on';
  expect(sentenceEnd(text, 0)).toBe(0);
});

test('no sentence end at all cuts nothing rather than cutting mid-sentence', () => {
  expect(sentenceEnd('a column with no full stop anywhere in it', 0)).toBe(0);
});

test('only sentence ends at or after the mark count', () => {
  const text = 'Already spoken. Not yet spoken. Trailing fragment';
  const from = text.indexOf('Not yet');
  expect(sentenceEnd(text, from)).toBe(text.indexOf('Trailing') - 1);
});
