// Picking the word a boundary names.
//
// The mark is only ever as right as this lookup: everything downstream is arithmetic on a bbox
// that is already known to be correct. What is NOT guaranteed is that a boundary lands cleanly
// inside a word - `chrome.tts` reports the leading space on some voices, and the Kokoro marks
// are an estimate to begin with - so the gap cases are the ones worth pinning down.

import { test, expect } from 'bun:test';
import { markRect, relocate, wordAt, wordIndexAt } from '../src/content/highlight';
import type { OcrWord } from '../src/ocr';

/** OCR words for `text`, laid out the way `src/ocr/` emits them. */
function words(text: string): OcrWord[] {
  return [...text.matchAll(/\S+/g)].map((m, i) => ({
    text: m[0],
    confidence: 95,
    bbox: { x0: i * 100, y0: 0, x1: i * 100 + 80, y1: 20 },
    charStart: m.index,
    charLen: m[0].length,
  }));
}

const TEXT = 'Call me Ishmael.';
const WORDS = words(TEXT);

test('a boundary inside a word picks that word', () => {
  expect(wordAt(WORDS, 0)!.text).toBe('Call');
  expect(wordAt(WORDS, 2)!.text).toBe('Call');
  expect(wordAt(WORDS, 5)!.text).toBe('me');
  expect(wordAt(WORDS, 12)!.text).toBe('Ishmael.');
});

test('a boundary in the gap goes to the nearer word rather than nowhere', () => {
  // Index 4 is the space after "Call" - equidistant, and the word just ended, so it stays put.
  expect(wordAt(WORDS, 4)!.text).toBe('Call');
  expect(wordAt(WORDS, 7)!.text).toBe('me');
});

test('offsets outside the page clamp to its ends instead of returning null', () => {
  // A highlight that vanishes reads as narration having died; the first/last word does not.
  expect(wordAt(WORDS, -5)!.text).toBe('Call');
  expect(wordAt(WORDS, 9_000)!.text).toBe('Ishmael.');
});

test('a page with no words at all is null, not a throw', () => {
  expect(wordAt([], 3)).toBeNull();
});

// --- surviving a re-render ------------------------------------------------------------------
//
// Resizing the window makes the reader re-render: new blob: URL, new layout, text on different
// lines. The narrator keeps reporting offsets against the OCR taken at the start of the page, so
// the word has to be found again in the new one. This is what stopped the mark vanishing for the
// rest of the page after a resize.

const PASSAGE = 'Call me Ishmael. Some years ago, never mind how long precisely, having little money.';

test('a word is found again after the page reflows', () => {
  const before = words(PASSAGE);
  // Same words, re-OCR'd: different boxes, and the offsets restart because the line breaks moved.
  const after = words(PASSAGE.replace(/ /g, '\n'));
  for (let k = 0; k < before.length; k++) {
    expect(relocate(before, k, after)!.text).toBe(before[k]!.text);
  }
});

test('the first and last words relocate too, with only one neighbour to go on', () => {
  const before = words(PASSAGE);
  const after = words(PASSAGE);
  expect(relocate(before, 0, after)!.text).toBe('Call');
  expect(relocate(before, before.length - 1, after)!.text).toBe('money.');
});

test('OCR disagreeing about case or punctuation is not a mismatch', () => {
  const before = words('the quick brown fox');
  const after = words('The, "quick" brown fox');
  expect(relocate(before, 1, after)!.text).toBe('"quick"');
});

test('a word the reflow pushed off the page draws nothing rather than guessing', () => {
  const before = words(PASSAGE);
  const after = words('An entirely different page of text.');
  expect(relocate(before, 3, after)).toBeNull();
});

test('an ambiguous match draws nothing - a mark in the wrong place is worse than none', () => {
  // "on" appears twice with identical neighbours. Picking either is a coin flip, and a highlight
  // that is confidently wrong costs more trust than one that is briefly absent.
  const before = words('so it goes on and on and on');
  const after = words('so it goes on and on and on');
  expect(relocate(before, 5, after)).toBeNull();
  // The unambiguous words around it still resolve.
  expect(relocate(before, 0, after)!.text).toBe('so');
});

test('a repeated word with different neighbours is still unambiguous', () => {
  const before = words('the cat sat on the mat');
  const after = words('the cat sat\non the mat');
  expect(relocate(before, 4, after)!.charStart).toBe(after[4]!.charStart); // "the" before "mat"
});

// --- geometry -------------------------------------------------------------------------------
//
// The mapping is four lines of arithmetic and every way of getting it wrong looks the same on
// screen: a mark that is nearly right at the top of the page and further off with every line.

/** A word 100px in and 40px down on a 1000x1400 capture. */
const BOX = { x0: 100, y0: 40, x1: 180, y1: 60 };
const NATURAL = { w: 1000, h: 1400 };

test('at natural size the box lands on the word, padded a little', () => {
  const r = markRect(BOX, { left: 0, top: 0, width: 1000, height: 1400 }, NATURAL)!;
  // Height 20 + padding on both sides; the box grows around the ink, never shifts off it.
  expect(r.height).toBeGreaterThan(20);
  expect(r.left).toBeLessThan(100);
  expect(r.left + r.width).toBeGreaterThan(180);
  expect(r.top).toBeLessThan(40);
  expect(r.top + r.height).toBeGreaterThan(60);
  // ...but only just: padding is a few pixels, not a second word's worth.
  expect(100 - r.left).toBeLessThan(8);
});

test('a CSS-scaled image scales the box with it', () => {
  const half = markRect(BOX, { left: 0, top: 0, width: 500, height: 700 }, NATURAL)!;
  const full = markRect(BOX, { left: 0, top: 0, width: 1000, height: 1400 }, NATURAL)!;
  expect(half.left).toBeCloseTo(full.left / 2);
  expect(half.width).toBeCloseTo(full.width / 2);
  expect(half.height).toBeCloseTo(full.height / 2);
});

test('the image rect is already viewport-space, so its offset is added once and only once', () => {
  // getBoundingClientRect has applied scroll and devicePixelRatio already. Applying either
  // again is the bug this pins: the mark tracks the image, whatever the image is doing.
  const scrolled = markRect(BOX, { left: 24, top: -300, width: 1000, height: 1400 }, NATURAL)!;
  const rested = markRect(BOX, { left: 0, top: 0, width: 1000, height: 1400 }, NATURAL)!;
  expect(scrolled.left - rested.left).toBeCloseTo(24);
  expect(scrolled.top - rested.top).toBeCloseTo(-300);
});

test('an image with no size yet is null rather than a NaN rect', () => {
  // Mid-swap the reader hands us an <img> that is laid out at zero. A NaN here writes
  // "NaNpx" into the style and the mark silently never appears again.
  expect(markRect(BOX, { left: 0, top: 0, width: 0, height: 0 }, NATURAL)).toBeNull();
  expect(markRect(BOX, { left: 0, top: 0, width: 800, height: 1000 }, { w: 0, h: 0 })).toBeNull();
});

test('the search holds over a page-sized word list', () => {
  const page = Array.from({ length: 900 }, (_, i) => `word${i}`).join(' ');
  const list = words(page);
  for (const w of list) {
    expect(wordAt(list, w.charStart)!.text).toBe(w.text);
    expect(wordAt(list, w.charStart + w.charLen - 1)!.text).toBe(w.text);
  }
});
