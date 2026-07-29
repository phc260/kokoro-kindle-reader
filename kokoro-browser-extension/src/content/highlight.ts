// The current word, marked on the page.
//
// The reader renders the book to a bitmap (see capture.ts), so there is no text node to wrap and
// no selection to set - the word being spoken exists on screen only as pixels. What there IS is
// the OCR word list: every word carries a bbox in ORIGINAL IMAGE pixels, which is the whole
// reason ocr.ts adds the column offset back and does its inversion without touching geometry.
//
// So the mark is an absolutely-positioned box floated over the page image, and the chain is:
//
//   charIndex (from the narrator) -> OcrWord -> bbox -> image scale + live rect -> screen
//
// Two rules keep it honest:
//
//   1. Never draw on a page we did not OCR. The bbox list belongs to one specific render, and
//      the reader replaces the whole bitmap on a page turn. The captured blob: URL is the
//      identity, so a mismatch hides the mark rather than painting last page's boxes on this
//      one.
//   2. Read the image's rect at draw time, never cache it. Window resizes, zoom changes and the
//      reader's own relayout all move it, and a stale rect puts the mark in the margin.
//
// It lives in its own shadow root for the same reason the panel does: Amazon's CSS cannot reach
// in, ours cannot leak out. `pointer-events: none` throughout - clicking the page has to keep
// turning it.

import { findPageImage } from './capture';
import type { OcrWord } from './ocr';

/** The page a set of word boxes was measured on. */
export interface HighlightPage {
  /** The blob: URL captured from. Identity: the mark only draws while this page is displayed. */
  src: string;
  /** Natural size of that capture, which is the coordinate space the bboxes are in. */
  natural: { w: number; h: number };
  words: OcrWord[];
  /** The page was light-on-dark, so the mark has to lighten rather than darken. */
  inverted: boolean;
}

export interface HighlightHandle {
  /** Point at a new page's words, or null to go dormant. Clears any visible mark. */
  page(p: HighlightPage | null): void;
  /**
   * More words for the page already set - the second column, recognized while the first was
   * being read. Their `charStart`s must already be shifted to the whole page, and continue after
   * the words that are there, or the binary search in `wordIndexAt` stops being valid.
   */
  extend(words: OcrWord[]): void;
  /**
   * A fresh OCR of the SAME passage after the reader re-rendered it - a resize or a zoom.
   *
   * The narrator keeps reporting offsets against the page given to `page()`, because that is the
   * text being spoken; only the boxes have moved. Each word is relocated in the new render by
   * matching its neighbours. Pass null to go back to drawing on the original.
   */
  reflow(p: HighlightPage | null): void;
  /**
   * Move the mark to whatever word covers `charIndex` in that page's OCR text.
   *
   * Deliberately takes no length: a boundary's span can cross a line break (cleanup() joins
   * hyphenated words, so one spoken word is two boxes on two lines) and one rectangle cannot
   * cover that. Marking the word the offset lands in is right in every case and cheap.
   */
  at(charIndex: number): void;
  clear(): void;
  destroy(): void;
}

const HOST_ID = 'kokoro-kindle-cloud-reader-highlight';

/**
 * Grow the box by this fraction of its height on each side. Tesseract's boxes are tight to the
 * ink, so an unpadded mark clips ascenders and looks like a mistake rather than a highlight.
 */
const PAD = 0.16;

/**
 * Slide to the next word only when it is on the same line - within this fraction of a line
 * height. Sliding looks deliberate along a line and looks broken travelling back across the page
 * to start the next one, so a line change is a cut.
 */
const SAME_LINE = 0.6;

const CSS = `
:host { all: initial; }
.mark {
  position: fixed; pointer-events: none; border-radius: 3px;
  opacity: 0; visibility: hidden;
  /* Multiply darkens what is under it and leaves the glyphs legible, which a flat overlay at any
     opacity does not - the point is to look like a marker pen over the text, not a pane in
     front of it. */
  background: rgba(255, 206, 0, .55); mix-blend-mode: multiply;
}
/* An inverted (light-on-dark) page needs the opposite: multiply over near-black does nothing
   visible at all. Screen lightens, so the mark reads as a glow behind pale text. */
.mark.on-dark { background: rgba(70, 110, 190, .62); mix-blend-mode: screen; }
.mark.on { opacity: 1; visibility: visible; }
.mark.slide { transition: left .09s linear, width .09s linear; }
`;

/**
 * Index of the word covering `charIndex`, or of the nearest one. -1 only for an empty page.
 *
 * Nearest rather than strictly containing, because a boundary can land in the gap between two
 * words: `chrome.tts` reports the offset of a leading space on some voices, and the Kokoro marks
 * are an estimate to begin with. Silently drawing nothing there would read as the highlight
 * dying mid-sentence.
 *
 * `words` is in reading order and its `charStart`s are non-decreasing (ocr.ts walks the kept
 * lines with a single cursor), so a binary search is valid.
 */
export function wordIndexAt(words: OcrWord[], charIndex: number): number {
  if (!words.length) return -1;

  let lo = 0;
  let hi = words.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (words[mid]!.charStart <= charIndex) {
      found = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }

  const before = found >= 0 ? words[found]! : null;
  if (!before) return 0;
  if (charIndex <= before.charStart + before.charLen) return found;
  const after = words[found + 1];
  if (!after) return found;
  // In the gap: whichever edge is closer.
  return charIndex - (before.charStart + before.charLen) <= after.charStart - charIndex ? found : found + 1;
}

/** The word covering `charIndex`, or the nearest one. */
export function wordAt(words: OcrWord[], charIndex: number): OcrWord | null {
  const i = wordIndexAt(words, charIndex);
  return i >= 0 ? words[i]! : null;
}

/** How two OCRs of the same words are compared. Case and punctuation are not evidence. */
const key = (w: OcrWord | undefined): string => (w ? w.text.toLowerCase().replace(/[^a-z0-9]+/g, '') : '');

/**
 * The same word as `from[k]`, found in a DIFFERENT OCR of the passage.
 *
 * Needed because the reader re-renders on every resize and zoom: the text reflows, so the boxes
 * move and the character offsets the narrator is reporting no longer address them. Reading order
 * survives a reflow even though the line breaks do not, so a word plus its immediate neighbours
 * is enough to find it again.
 *
 * Returns null rather than a guess in the two cases that matter: the word is not on the new
 * render at all (the reflow pushed it onto the next page), and the match is ambiguous. A mark in
 * the wrong place is worse than no mark - it makes the reader distrust the ones that are right.
 */
export function relocate(from: OcrWord[], k: number, to: OcrWord[]): OcrWord | null {
  const want = key(from[k]);
  if (!want) return null;
  const before = key(from[k - 1]);
  const after = key(from[k + 1]);

  let hit: OcrWord | null = null;
  for (let i = 0; i < to.length; i++) {
    if (key(to[i]) !== want) continue;
    // Context is checked only where both sides have it. At the head or foot of a render one
    // neighbour is missing, and insisting on it there would refuse every first and last word -
    // which are exactly the ones a reflow is most likely to move.
    const prev = key(to[i - 1]);
    const next = key(to[i + 1]);
    if (before && prev && prev !== before) continue;
    if (after && next && next !== after) continue;
    if (hit) return null; // two places on the page look alike; draw neither
    hit = to[i]!;
  }
  return hit;
}

/** A viewport-space box, ready to write onto the mark's style. */
export interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * Where a word's box lands on screen.
 *
 * `bbox` is in the captured image's own pixels; `rect` is where that image is right now, in
 * viewport coordinates. Everything the mapping needs is in those two - notably NOT the scroll
 * offset or the device pixel ratio: `getBoundingClientRect` has already applied both, and adding
 * either again is the classic way to end up with a mark that is right at the top of the page and
 * drifting further off with every line.
 *
 * Null when the image has no size to scale against, which happens mid-swap.
 */
export function markRect(bbox: OcrWord['bbox'], rect: Rect, natural: { w: number; h: number }): Rect | null {
  if (!rect.width || !rect.height || !natural.w || !natural.h) return null;

  const sx = rect.width / natural.w;
  const sy = rect.height / natural.h;
  const height = (bbox.y1 - bbox.y0) * sy;
  const pad = height * PAD;

  return {
    left: rect.left + bbox.x0 * sx - pad,
    top: rect.top + bbox.y0 * sy - pad,
    width: (bbox.x1 - bbox.x0) * sx + pad * 2,
    height: height + pad * 2,
  };
}

export function mountHighlight(): HighlightHandle {
  document.getElementById(HOST_ID)?.remove(); // never mount twice

  const host = document.createElement('div');
  host.id = HOST_ID;
  const root = host.attachShadow({ mode: 'open' });
  const style = document.createElement('style');
  style.textContent = CSS;
  const mark = document.createElement('div');
  mark.className = 'mark';
  root.append(style, mark);
  document.documentElement.append(host);

  /** The OCR the narrator's offsets are measured against. Never replaced mid-page. */
  let ref: HighlightPage | null = null;
  /** The render actually on screen. Same object as `ref` until the reader re-renders. */
  let live: HighlightPage | null = null;
  /** The bbox currently shown, so scroll and resize can redraw it without a new boundary. */
  let box: OcrWord['bbox'] | null = null;
  /** The last offset reported, so a re-render can re-resolve it without waiting for the next. */
  let cursor = -1;
  /**
   * The page's <img>, held across draws.
   *
   * `findPageImage` walks every element in the document, piercing ~37 shadow roots, so it is far
   * too heavy to run per draw - and a draw happens on every word and every scroll event. The
   * element is stable for the life of a page, and re-validating the one we have is two property
   * reads, so the walk only runs when that check fails.
   */
  let held: HTMLImageElement | null = null;

  const hide = () => {
    mark.classList.remove('on');
  };

  /** The <img> currently showing `page`, or null if the reader has moved on. */
  const pageImage = (src: string): HTMLImageElement | null => {
    const ok = (el: HTMLImageElement | null) => !!el && el.isConnected && (el.currentSrc || el.src) === src;
    if (!ok(held)) held = findPageImage();
    return ok(held) ? held : null;
  };

  const draw = () => {
    if (!live || !box) return hide();

    // Rule 1: the page image on screen has to be the one these boxes were measured on.
    const img = pageImage(live.src);
    if (!img) return hide();

    // Rule 2: live rect, every time.
    const at = markRect(box, img.getBoundingClientRect(), live.natural);
    if (!at) return hide();

    const moved = Math.abs(at.top - parseFloat(mark.style.top || 'NaN'));
    mark.classList.toggle('slide', mark.classList.contains('on') && moved < at.height * SAME_LINE);
    mark.classList.toggle('on-dark', live.inverted);

    mark.style.top = `${at.top}px`;
    mark.style.left = `${at.left}px`;
    mark.style.width = `${at.width}px`;
    mark.style.height = `${at.height}px`;
    mark.classList.add('on');
  };

  /** Resolve an offset to a box on whatever render is currently on screen, and draw it. */
  const resolve = (charIndex: number) => {
    cursor = charIndex;
    if (!ref || !live) return hide();

    const k = wordIndexAt(ref.words, charIndex);
    if (k < 0) {
      box = null;
      return hide();
    }
    // Straight through while the page we OCR'd is the page on screen, which is the whole session
    // unless the window was resized mid-page.
    const word = live === ref ? ref.words[k]! : relocate(ref.words, k, live.words);
    box = word?.bbox ?? null;
    draw();
  };

  // Scroll and resize move the image without producing a word boundary, so the mark has to
  // follow them itself or it detaches from the text for as long as the current word lasts.
  // Coalesced onto a frame: scroll fires far faster than the screen updates, and every draw
  // reads a layout-forcing rect.
  let frame = 0;
  const onView = () => {
    if (frame || !box) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      draw();
    });
  };
  // Capture, because the reader scrolls a container of its own rather than the document.
  addEventListener('scroll', onView, { passive: true, capture: true });
  addEventListener('resize', onView, { passive: true });

  return {
    page(p) {
      ref = p;
      live = p;
      box = null;
      held = null;
      cursor = -1;
      hide();
    },
    extend(words) {
      if (!ref || !words.length) return;
      ref.words = ref.words.concat(words);
      // `live` is the same object until a re-render; when it is not, the reflow's own OCR already
      // covered the whole page, so only the reference list grows.
      if (cursor >= 0) resolve(cursor);
    },
    reflow(p) {
      if (!ref) return;
      live = p ?? ref;
      held = null;
      // Re-resolve straight away rather than waiting for the next boundary: at a slow speed that
      // is most of a second of the mark being missing right after the resize that caused it,
      // which reads as the resize having broken it.
      if (cursor >= 0) resolve(cursor);
    },
    at(charIndex) {
      resolve(charIndex);
    },
    clear() {
      box = null;
      hide();
    },
    destroy() {
      if (frame) cancelAnimationFrame(frame);
      removeEventListener('scroll', onView, { capture: true });
      removeEventListener('resize', onView);
      host.remove();
    },
  };
}
