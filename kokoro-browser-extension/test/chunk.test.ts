// Chunking is where continuous narration is won or lost.
//
// Kokoro synthesizes ~3.4x faster than the ear consumes, so chunk i's audio pays for chunk
// i+1's synthesis - as long as i+1 is not much more than 3.4x the size of i. Every rule here
// exists because breaking it produced measurable silence in backend/test-pipeline.ts:
// a 16-character opening sentence emitted alone left a 2-second hole behind it.

import { test, expect } from 'bun:test';
import path from 'node:path';
import { chunk, PLAYBACK_RAMP } from '../src/speak';

const fixture = await Bun.file(path.join(import.meta.dir, 'ocr-fixture.html')).text();
const PROSE = fixture.match(/GROUND_TRUTH\s*=\s*`([\s\S]*?)`/)![1]!.replace(/\s+/g, ' ').trim();

/** A page's worth, the way the bench builds one. */
let page = PROSE;
while (page.split(' ').length < 840) page += ' ' + PROSE;

test('the ramp starts small, so the first word arrives quickly', () => {
  const out = chunk(page, PLAYBACK_RAMP);
  expect(out[0]!.length).toBeLessThan(PLAYBACK_RAMP[0]! * 2);
});

test('no runt chunks: nothing finishes before the next chunk can be synthesized', () => {
  const out = chunk(page, PLAYBACK_RAMP);
  // The last chunk is whatever text remains, so it is allowed to be short.
  out.slice(0, -1).forEach((c, i) => {
    const budget = PLAYBACK_RAMP[Math.min(i, PLAYBACK_RAMP.length - 1)]!;
    expect(c.length).toBeGreaterThanOrEqual(budget * 0.6);
  });
});

test('chunk sizes never jump faster than synthesis can keep up', () => {
  // The measured margin is 3.4x. Assert 3x so a small regression in throughput does not
  // immediately mean audible gaps.
  const out = chunk(page, PLAYBACK_RAMP);
  for (let i = 1; i < out.length; i++) {
    expect(out[i]!.length / out[i - 1]!.length).toBeLessThan(3);
  }
});

test('chunking is lossless once whitespace is normalized', () => {
  for (const schedule of [400, 120, PLAYBACK_RAMP] as const) {
    expect(chunk(page, schedule).join(' ')).toBe(page);
  }
});

test('a long sentence is split at clauses, never mid-clause', () => {
  const long =
    'It is a way I have of driving off the spleen, and regulating the circulation, ' +
    'whenever I find myself growing grim about the mouth, and it is a high time to get to sea.';
  const out = chunk(long, 40);
  expect(out.length).toBeGreaterThan(1);
  // Every break lands after a clause- or sentence-ending mark.
  for (const c of out.slice(0, -1)) expect(c).toMatch(/[,;:.!?]$/);
});

test('a sentence with no internal punctuation is never broken up', () => {
  const unbreakable = 'a'.repeat(300);
  expect(chunk(`${unbreakable}.`, 40)).toEqual([`${unbreakable}.`]);
});

test('a fixed size still respects its budget where the text allows', () => {
  // Overshoot is permitted only to avoid a runt or to keep an atom whole.
  const out = chunk(page, 400);
  const over = out.filter((c) => c.length > 400 * 1.6);
  expect(over).toEqual([]);
});

test('the schedule repeats its last entry rather than running out', () => {
  const out = chunk(page, [40, 400]);
  expect(out[0]!.length).toBeLessThan(120);
  expect(out.slice(1, -1).every((c) => c.length > 200)).toBe(true);
});
