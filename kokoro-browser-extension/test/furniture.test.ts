// What the narration is allowed to leave out.
//
// This is the only rule in the pipeline that decides a line of the book will not be read, and it
// fails silently in both directions: keep a running head and it is spoken between every page,
// drop a heading or a body line and the book quietly loses part of itself. It also carries state
// ACROSS pages, which is what makes it worth testing directly rather than through a whole
// recognition pass.
//
// The balance it strikes is deliberate and was arrived at the hard way: nothing is dropped on a
// guess about how a line LOOKS, because a section heading looks exactly like a running head. Only
// an unambiguous pattern (a folio, a copyright line) or verbatim repetition on another page will
// remove anything.

import { test, expect, beforeEach } from 'bun:test';
import {
  furnitureCheckpoint,
  furnitureReason,
  looksInterleaved,
  measureOf,
  pageToken,
  resetFurnitureMemory,
  type RawWord,
} from '../src/content/ocr';

const PAGE_H = 1000;
/** Width of a full line of body text in the test column. */
const MEASURE = 720;

/** The furniture rule as a column sees it. `token` is which page we are pretending to be on. */
const on = (token: string, measure = MEASURE) => ({ height: PAGE_H, width: MEASURE, measure, token });

/** One OCR'd line of `text`, with its middle at `y` on a 1000px-tall page. */
function line(text: string, y: number): RawWord[] {
  let x = 0;
  return text.split(' ').map((word) => {
    const w = word.length * 12;
    const box = { text: word, confidence: 95, bbox: { x0: x, y0: y - 10, x1: x + w, y1: y + 10 } };
    x += w + 8;
    return box;
  });
}

/** The same line, justified: stretched evenly to exactly `width`, starting at `x0`. */
function justified(text: string, y: number, width = MEASURE, x0 = 0): RawWord[] {
  const words = text.split(' ');
  const ink = words.reduce((n, w) => n + w.length * 12, 0);
  const gap = words.length > 1 ? (width - ink) / (words.length - 1) : 0;
  let x = x0;
  return words.map((word) => {
    const w = word.length * 12;
    const box = { text: word, confidence: 95, bbox: { x0: x, y0: y - 10, x1: x + w, y1: y + 10 } };
    x += w + gap;
    return box;
  });
}

beforeEach(resetFurnitureMemory);

// --- what still gets removed -----------------------------------------------------------------

test('a copyright line goes whatever height it sits at', () => {
  // The only text still removed for what it says. No book has this in its body, and it turns up
  // at the end of a chapter where a repeat may never come.
  expect(furnitureReason(line('All rights reserved.', 500), on('p1'))).toBe('pattern');
  expect(furnitureReason(line('© 2026 Harbour Press Limited', 500), on('p1'))).toBe('pattern');
});

test('a folio is caught by its slot, because its text never repeats', () => {
  // 293, 294, 295 - the number changes every page, so no amount of verbatim matching will ever
  // catch it. What repeats is a numeric line in the same corner.
  const folio = (n: string) => line(n, 28);
  expect(furnitureReason(folio('[293]'), on('p1'))).toBeNull(); // read once
  expect(furnitureReason(folio('[294]'), on('p2'))).toBe('folio');
  expect(furnitureReason(folio('[295]'), on('p3'))).toBe('folio');
});

test('a lone number that is NOT in a folio slot is read', () => {
  // A numbered list item, a verse number, a figure caption. Nothing but the shape says folio, and
  // shape alone is not evidence - which is exactly the rule that used to eat these.
  expect(furnitureReason(line('42', 500), on('p1'))).toBeNull();
  expect(furnitureReason(line('42', 500), on('p2'))).toBeNull();
});

test('a folio slot is a position, so the other corner is a different slot', () => {
  const w = 720;
  const at = (x: number, y: number): RawWord[] => [
    { text: '293', confidence: 95, bbox: { x0: x, y0: y - 10, x1: x + 40, y1: y + 10 } },
  ];
  expect(furnitureReason(at(w - 60, 28), on('p1'))).toBeNull();
  expect(furnitureReason(at(w - 60, 28), on('p2'))).toBe('folio'); // same corner, next page
  expect(furnitureReason(at(0, 28), on('p3'))).toBeNull(); // other corner, never seen before
});

test('a sentence opening with a year is read', () => {
  // The old pattern had a "year followed by a capital" branch for copyright footers. It also
  // matched this.
  expect(furnitureReason(line('1997 Grace wrote to them.', 28), on('p1'))).toBeNull();
});

test('a running head is read once, then never again', () => {
  // Not dropped on sight: on a first look it is indistinguishable from a section heading, and
  // guessing wrong there removes part of the book. It condemns itself by coming back.
  expect(furnitureReason(line('FIELD-GUIDE TO HARBOURS', 30), on('p1'))).toBeNull();
  expect(furnitureReason(line('FIELD-GUIDE TO HARBOURS', 30), on('p2'))).toBe('repeats');
  expect(furnitureReason(line('FIELD-GUIDE TO HARBOURS', 30), on('p3'))).toBe('repeats');
});

test('a repeat has to be on ANOTHER page, not the same one read again', () => {
  // Re-reading a page is routine: a re-render while narrating, or readPage() before Play.
  // Counting those would condemn a heading on the second look at the page it belongs to.
  const heading = line('A CHANGING COASTLINE', 40);
  expect(furnitureReason(heading, on('p1'))).toBeNull();
  expect(furnitureReason(heading, on('p1'))).toBeNull();
  expect(furnitureReason(heading, on('p1'))).toBeNull();
  expect(furnitureReason(heading, on('p2'))).toBe('repeats');
});

// --- what must survive -----------------------------------------------------------------------

test('section headings at the top of a column are read', () => {
  // The regression this replaces. A whole chapter on one page puts three of these in the top
  // band of the left column and another at the top of the right, and every one was silently
  // dropped: short, near the edge, no closing punctuation - a running head by every test that
  // does not involve seeing the next page.
  for (const heading of ['CHAPTER THIRTY-SIX', 'KEEPING THE LAMPS OF THE HARBOUR', 'A CHANGING COASTLINE']) {
    expect(furnitureReason(line(heading, 40), on('p1'))).toBeNull();
  }
});

test('body text in the middle of the page is never furniture', () => {
  expect(furnitureReason(line('care for the lantern.', 500), on('p1'))).toBeNull();
  expect(furnitureReason(line('vital to the work of keepers.', 500), on('p1'))).toBeNull();
});

test('a long line in the band is body text, however near the edge it sits', () => {
  const long = 'see, on the one hand, the living the harbour should have, and, on the other hand,';
  expect(furnitureReason(line(long, 25), on('p1'))).toBeNull();
});

test('a short body line at the top of a column survives being read again', () => {
  // Ends a paragraph, so it reaches nothing like the column measure; the sentence-end escape is
  // what keeps it, and it must not enter the running-head memory on the way past.
  const body = 'the responsibility the harbour should bear.';
  expect(furnitureReason(line(body, 55), on('p1'))).toBeNull();
  expect(furnitureReason(line(body, 55), on('p1'))).toBeNull();
  expect(furnitureReason(line(body, 55), on('p2'))).toBeNull();
});

test('a full-measure line at the top of a column is body text, however it ends', () => {
  // Seven words, top of the page, ends mid-clause on "to". Only the measure separates it from a
  // running head - and at a larger font nearly every line of the book looks like this.
  const opening = justified('To tend the lamp each night, to', 60);
  expect(furnitureReason(opening, on('p1'))).toBeNull();
  expect(furnitureReason(opening, on('p2'))).toBeNull();
});

test('the bottom band is treated exactly like the top one', () => {
  // In two columns the LEFT column's last lines land here, mid-sentence and short.
  expect(furnitureReason(line('would tend the lamp alone.', 960), on('p1'))).toBeNull();
  expect(furnitureReason(line('would tend the lamp alone.', 960), on('p2'))).toBeNull();
  // A real running foot still goes, once it has repeated.
  expect(furnitureReason(line('Chapter Four', 965), on('p1'))).toBeNull();
  expect(furnitureReason(line('Chapter Four', 965), on('p2'))).toBe('repeats');
});

// --- the measure -----------------------------------------------------------------------------
//
// Body text is set to the column's measure and a running head is not, which is what keeps
// justified prose out of the running-head memory entirely - it can never be dropped as a repeat,
// however often the same sentence turns up in a band.

test('a repeated full-measure line is body text, not a running head', () => {
  const prose = justified('and this beacon is our lantern so we', 40);
  expect(furnitureReason(prose, on('p1'))).toBeNull();
  expect(furnitureReason(prose, on('p2'))).toBeNull();
  expect(furnitureReason(prose, on('p3'))).toBeNull();
});

test('a full-width title-and-folio header is not mistaken for justified text', () => {
  // Spans the measure, but as two groups with one huge gap - not stretched evenly between every
  // pair of words the way justification does it. Without the spacing check it would look like
  // body text and never be eligible to repeat itself out of the narration.
  const header = (): RawWord[] => [
    { text: 'HARBOURS', confidence: 95, bbox: { x0: 0, y0: 20, x1: 200, y1: 40 } },
    { text: 'lantern', confidence: 95, bbox: { x0: 680, y0: 20, x1: 720, y1: 40 } },
    { text: 'x', confidence: 95, bbox: { x0: 725, y0: 20, x1: 730, y1: 40 } },
  ];
  expect(furnitureReason(header(), on('p1'))).toBeNull();
  expect(furnitureReason(header(), on('p2'))).toBe('repeats');
});

test('a justified line whose words the recognizer split still reads as body text', () => {
  // Split words sit touching, so the median gap is ~0 and every real gap looks enormous beside
  // it. The floor is what keeps such a line passing - it is ordinary prose.
  const split: RawWord[] = [];
  let x = 0;
  const pieces = [
    ['To', false],
    ['ten', true],
    ['d', false],
    ['lanter', true],
    ['ns', false],
    ['of', false],
    ['sea', false],
  ] as const;
  for (const [word, tight] of pieces) {
    const w = word.length * 20;
    split.push({ text: word, confidence: 95, bbox: { x0: x, y0: 50, x1: x + w, y1: 70 } });
    x += w + (tight ? 0 : 30);
  }
  expect(furnitureReason(split, on('p1', x - 30))).toBeNull();
  expect(furnitureReason(split, on('p2', x - 30))).toBeNull();
});

test('the measure is the column, not its widest accident', () => {
  const lines = [
    justified('one two three four five', 100),
    justified('six seven eight nine ten', 140),
    justified('eleven twelve thirteen', 180),
    // A stray mark at the margin merged into one line; it must not set the bar.
    justified('a', 220, MEASURE * 3),
  ];
  expect(measureOf(lines)).toBeLessThanOrEqual(MEASURE * 1.05);
  expect(measureOf([])).toBe(0);
});

// --- the page fingerprint --------------------------------------------------------------------

test('a reflowed page is the same page', () => {
  // A resize rewraps the text onto different lines and rehyphenates it. The token has to see
  // through all of that, or a re-render counts as a new page and the memory condemns a heading
  // that has only ever appeared once.
  const wrapped = [line('To tend the lamp', 40), line('each night, to keep', 60), line('the proper light burning.', 80)];
  const rewrapped = [line('To tend the lamp each', 40), line('night, to keep the harbour', 60), line('light burning.', 80)];
  expect(pageToken(rewrapped)).toBe(pageToken(wrapped));
});

test('a different page is a different page', () => {
  expect(pageToken([line('Keeping the lantern', 40)])).not.toBe(pageToken([line('A changing coastline', 40)]));
});

// --- reading across a missed gutter ----------------------------------------------------------
//
// `findGutter` needs a band free of ink over almost the whole page height, so ONE figure or rule
// crossing the gutter hides it - and the page is then read across its columns line by line into
// sentences that are fluent and wrong. Nothing downstream can notice: every word is real and the
// confidence is high. The signature it leaves is a huge gap in the middle of every full-width
// line, which justification never produces.

/** A line built from two groups of words with `gap` pixels of nothing between them. */
function twoGroups(left: string, right: string, y: number, gap: number): RawWord[] {
  const out = justified(left, y, 300, 0);
  const start = 300 + gap;
  for (const w of justified(right, y, 300, start)) out.push(w);
  return out;
}

test('a page read across a gutter is detected', () => {
  const lines = [
    twoGroups('the harbour should have', 'the lamp is lit', 100, 120),
    twoGroups('and on the other hand', 'even the harbour light', 140, 120),
    twoGroups('the responsibility of it', 'realized as the life', 180, 120),
    twoGroups('in the turning beam of', 'giving beacon and this', 220, 120),
  ];
  expect(looksInterleaved(lines)).toBe(true);
});

test('an ordinary justified page is not', () => {
  const lines = [
    justified('the harbour should have and on the other hand', 100),
    justified('the responsibility that the harbour should bear', 140),
    justified('in reminding the crews to watch closely for rocks', 180),
    justified('he spoke from his status as a keeper on the point', 220),
  ];
  expect(looksInterleaved(lines)).toBe(false);
});

test('too little text to judge is left alone', () => {
  // A title page, or a column with two lines on it. Re-splitting on that evidence would be a
  // guess, and the cost of guessing wrong here is the whole page in the wrong order.
  expect(looksInterleaved([justified('FIELD-GUIDE TO HARBOURS', 100)])).toBe(false);
  expect(looksInterleaved([])).toBe(false);
});

// --- the checkpoint --------------------------------------------------------------------------

test('a discarded pass leaves the running-head memory untouched', () => {
  // The re-split throws away everything the first read decided. Without this the discarded read
  // leaves its candidate lines behind under a different page token, and the read that replaces
  // it counts them a second time and drops a heading seen only once.
  const heading = line('A CHANGING COASTLINE', 40);
  const undo = furnitureCheckpoint();
  expect(furnitureReason(heading, on('trial'))).toBeNull();
  undo();
  expect(furnitureReason(heading, on('real'))).toBeNull();
  expect(furnitureReason(heading, on('next-page'))).toBe('repeats');
});

test('a re-render read for boxes only must not count as another page', () => {
  // A resize repaginates, so lines move between columns and the column fingerprint changes. If
  // that read ran the furniture rule, the same heading would look like it had appeared on a
  // second page - and the next real read of the page would withhold it. `followReflow` passes
  // `trial`, and this is the memory behaviour that depends on: the reflow read never gets here.
  const heading = line('A CHANGING COASTLINE', 40);
  expect(furnitureReason(heading, on('render-a'))).toBeNull();
  // ...the reflow read is skipped entirely rather than counted under 'render-b'...
  expect(furnitureReason(heading, on('render-a'))).toBeNull(); // the page, read again
  expect(furnitureReason(heading, on('render-b'))).toBe('repeats'); // and this is what a real second page does
});
