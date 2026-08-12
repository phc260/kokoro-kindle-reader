// The only rule in the pipeline that decides a line of the book will not be read.
//
// A RULE THAT CAN SILENTLY REMOVE TEXT MUST ACT ON EVIDENCE, NOT ON APPEARANCE. That is the
// governing rule of this file and it was earned: four content losses, every one from a threshold
// that encoded what a page was assumed to look like. Appearance may still decide what is a
// CANDIDATE; only evidence may act, and evidence comes in exactly two shapes here - text no book
// has in its body (`FURNITURE_PATTERN`), or the same thing seen on ANOTHER page
// (`repeatsAcrossPages`, for running heads and for folio *slots*, whose text changes every page).
//
// It is invisible in both directions and that is what makes it dangerous. Furniture OCRs
// *perfectly*, so no confidence or accuracy check can point at a mistake: keep a running head and
// it is spoken between every page; drop a heading and the book quietly loses part of itself. The
// caller logs every drop with its reason (`[kwr] not narrated: ...`) because that log is the only
// evidence that exists when a line goes missing.
//
// It also carries STATE ACROSS PAGES (`bandSeen`), which is why `furnitureCheckpoint` exists and
// why a read whose text is not narrated must not reach this file at all.

import type { RawWord } from './backend';
import { setAsBody } from './lines';

/** Fraction of page height at top/bottom where running heads and folios live. */
const FURNITURE_BAND = 0.1;
/** A furniture line is short; a body line that happens to sit high on the page is not. */
const FURNITURE_MAX_WORDS = 8;

/**
 * The only text removed for what it SAYS rather than for having been seen before.
 *
 * Deliberately tiny. Every branch that used to live here was a guess about what a page looks
 * like - a bare number, a year followed by a capital - and each could match a line of the book:
 * a list item that is only "42", a sentence opening "1997 Grace wrote". They have moved to
 * evidence (see `FOLIO_SHAPE` and `repeatsAcrossPages`); what is left is text that no book has
 * in its body. A copyright notice in particular ends in a full stop ("All rights reserved."),
 * which defeats any punctuation-based test, and it is worth catching on sight because it sits at
 * the end of a chapter where a repeat may never come.
 */
const FURNITURE_PATTERN = /^\s*(©|\(c\)|copyright\b)|all rights reserved/iu;

/**
 * A line that is nothing but a number, optionally bracketed or prefixed - the SHAPE of a folio.
 *
 * A shape is not evidence on its own, and this one is never acted on alone: a folio's text
 * changes every page, so it can never repeat its way out of the narration, but its shape and its
 * position do repeat. `furnitureReason` therefore remembers the SLOT a numeric line appeared in
 * and drops one only once that slot has held one on another page too.
 */
const FOLIO_SHAPE = /^\s*(page\s+)?[[(]?\d{1,4}[\])]?\s*$/i;
/** How finely a folio's horizontal position is bucketed when identifying its slot. */
const SLOT_BUCKETS = 10;

/**
 * Short lines seen in a band, and how many DISTINCT pages each has turned up on.
 *
 * Repetition is the ONLY reliable evidence of a running head. Everything else a running head has
 * - short, near the page edge, no closing punctuation - a section heading has too, and a
 * two-column page puts headings at the top of the right column on nearly every page. Three
 * different real headings were lost to that guess before this became a count.
 *
 * `pages` is a count of pages, not of sightings, which is why each entry remembers the last page
 * token it was counted for: the same page gets recognized more than once as a matter of course
 * (a re-render while narrating, `kwr.readPage()` before pressing Play), and counting those would
 * condemn a heading on the second look at the page it belongs to.
 *
 * Bounded so a long session cannot grow it without limit.
 */
const bandSeen = new Map<string, { pages: number; token: string }>();
const BAND_MEMORY = 400;

function normalizeLine(text: string): string {
  return text.toLowerCase().replace(/[^a-z0-9]+/g, ' ').trim();
}

/**
 * A fingerprint of what is on this column, for telling "the same words on another page" from
 * "the same page, read again".
 *
 * Letters and digits only: a re-render reflows the text onto different lines and rehyphenates it,
 * so anything that counts whitespace, line structure or hyphens would call the same page new.
 */
export function pageToken(lines: RawWord[][]): string {
  let h = 2166136261;
  for (const line of lines) {
    for (const w of line) {
      for (const ch of w.text.toLowerCase()) {
        if (!(ch >= 'a' && ch <= 'z') && !(ch >= '0' && ch <= '9')) continue;
        h = Math.imul(h ^ ch.charCodeAt(0), 16777619);
      }
    }
  }
  return (h >>> 0).toString(36);
}

/** Forget the running-head history - call when switching books. */
export function resetFurnitureMemory(): void {
  bandSeen.clear();
}

/**
 * Snapshot the running-head memory; the returned function puts it back.
 *
 * For a pass whose result might be thrown away - a page recognized as one column that turns out
 * to have been two. Without it the discarded pass leaves its candidate lines behind under a
 * different page token, and the pass that replaces it counts them a second time and drops a
 * heading that has only ever appeared once. That exact shape has already cost a line of the book
 * once; it does not get to happen again through a different door.
 */
export function furnitureCheckpoint(): () => void {
  const snapshot = [...bandSeen].map(([k, v]) => [k, { ...v }] as const);
  return () => {
    bandSeen.clear();
    for (const [k, v] of snapshot) bandSeen.set(k, v);
  };
}

/** What this column is, for the furniture rule. */
export interface PageContext {
  /** Page height in pixels, for the top/bottom band. */
  height: number;
  /** Column width in pixels, for locating a folio's slot across pages. */
  width: number;
  /** Width of a full line of body text in this column; 0 when it cannot be told. */
  measure: number;
  /** `pageToken` of this column, so a page read twice is not two pages. */
  token: string;
}

/**
 * Has this line appeared in a band on a page OTHER than this one? Records it either way.
 */
function repeatsAcrossPages(key: string, token: string): boolean {
  const seen = bandSeen.get(key);
  if (!seen) {
    if (bandSeen.size > BAND_MEMORY) bandSeen.clear();
    bandSeen.set(key, { pages: 1, token });
    return false;
  }
  if (seen.token !== token) {
    seen.pages++;
    seen.token = token;
  }
  return seen.pages > 1;
}

/**
 * Why this line is furniture rather than body text, or null to keep it.
 *
 * Furniture OCRs *perfectly* and is worse for it: a running head or copyright notice spoken
 * between every page is what makes narration unusable, and no accuracy check can see it because
 * the characters are correct.
 *
 * Exported for `test/furniture.test.ts`. This is the one place in the pipeline that decides a
 * line of the book will not be read, it carries state across pages, and both of those are
 * invisible from the outside - so it is worth reaching in to test directly rather than through
 * a whole recognition pass.
 */
export function furnitureReason(line: RawWord[], page: PageContext): string | null {
  if (line.length > FURNITURE_MAX_WORDS) return null;

  const text = line
    .map((w) => w.text.trim())
    .join(' ')
    .trim();
  if (!text) return 'empty';

  // Unambiguous regardless of position - a copyright line can sit well above the bottom edge.
  if (FURNITURE_PATTERN.test(text)) return 'pattern';

  const top = Math.min(...line.map((w) => w.bbox.y0));
  const bottom = Math.max(...line.map((w) => w.bbox.y1));
  const mid = (top + bottom) / 2;
  const atTop = mid < page.height * FURNITURE_BAND;
  const inBand = atTop || mid > page.height * (1 - FURNITURE_BAND);
  if (!inBand) return null;

  // A folio never repeats its text - the number changes every page - so it is remembered by
  // WHERE it sits instead. A numeric line in the same corner on a second page is a page number;
  // one on its own is a numbered item, and gets read.
  if (FOLIO_SHAPE.test(text)) {
    const centre = (Math.min(...line.map((w) => w.bbox.x0)) + Math.max(...line.map((w) => w.bbox.x1))) / 2;
    const bucket = page.width > 0 ? Math.round((centre / page.width) * SLOT_BUCKETS) : 0;
    return repeatsAcrossPages(`folio:${atTop ? 't' : 'b'}${bucket}`, page.token) ? 'folio' : null;
  }

  // Set to the column's measure: body text, whatever else it looks like.
  if (setAsBody(line, page.measure)) return null;

  // A line in the band that ends mid-sentence is body text that happens to sit near the edge.
  // Catches what the measure test cannot: the LAST line of a paragraph is short by nature, so a
  // paragraph ending in the band ("the responsibility the keeper should bear.") reaches nothing
  // like the full measure and is body text all the same.
  if (/[.!?]["'”’)]?$/.test(text)) return null;

  // What is left is a short line near the page edge that could be a running head - or could be a
  // section heading, which looks identical. REPETITION is the only thing that separates them, so
  // nothing is dropped on a first sighting: "A CHANGING COASTLINE" at the top of a right column
  // is content and gets read, and "FIELD-GUIDE TO HARBOURS" is read once and then never again.
  //
  // Guessing here instead cost three real headings on one page, and every loss was silent.
  // Speaking a running head once at the start of a session is the cheaper mistake by far: it is
  // audible, it is over, and it does not remove any of the book.
  const key = normalizeLine(text);
  if (key && repeatsAcrossPages(key, page.token)) return 'repeats';
  return null;
}
