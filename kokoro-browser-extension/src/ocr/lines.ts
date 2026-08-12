// What a line's GEOMETRY says about it - shared by the two rules that need to know.
//
// Both of this module's consumers ask the same question from opposite ends. `looksInterleaved`
// (used by `../ocr` to decide a page was read across a missed gutter) and `setAsBody` (used by
// `furniture.ts` to decide a line is body text) both come down to: is this line set to the
// column's measure, and are its word gaps even? A running head spanning the full width with a
// folio at the right, and two columns the detector found as one region, have the SAME signature -
// one enormous gap where justification would have stretched every gap together. Splitting them
// apart would mean two copies of that measurement drifting.
//
// Nothing here decides anything on its own, and nothing here removes text. It measures.

import type { RawWord } from './backend';

/** A line reaching this fraction of the column's measure is set as body text, not as a head. */
const FULL_MEASURE = 0.8;
/** How much wider than its own median a justified line's largest word gap may be. */
const SPACING_EVEN = 3;

function lineWidth(line: RawWord[]): number {
  let x0 = Infinity;
  let x1 = -Infinity;
  for (const w of line) {
    if (w.bbox.x0 < x0) x0 = w.bbox.x0;
    if (w.bbox.x1 > x1) x1 = w.bbox.x1;
  }
  return x1 > x0 ? x1 - x0 : 0;
}

/**
 * How wide a full line of body text is in this column.
 *
 * The 90th percentile rather than the maximum, so one over-wide line - a stray mark picked up at
 * the margin, two lines merged - cannot set the bar too high and disqualify every real one.
 */
export function measureOf(lines: RawWord[][]): number {
  const widths = lines
    .map(lineWidth)
    .filter((w) => w > 0)
    .sort((a, b) => a - b);
  if (!widths.length) return 0;
  return widths[Math.min(widths.length - 1, Math.floor(widths.length * 0.9))]!;
}

/**
 * Does one gap in this line dwarf the rest of them?
 *
 * Justification stretches the space between every pair of words together, so a line set as body
 * text has no outlier. One huge gap means two groups of words that are not a phrase: a
 * title-left/folio-right header, or - the reason this is shared with `looksInterleaved` - two
 * COLUMNS the detector found as one region and read across as though they were one line.
 */
function hasOutlierGap(line: RawWord[]): boolean {
  if (line.length < 3) return false; // too few gaps to tell one apart from the rest

  const gaps: number[] = [];
  for (let i = 1; i < line.length; i++) gaps.push(line[i]!.bbox.x0 - line[i - 1]!.bbox.x1);
  gaps.sort((a, b) => a - b);
  // The LOWER middle on an even count. With two gaps - a title, a folio, and one stray mark -
  // the upper middle is the huge gap itself, which then measures as typical and the header
  // passes for justified text.
  const median = gaps[(gaps.length - 1) >> 1]!;
  const widest = gaps[gaps.length - 1]!;

  // Floored at the type size rather than a pixel count, because a median of ~0 is real: a word
  // gets split now and then, and the halves sit touching. Without a floor those lines could
  // never pass; with a fixed one the threshold means something different at every font size.
  const height = Math.max(...line.map((w) => w.bbox.y1 - w.bbox.y0));
  return widest > Math.max(median * SPACING_EVEN, height * 1.5);
}

/**
 * Is this line set to the column's measure, the way justified body text is?
 *
 * This is the signal that survives a font change, and the reason `furniture.ts` needs it: the word
 * count and the sentence-end escape there both fail on the SAME line at a larger font. "To tend
 * the lamp each night, to" is seven words, sits at the top of the page, and ends mid-clause -
 * it looks exactly like a running head to every test except this one. Enlarge the type and most of
 * the book's lines become short enough to look that way.
 *
 * Two things have to hold, and the second matters as much as the first. A running head can span
 * the full width too - title at the left, folio at the right - but it does that with one large
 * gap in the middle, where justification stretches the space between every pair of words
 * together. So: full measure, AND no gap far wider than the rest of them.
 */
export function setAsBody(line: RawWord[], measure: number): boolean {
  if (measure <= 0 || lineWidth(line) < measure * FULL_MEASURE) return false;
  if (line.length < 3) return false;
  return !hasOutlierGap(line);
}

/** How many full-measure lines must show the two-group signature before a page is re-split. */
const INTERLEAVED_FRAC = 0.4;

/**
 * Was this page read ACROSS a gutter that was missed?
 *
 * `findGutter` needs a band free of ink over almost the whole page height, so one figure, rule or
 * full-width heading crossing the gutter is enough to hide it - and then the two columns are read
 * line by line into sentences that are fluent and wrong. Nothing downstream can notice:
 * the words are all real and the confidence is high.
 *
 * What it leaves behind is every full-width line having one enormous gap in the middle where the
 * gutter is, which justified text never has. Enough lines like that and the page gets a second
 * look with the gutter test relaxed.
 */
export function looksInterleaved(lines: RawWord[][]): boolean {
  const measure = measureOf(lines);
  if (measure <= 0) return false;

  const wide = lines.filter((l) => l.length >= 3 && lineWidth(l) >= measure * FULL_MEASURE);
  if (wide.length < 3) return false; // too little of a page to conclude anything from

  return wide.filter(hasOutlierGap).length >= wide.length * INTERLEAVED_FRAC;
}
