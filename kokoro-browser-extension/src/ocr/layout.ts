// Page pixels -> one canvas per column. The only preprocessing left in the extension.
//
// Everything else went to the host with the engine; this stayed because it produces a SILENT
// failure rather than an error. A two-column page left unsplit is read across the gutter, line by
// line, into sentences that are fluent and wrong - every word real, the confidence high, and
// nothing downstream able to tell. It is also driven by a reader setting the user can change
// mid-session, so it is decided per page and never cached.
//
// NOTHING HERE IS WRITTEN BACK. The canvas is left exactly as the reader drew it and the columns
// cropped out of it are posted in the page's own colours; the luminance plane below is a separate
// buffer that exists to find the gutter and is then dropped. The dark-page test survives from the
// era when this file flattened and inverted the page for the engine - but it survives for one
// purpose only, which is knowing which way ink runs. See `backend.ts` for why the inversion left.
//
// There is no page-wide upscale either, and its absence is deliberate: the recognizer resizes
// every detected line to a fixed height from the SOURCE pixels, so small type is upsampled per
// line for free. A 2x in front of that resamples twice and quadruples the detector's input for
// nothing. (It was the dominant accuracy lever for the old engine, which read whatever
// resolution it was handed. Do not port the reasoning across.)

export interface Prepared {
  /** One canvas per column, left to right, in the page's original colours. */
  columns: OffscreenCanvas[];
  /** x offset of each column within the original image. */
  offsets: number[];
  /**
   * The page is set light-on-dark.
   *
   * Reported and logged, and used here for one thing only: which way "ink" runs when looking
   * for the gutter. Nothing is inverted - see the header.
   */
  inverted: boolean;
}

/** A gutter must be this fraction of the page height free of ink to count. */
const GUTTER_CLEAN = 0.995;
/** ...and this fraction of page width wide. */
const MIN_GUTTER_FRAC = 0.02;
/** Only look for a gutter in the middle of the page, not in the margins. */
const GUTTER_SEARCH = [0.3, 0.7] as const;
/** A pixel this far from the paper's own level counts as ink. */
const INK = 160;

/**
 * How much dirt a gutter may carry when we already have evidence one is there.
 *
 * `GUTTER_CLEAN` is what it takes to CLAIM a gutter with nothing else to go on, and it is strict
 * on purpose. This is the bar for CONFIRMING one the OCR has already shown us - a figure or a
 * rule crossing the gutter breaks the strict test, and one stray mark is not a reason to read the
 * page across its columns.
 */
const GUTTER_CLEAN_RETRY = 0.9;

export interface PreprocessOptions {
  /**
   * 'auto' looks for a gutter and splits if it finds a convincing one; 'split' takes the best
   * candidate band even if it is not pristine, for a page the OCR has already shown to be two
   * columns; 'single' never splits.
   */
  columns?: 'auto' | 'split' | 'single';
}

/**
 * The page's background level: the most common luminance on it.
 *
 * The MODE, not the mean. The paper is whatever colour most of the page is, and a mean is pulled
 * around by anything large that is not paper - a full-width plate, a dark figure, a table of
 * solid rules - so a page could average its way across a threshold while still plainly being
 * black text on white. The mode cannot: text covers a fraction of a page, so the tallest bin is
 * always the paper.
 *
 * With the paper level in hand there is no threshold left to tune: below the midpoint of the
 * range means the paper is dark, which is exactly what "rendered light-on-dark" means.
 */
function backgroundLevel(hist: Uint32Array): number {
  let best = 0;
  for (let v = 1; v < hist.length; v++) if (hist[v]! > hist[best]!) best = v;
  return best;
}

export async function preprocess(source: Blob | ImageBitmap, opts: PreprocessOptions = {}): Promise<Prepared> {
  const bitmap = source instanceof ImageBitmap ? source : await createImageBitmap(source);
  const w = bitmap.width;
  const h = bitmap.height;

  const canvas = new OffscreenCanvas(w, h);
  const ctx = canvas.getContext('2d', { willReadFrequently: true })!;
  ctx.drawImage(bitmap, 0, 0);

  // The canvas is left exactly as the reader drew it - the columns cropped out of it below are
  // what gets posted. Everything from here to the split reads a separate luminance plane and
  // writes nothing back.
  const px = ctx.getImageData(0, 0, w, h).data;

  // --- luminance + its histogram in one pass
  const gray = new Uint8Array(w * h);
  const hist = new Uint32Array(256);
  for (let i = 0, p = 0; i < px.length; i += 4, p++) {
    const g = (px[i]! * 0.299 + px[i + 1]! * 0.587 + px[i + 2]! * 0.114) | 0;
    gray[p] = g;
    hist[g]!++;
  }

  // --- which way does ink run? Dark paper means light text, so the ink test flips with it.
  const inverted = backgroundLevel(hist) < 128;

  // --- ink profile per column, for gutter detection
  const inkPerCol = new Float32Array(w);
  for (let y = 0; y < h; y++) {
    const row = y * w;
    for (let x = 0; x < w; x++) {
      const level = inverted ? 255 - gray[row + x]! : gray[row + x]!;
      if (level < INK) inkPerCol[x]! += 1;
    }
  }

  const want = opts.columns ?? 'auto';
  const cut = want === 'single' ? null : findGutter(inkPerCol, w, h, want === 'split' ? GUTTER_CLEAN_RETRY : GUTTER_CLEAN);
  if (cut === null) return { columns: [canvas], offsets: [0], inverted };

  return { columns: [crop(canvas, 0, cut), crop(canvas, cut, w - cut)], offsets: [0, cut], inverted };
}

/**
 * Centre of the widest clean vertical band in the middle of the page, or null for single column.
 *
 * A false positive here cannot slice through words, whatever it does to reading order: the band
 * has to be free of ink over `cleanFrac` of the FULL page height, so a page it splits has a real
 * empty stripe down the middle of it. The failure that does happen is the other one - one figure
 * or rule crossing the gutter breaks the strict test, no split is made, and the page is read
 * across its columns into fluent nonsense. `looksInterleaved` catches that afterwards and asks
 * for a second look with `cleanFrac` relaxed.
 */
function findGutter(ink: Float32Array, w: number, h: number, cleanFrac: number): number | null {
  const lo = Math.floor(w * GUTTER_SEARCH[0]);
  const hi = Math.ceil(w * GUTTER_SEARCH[1]);
  const clean = (1 - cleanFrac) * h;

  let best: { start: number; len: number } | null = null;
  let run = -1;

  for (let x = lo; x <= hi; x++) {
    if (ink[x]! <= clean) {
      if (run < 0) run = x;
    } else if (run >= 0) {
      const len = x - run;
      if (!best || len > best.len) best = { start: run, len };
      run = -1;
    }
  }
  if (run >= 0) {
    const len = hi - run;
    if (!best || len > best.len) best = { start: run, len };
  }

  if (!best || best.len < w * MIN_GUTTER_FRAC) return null;
  return Math.round(best.start + best.len / 2);
}

function crop(src: OffscreenCanvas, x: number, width: number): OffscreenCanvas {
  const out = new OffscreenCanvas(width, src.height);
  out.getContext('2d')!.drawImage(src, x, 0, width, src.height, 0, 0, width, src.height);
  return out;
}
