// Page image -> text + word boxes. The only place Tesseract is touched.
//
// Two preprocessing steps exist because both produce SILENT failures - wrong text rather than
// an error - and both are driven by reader settings the user can change mid-session, so they
// are decided per page and never cached:
//
//   1. Dark mode. Tesseract on light-text-on-dark returns garbage. Detect and invert.
//   2. Two-column layout. Left ungated, Tesseract interleaves the columns line by line and
//      produces fluent-looking scrambled sentences. Detect the gutter and OCR each column.
//
// Word boxes come back in ORIGINAL image pixel coordinates - inversion changes no geometry and
// the column split's x offset is added back - so highlight.ts can map them to the screen with
// only the CSS/natural scale and the live devicePixelRatio.

export interface OcrWord {
  text: string;
  /** 0-100. Below ~60 usually means Tesseract guessed. */
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

export interface OcrAssets {
  workerPath: string;
  corePath: string;
  langPath: string;
}

/** In an extension these resolve to packaged files; the bench overrides them. */
export function defaultAssets(tier: 'fast' | 'standard' = 'fast'): OcrAssets {
  const url = (p: string) =>
    typeof chrome !== 'undefined' && chrome.runtime?.getURL ? chrome.runtime.getURL(p) : `/${p}`;
  return {
    workerPath: url('vendor/tesseract-worker.js'),
    corePath: url('vendor/tesseract-core-simd-lstm.wasm.js'),
    langPath: url(`vendor/tessdata-${tier}`),
  };
}

// ------------------------------------------------------------------------- preprocess

export interface Prepared {
  /** One canvas per column, left to right. */
  columns: OffscreenCanvas[];
  /** x offset of each column within the original image. */
  offsets: number[];
  inverted: boolean;
}

/** Luminance below this (0-255 mean) means the page is rendered light-on-dark. */
const DARK_MEAN = 110;
/** A gutter must be this fraction of the page height free of ink to count. */
const GUTTER_CLEAN = 0.995;
/** ...and this fraction of page width wide. */
const MIN_GUTTER_FRAC = 0.02;
/** Only look for a gutter in the middle of the page, not in the margins. */
const GUTTER_SEARCH = [0.3, 0.7] as const;
/** A pixel darker than this counts as ink (after any inversion). */
const INK = 160;

export async function preprocess(source: Blob | ImageBitmap): Promise<Prepared> {
  const bitmap = source instanceof ImageBitmap ? source : await createImageBitmap(source);
  const w = bitmap.width;
  const h = bitmap.height;

  const canvas = new OffscreenCanvas(w, h);
  const ctx = canvas.getContext('2d', { willReadFrequently: true })!;
  ctx.drawImage(bitmap, 0, 0);

  const img = ctx.getImageData(0, 0, w, h);
  const px = img.data;

  // --- grayscale + mean luminance in one pass
  let sum = 0;
  for (let i = 0; i < px.length; i += 4) {
    const g = (px[i]! * 0.299 + px[i + 1]! * 0.587 + px[i + 2]! * 0.114) | 0;
    px[i] = px[i + 1] = px[i + 2] = g;
    sum += g;
  }
  const mean = sum / (px.length / 4);

  // --- invert if the page is dark. Tesseract expects dark ink on a light ground.
  const inverted = mean < DARK_MEAN;
  if (inverted) {
    for (let i = 0; i < px.length; i += 4) {
      const v = 255 - px[i]!;
      px[i] = px[i + 1] = px[i + 2] = v;
    }
  }
  ctx.putImageData(img, 0, 0);

  // --- ink profile per column, for gutter detection
  const inkPerCol = new Float32Array(w);
  for (let y = 0; y < h; y++) {
    const row = y * w * 4;
    for (let x = 0; x < w; x++) {
      if (px[row + x * 4]! < INK) inkPerCol[x]! += 1;
    }
  }

  const cut = findGutter(inkPerCol, w, h);
  if (cut === null) return { columns: [canvas], offsets: [0], inverted };

  return { columns: [crop(canvas, 0, cut), crop(canvas, cut, w - cut)], offsets: [0, cut], inverted };
}

/**
 * Centre of the widest clean vertical band in the middle of the page, or null for single
 * column. Deliberately conservative: a false positive scrambles a single-column page far worse
 * than a false negative scrambles a two-column one.
 */
function findGutter(ink: Float32Array, w: number, h: number): number | null {
  const lo = Math.floor(w * GUTTER_SEARCH[0]);
  const hi = Math.ceil(w * GUTTER_SEARCH[1]);
  const clean = (1 - GUTTER_CLEAN) * h;

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

// -------------------------------------------------------------------------- recognize

type TesseractWorker = {
  recognize: (image: unknown, opts?: unknown, output?: unknown) => Promise<{ data: RawPage }>;
  terminate: () => Promise<unknown>;
};

interface RawWord {
  text: string;
  confidence: number;
  bbox: { x0: number; y0: number; x1: number; y1: number };
}
interface RawPage {
  text: string;
  confidence: number;
  words?: RawWord[];
  blocks?: { paragraphs?: { lines?: { words?: RawWord[] }[] }[] }[];
}

let workerPromise: Promise<TesseractWorker> | null = null;

/**
 * One worker for the process lifetime. Spinning one up costs ~1-2 s (wasm compile + language
 * load), which would blow the per-page budget on its own if paid per page.
 */
export async function getWorker(assets: OcrAssets = defaultAssets()): Promise<TesseractWorker> {
  workerPromise ??= (async () => {
    const { createWorker } = await import('tesseract.js');
    return (await createWorker('eng', 1, {
      workerPath: assets.workerPath,
      corePath: assets.corePath,
      langPath: assets.langPath,
      // Never let it reach for a CDN, and never build the worker from a blob: URL - MV3's CSP
      // rejects both.
      workerBlobURL: false,
      gzip: true,
    })) as unknown as TesseractWorker;
  })();
  return workerPromise;
}

export async function terminate(): Promise<void> {
  if (!workerPromise) return;
  const w = await workerPromise;
  workerPromise = null;
  await w.terminate();
}

/** Words grouped into lines - line structure is what makes furniture detection possible. */
function collectLines(page: RawPage): RawWord[][] {
  const out: RawWord[][] = [];
  for (const b of page.blocks ?? [])
    for (const p of b.paragraphs ?? [])
      for (const l of p.lines ?? []) {
        const words = (l.words ?? []).filter((w) => w.text?.trim());
        if (words.length) out.push(words);
      }
  if (out.length === 0 && page.words?.length) out.push(page.words.filter((w) => w.text?.trim()));
  return out;
}

/** Fraction of page height at top/bottom where running heads and folios live. */
const FURNITURE_BAND = 0.1;
/** A furniture line is short; a body line that happens to sit high on the page is not. */
const FURNITURE_MAX_WORDS = 8;

/**
 * Lines that are furniture no matter where they sit. Copyright notices in particular end in a
 * full stop ("All rights reserved."), which defeats any punctuation-based test.
 */
const FURNITURE_PATTERN =
  /^\s*(©|\(c\)|copyright\b)|all rights reserved|^\s*(19|20)\d{2}\s+\p{Lu}|^\s*\d{1,4}\s*$|^\s*page \d+/iu;

/**
 * Text of the same line seen in a band on earlier pages. A running head repeats verbatim page
 * after page, which is the strongest signal there is - and it needs no guess about wording.
 * Bounded so a long session cannot grow it without limit.
 */
const bandSeen = new Map<string, number>();
const BAND_MEMORY = 400;

function normalizeLine(text: string): string {
  return text.toLowerCase().replace(/[^a-z0-9]+/g, ' ').trim();
}

/** Forget the running-head history - call when switching books. */
export function resetFurnitureMemory(): void {
  bandSeen.clear();
}

/**
 * Why this line is furniture rather than body text, or null to keep it.
 *
 * Furniture OCRs *perfectly* and is worse for it: a running head or copyright notice spoken
 * between every page is what makes narration unusable, and no accuracy check can see it because
 * the characters are correct.
 */
function furnitureReason(line: RawWord[], pageHeight: number): string | null {
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
  const inBand = mid < pageHeight * FURNITURE_BAND || mid > pageHeight * (1 - FURNITURE_BAND);
  if (!inBand) return null;

  const key = normalizeLine(text);
  if (key && (bandSeen.get(key) ?? 0) > 0) return 'repeats';

  if (key) {
    if (bandSeen.size > BAND_MEMORY) bandSeen.clear();
    bandSeen.set(key, (bandSeen.get(key) ?? 0) + 1);
  }

  // A line in the band that ends mid-sentence is body text that happens to sit near the edge.
  if (/[.!?]["'”’)]?$/.test(text)) return null;
  return 'band';
}

/**
 * OCR one captured page. Columns are recognized separately and concatenated in reading order -
 * left column fully, then right - which is the whole point of the split.
 */
export async function recognize(source: Blob | ImageBitmap, assets?: OcrAssets): Promise<OcrResult> {
  const t0 = performance.now();
  const prepared = await preprocess(source);
  const t1 = performance.now();

  const worker = await getWorker(assets);

  // Text is assembled from the retained lines rather than taken from Tesseract's `data.text`,
  // because furniture has to be dropped from BOTH the text and the word list or highlighting
  // and narration disagree about what is on the page.
  const kept: { word: RawWord; dx: number }[][] = [];
  let confSum = 0;
  let confN = 0;
  const furniture: { text: string; reason: string }[] = [];

  for (let i = 0; i < prepared.columns.length; i++) {
    const canvas = prepared.columns[i]!;
    const dx = prepared.offsets[i]!;
    const blob = await canvas.convertToBlob({ type: 'image/png' });
    const { data } = await worker.recognize(blob, {}, { blocks: true, text: true });

    for (const line of collectLines(data)) {
      const reason = furnitureReason(line, canvas.height);
      if (reason) {
        furniture.push({ text: line.map((w) => w.text.trim()).join(' '), reason });
        continue;
      }
      kept.push(line.map((word) => ({ word, dx })));
      for (const w of line) {
        confSum += w.confidence;
        confN++;
      }
    }
  }

  const rawText = kept.map((line) => line.map((e) => e.word.text.trim()).join(' ')).join('\n');
  const text = cleanup(rawText);

  // cleanup() joins hyphenated words and unwraps lines, so offsets must be re-derived against
  // the final string. A word that cleanup merged away (the head of a hyphen split) simply keeps
  // the cursor position - it still points into the right region for highlighting.
  const words: OcrWord[] = [];
  let cursor = 0;
  for (const line of kept) {
    for (const { word, dx } of line) {
      const t = word.text.trim();
      const at = text.indexOf(t, cursor);
      if (at >= 0) cursor = at + t.length;
      words.push({
        text: t,
        confidence: word.confidence,
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
    furnitureDropped: furniture.length,
    furniture,
    meanConfidence: confN ? confSum / confN : 0,
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
