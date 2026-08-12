// Page image -> text + word boxes. The pipeline, and the public face of `src/ocr/`.
//
// WHERE THIS RUNS: the OFFSCREEN DOCUMENT, not the content script. It lived under `content/`
// until recognition moved to the host, which is when the two parted company - a content script's
// `fetch` carries Amazon's origin and the host allowlists `chrome-extension://<id>` alone, so the
// call has to be made from an extension-origin context. The difference is not cosmetic:
// `furniture.ts` keeps module state, so a copy of this module in another context has a furniture
// memory of its own that no page ever writes to. The content script does still bundle it, for
// `kwr.ocr` - which `test/ocr-bench.ts` drives with a backend of its own, against pages this repo
// renders. That copy is the bench's; the reader's is here.
//
// WHAT THIS DIRECTORY IS. Recognition itself is now one authenticated fetch, so what remains is
// everything the engine never did - and those parts had been living inside the engine's file:
//
//   backend.ts    `POST /ocr` and the failures it can return. The whole transport.
//   layout.ts     page pixels -> one canvas per column: dark-page test, gutter, split.
//   lines.ts      what a line's geometry says: its measure, and whether its gaps are even.
//   furniture.ts  the only rule that decides a line will not be read. Evidence, never appearance.
//   index.ts      this: assemble those into a page, in reading order, with offsets that hold.
//
// The split is the engine move finishing. One file made sense while OCR *was* one thing here; it
// stopped making sense when the thing it was named after left, and four concerns that answer to
// different tests were left sharing a header.
//
// Word boxes come out in ORIGINAL image pixel coordinates - the column split's x offset is added
// back here, exactly once - so `highlight.ts` can map them to the screen with only the CSS/natural
// scale and the live devicePixelRatio.

import { preprocess, type Prepared } from './layout';
import { recognizeViaHost, type OcrBackend, type RawWord } from './backend';
import { looksInterleaved, measureOf } from './lines';
import { furnitureCheckpoint, furnitureReason, pageToken, type PageContext } from './furniture';

// Re-exported deliberately narrowly: what a CALLER outside this directory needs, and no more.
// `lines.ts` and `furniture.ts` are reached by their own paths (see `test/furniture.test.ts`),
// which is what keeps `resetFurnitureMemory` off `kwr.ocr` — the content script's copy of that
// memory is never written to by any page, so a console route to it would only ever mislead.
export { preprocess, type Prepared, type PreprocessOptions } from './layout';
export { type OcrBackend } from './backend';

export interface OcrWord {
  text: string;
  /** 0-100. The mean CTC probability of the characters in the word, times 100. */
  confidence: number;
  /** Original-image pixel coordinates. */
  bbox: { x0: number; y0: number; x1: number; y1: number };
  /** Character offset of this word within the returned `text`. */
  charStart: number;
  charLen: number;
}

export interface OcrResult {
  text: string;
  words: OcrWord[];
  columns: number;
  inverted: boolean;
  /** Running heads / folios removed. They OCR perfectly and must still not be narrated. */
  furnitureDropped: number;
  /** What was dropped, and why - dropping body text by mistake must be visible, not silent. */
  furniture: { text: string; reason: string }[];
  meanConfidence: number;
  timing: { preprocessMs: number; recognizeMs: number; totalMs: number };
}

/** One column's worth of a page. Word boxes are in PAGE coordinates; offsets are column-local. */
export interface ColumnOcr {
  text: string;
  words: OcrWord[];
  /** How many columns the page has - known as soon as any one of them is recognized. */
  columns: number;
  inverted: boolean;
  furniture: { text: string; reason: string }[];
  meanConfidence: number;
  /**
   * This was read as one column and the lines say it was two. The caller should throw the result
   * away and prepare again with `columns: 'split'` - see `looksInterleaved`.
   */
  suspectSplit: boolean;
  timing: { preprocessMs: number; recognizeMs: number; totalMs: number };
}

/**
 * OCR ONE column of an already-preprocessed page.
 *
 * Separately callable because a two-column page's first column is enough to start narrating: the
 * caller can speak it while this runs again for the second, which is most of a second off the
 * wait for the first word. `recognize` below is the same thing over every column at once.
 */
export interface RecognizeOptions {
  /**
   * Skip furniture removal entirely, and leave the running-head memory untouched.
   *
   * For a read whose TEXT is not going to be narrated - the re-OCR of a re-rendered page, which
   * exists only to re-locate word boxes. Two reasons it must not run the furniture rule:
   *
   *   1. Its page token is not the token of the read being narrated. A resize repaginates, so
   *      lines move between columns and the fingerprint changes; the same heading then counts as
   *      having been seen on a second page and is dropped from the next real read of that page.
   *   2. A heading dropped from the reflow read has no boxes in it, so the highlight has nothing
   *      to relocate onto and goes dark for as long as that heading is being spoken.
   */
  trial?: boolean;
  /**
   * Abandon the request. Stop presses this, and so does a page that has been superseded.
   *
   * It reaches the host as a closed socket, which is what lets it cancel the work rather than
   * merely stop listening to it. ONNX Runtime cannot abandon a run in progress, so the host
   * stops between lines and a job already inside one finishes there and has its result thrown
   * away - safe precisely because the cross-page furniture memory is here, not in the host.
   */
  signal?: AbortSignal;
}

export async function recognizeColumn(
  prepared: Prepared,
  index: number,
  backend: OcrBackend,
  options: RecognizeOptions = {},
): Promise<ColumnOcr> {
  const t1 = performance.now();
  const canvas = prepared.columns[index];
  if (!canvas) throw new Error(`no column ${index} on a ${prepared.columns.length}-column page`);
  const dx = prepared.offsets[index]!;

  const blob = await canvas.convertToBlob({ type: 'image/png' });
  const lines = await recognizeViaHost(blob, backend, options.signal);

  // Text is assembled from the retained lines rather than from a whole-page string, because
  // furniture has to be dropped from BOTH the text and the word list or highlighting and
  // narration disagree about what is on the page.
  const kept: RawWord[][] = [];
  const furniture: { text: string; reason: string }[] = [];
  let confSum = 0;
  let confN = 0;

  // Per column, not per page: two columns each have their own measure, and a page-wide figure
  // would be the wider of them and disqualify every line of the narrower.
  const context: PageContext = {
    height: canvas.height,
    width: canvas.width,
    measure: measureOf(lines),
    // Taken over EVERY line, before any is dropped, so the fingerprint of a page does not depend
    // on decisions this same call is about to make.
    token: pageToken(lines),
  };
  // Only a page believed to be single-column can have had its gutter missed.
  const suspectSplit = prepared.columns.length === 1 && looksInterleaved(lines);

  for (const line of lines) {
    const reason = options.trial ? null : furnitureReason(line, context);
    if (reason) {
      furniture.push({ text: line.map((w) => w.text.trim()).join(' '), reason });
      continue;
    }
    kept.push(line);
    for (const w of line) {
      confSum += w.confidence;
      confN++;
    }
  }

  const text = cleanup(kept.map((line) => line.map((w) => w.text.trim()).join(' ')).join('\n'));

  // cleanup() joins hyphenated words and unwraps lines, so offsets must be re-derived against
  // the final string. A word that cleanup merged away (the head of a hyphen split) simply keeps
  // the cursor position - it still points into the right region for highlighting.
  const words: OcrWord[] = [];
  let cursor = 0;
  for (const line of kept) {
    for (const word of line) {
      const t = word.text.trim();
      const at = text.indexOf(t, cursor);
      if (at >= 0) cursor = at + t.length;
      words.push({
        text: t,
        confidence: word.confidence,
        // Page coordinates, so the column split is invisible to the highlight.
        bbox: { x0: word.bbox.x0 + dx, y0: word.bbox.y0, x1: word.bbox.x1 + dx, y1: word.bbox.y1 },
        charStart: at >= 0 ? at : cursor,
        charLen: t.length,
      });
    }
  }

  const t2 = performance.now();
  return {
    text,
    words,
    columns: prepared.columns.length,
    inverted: prepared.inverted,
    furniture,
    meanConfidence: confN ? confSum / confN : 0,
    suspectSplit,
    timing: { preprocessMs: 0, recognizeMs: t2 - t1, totalMs: t2 - t1 },
  };
}

/**
 * Recognize a column, and give the page a second look if it turns out to have been two columns
 * read as one.
 *
 * The retry costs an OCR pass, and only on a page that has already been shown to be wrong - a
 * missed gutter, which the strict test cannot see past on its own. The checkpoint is what makes
 * the discarded pass free of consequences: without it the thrown-away read leaves its candidate
 * lines in the running-head memory under a second page token, and the read that replaces it
 * drops a heading that has only ever been seen once.
 */
export async function recognizeColumnChecked(
  source: Blob | ImageBitmap,
  prepared: Prepared,
  index: number,
  backend: OcrBackend,
  options: RecognizeOptions = {},
): Promise<{ result: ColumnOcr; prepared: Prepared }> {
  const undo = furnitureCheckpoint();
  const result = await recognizeColumn(prepared, index, backend, options);
  if (!result.suspectSplit) return { result, prepared };

  console.warn('[kwr] page read across a missed gutter - splitting and reading it again');
  const split = await preprocess(source, { columns: 'split' });
  // The relaxed search can still find nothing; keep the first read rather than losing the page -
  // and keep what it learned with it. Rolling back here would discard the first sighting of this
  // page's running head from a read that IS the one being narrated, so the next page would see
  // that head as new and read it a second time.
  if (split.columns.length < 2) return { result, prepared };

  // Only now is the first read genuinely thrown away.
  undo();
  return { result: await recognizeColumn(split, index, backend, options), prepared: split };
}

/**
 * Stitch recognized columns into one page, in reading order - left column fully, then right,
 * which is the whole point of the split.
 *
 * Joined with a single '\n', and every word's offset shifted by where its column starts. The
 * caller streaming columns to the narrator has to use the SAME join, or the offsets a boundary
 * reports and the offsets the words carry stop addressing the same string.
 */
export function joinColumns(columns: ColumnOcr[]): { text: string; words: OcrWord[]; bases: number[] } {
  const bases: number[] = [];
  let base = 0;
  const words: OcrWord[] = [];

  for (const col of columns) {
    bases.push(base);
    for (const w of col.words) words.push({ ...w, charStart: w.charStart + base });
    base += col.text.length + 1; // the '\n' the join below inserts
  }

  return { text: columns.map((c) => c.text).join('\n'), words, bases };
}

/**
 * OCR one captured page, every column of it.
 *
 * The console path (`kwr.readPage()`) and the bench. The narration path drives `recognizeColumn`
 * itself so it can start speaking on the first column.
 */
export async function recognize(
  source: Blob | ImageBitmap,
  backend: OcrBackend,
  options: RecognizeOptions = {},
): Promise<OcrResult> {
  const t0 = performance.now();
  let prepared = await preprocess(source);
  const t1 = performance.now();

  // The first column decides whether the page was cut correctly; the rest follow whatever it
  // settled on.
  const first = await recognizeColumnChecked(source, prepared, 0, backend, options);
  prepared = first.prepared;
  const columns: ColumnOcr[] = [first.result];
  for (let i = 1; i < prepared.columns.length; i++)
    columns.push(await recognizeColumn(prepared, i, backend, options));

  const { text, words } = joinColumns(columns);
  const furniture = columns.flatMap((c) => c.furniture);
  const weighted = columns.reduce((n, c) => n + c.meanConfidence * c.words.length, 0);
  const counted = columns.reduce((n, c) => n + c.words.length, 0);

  const t2 = performance.now();
  return {
    text,
    words,
    columns: prepared.columns.length,
    inverted: prepared.inverted,
    furnitureDropped: furniture.length,
    furniture,
    meanConfidence: counted ? weighted / counted : 0,
    timing: { preprocessMs: t1 - t0, recognizeMs: t2 - t1, totalMs: t2 - t0 },
  };
}

// ---------------------------------------------------------------------------- cleanup

/**
 * Make OCR output speakable. Hyphenation across a line break is the one that matters most -
 * "under-\nstand" spoken literally is two nonsense words.
 */
export function cleanup(raw: string): string {
  return raw
    .replace(/\r/g, '')
    .replace(/(\w)[-‐‑]\n(\w)/g, '$1$2') // de-hyphenate across line breaks
    .replace(/([^\n.!?:;"'”’)])\n(?=[a-z])/g, '$1 ') // unwrap mid-sentence breaks
    .replace(/[ \t]+/g, ' ')
    .replace(/\n{3,}/g, '\n\n')
    .split('\n')
    .map((l) => l.trim())
    .join('\n')
    .trim();
}
