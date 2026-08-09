// Page image -> text + word boxes. The only place the OCR backend is called.
//
// Recognition itself runs in the host now (`POST /ocr`, PP-OCR: a detector then a recognizer),
// not in this extension. What did NOT move is everything below the fetch: the column split,
// the missed-gutter retry, and the whole evidence-based furniture policy. Those rules decide
// what gets narrated, they can remove a line of the book silently, and they were paid for in
// four separate content losses - so the engine swap is the entire change and they stay where
// they are reviewed. The backend returns the raw structure they already consumed: lines in
// reading order, each a list of words with text, confidence and a rectangle.
//
// There is no in-extension fallback and there must not be one. A missing host is a state to
// report, not a reason to run a second engine nobody has measured against these fixtures.
//
// WHAT IS POSTED IS THE PAGE AS THE READER RENDERED IT - original colour, neither flattened
// nor inverted. That is a reversal, and the reason is the engine change. Tesseract wanted dark
// ink on a light ground, so this file used to hand it a grayscale, possibly inverted page; a
// detector whose job is to find four words inside an illustration needs every bit of that
// discarded contrast, and engine-specific preprocessing is the backend's to own now. The
// dark-page test survives, but only to know which way ink runs for the gutter search below.
//
// Checked on a rendered dark-theme fixture - light grey serif on near-black paper reads
// perfectly through the backend with no inversion at all, at full confidence. A real dark-theme
// Cloud Reader capture is still on the corpus gate, and if one comes back wrong the inversion
// belongs in the BACKEND next to the models that want it, not here.
//
// One preprocessing step is left, and it is here because it produces a SILENT failure - wrong
// text rather than an error - and is driven by a reader setting the user can change
// mid-session, so it is decided per page and never cached: a two-column page left unsplit is
// read across the gutter, line by line, into fluent-looking scrambled sentences. Detect the
// gutter and OCR each column.
//
// Word boxes come back in ORIGINAL image pixel coordinates - the column split's x offset is
// added back - so highlight.ts can map them to the screen with only the CSS/natural scale and
// the live devicePixelRatio.

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

/**
 * Where the OCR backend is, and what proves this client may use it.
 *
 * Passed in on every call rather than read from `chrome.storage` here. Two reasons, and the
 * second is the one that matters: this module stays free of any `chrome` dependency, so the
 * furniture rules can still be tested under bun without a browser; and the pairing already
 * travels this way for `/synth` (see the `http-synth` message), so there is one answer to
 * "who knows the token" instead of two.
 */
export interface OcrBackend {
  base: string;
  token: string;
}

// ------------------------------------------------------------------------- preprocess

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

// -------------------------------------------------------------------------- recognize

export interface RawWord {
  text: string;
  confidence: number;
  bbox: { x0: number; y0: number; x1: number; y1: number };
}

/**
 * The version-1 `/ocr` response, frozen with the host before this adapter was written.
 *
 * `version` is in it so a host and an extension that disagree say so, instead of the mismatch
 * arriving as an undefined field halfway down a page.
 */
interface HostResponse {
  version: number;
  engine: string;
  /** Both models, named separately - either can be repinned without the other. */
  detector: string;
  recognizer: string;
  width: number;
  height: number;
  lines: { words?: RawWord[] }[];
  detectMs: number;
  recognizeMs: number;
  ocrMs: number;
}

/** What this client can parse. Matches `RESPONSE_VERSION` in `kokoro-ocr`. */
export const RESPONSE_VERSION = 1;

/**
 * Recognize one prepared column image into lines of words.
 *
 * Line grouping arrives from the backend rather than being reassembled here: the whole point
 * of a line is that the furniture rules judge one, and the engine is what knows where one
 * ends.
 */
export type LineRecognizer = (
  image: Blob,
  backend: OcrBackend,
  signal?: AbortSignal,
) => Promise<RawWord[][]>;

let recognizer: LineRecognizer | null = null;

/**
 * Swap the recognizer out. For fixtures and for comparing engines offline - **not** a
 * production seam, and emphatically not a fallback: nothing installs one automatically, and a
 * host that cannot recognize a page is reported, never worked around.
 */
export function setRecognizer(r: LineRecognizer | null): void {
  recognizer = r;
}

/**
 * The posted size, in the unit the host's own cap is written in.
 *
 * It is in the message because an over-cap POST is the failure most likely to arrive with no
 * response at all: a client streams the body, the host answers and closes, and whether the reply
 * is ever read is a race the client loses. The host now drains before refusing, so the legible
 * 413 is what should turn up - and if one doesn't, this number is what says whether size was the
 * question. A full-page colour plate is where it matters; a page of prose is a fraction of a MiB.
 */
const mib = (bytes: number): string => `${(bytes / (1024 * 1024)).toFixed(2)} MiB`;

/** Post one column to the host and unpack it. */
async function recognizeViaHost(
  image: Blob,
  backend: OcrBackend,
  signal?: AbortSignal,
): Promise<RawWord[][]> {
  let res: Response;
  try {
    res = await fetch(`${backend.base}/ocr`, {
      method: 'POST',
      headers: { authorization: `Bearer ${backend.token}`, 'content-type': 'image/png' },
      body: image,
      signal,
    });
  } catch (e) {
    // A rejected fetch is the ONE failure that arrives with no status, no body and no URL -
    // `TypeError: Failed to fetch` and nothing else - so it is the one that has to be given
    // those facts here. An abort is Stop working and is left alone; the caller knows.
    if (signal?.aborted) throw e;
    throw new Error(`could not reach ${backend.base}/ocr (posted ${mib(image.size)}): ${String(e)}`);
  }

  if (!res.ok) throw new Error(await describeFailure(res, image.size));

  const data = (await res.json()) as HostResponse;
  if (data.version !== RESPONSE_VERSION)
    throw new Error(
      `the host speaks /ocr v${data.version}, this extension speaks v${RESPONSE_VERSION} - update both`,
    );

  // Rectangles are already in the coordinate space of the image that was posted, whatever
  // scale the backend used internally. The column x-offset is added exactly once, downstream.
  return data.lines.map((l) => l.words ?? []).filter((words) => words.some((w) => w.text?.trim()));
}

/**
 * Turn a failed response into a sentence naming the next action.
 *
 * The host distinguishes its failures on purpose - a missing language pack, a timeout and a
 * page it could not read are three different problems - and losing that distinction here would
 * put the browser engine's worst property back: one "OCR failed" for everything, so the user
 * retries the one thing retrying cannot fix.
 */
async function describeFailure(res: Response, posted: number): Promise<string> {
  let code = '';
  let error = '';
  try {
    const body = (await res.json()) as { code?: string; error?: string };
    code = body.code ?? '';
    error = body.error ?? '';
  } catch {
    // A body that is not JSON is still a failure; the status carries the rest.
  }
  switch (res.status) {
    case 401:
      return 'the Kokoro host rejected the pairing token - re-pair from the options page';
    case 403:
      return 'the Kokoro host does not allow this extension id';
    case 413:
      // The host's message states the LIMIT; only this side knows what was actually sent, and
      // the gap between the two is the whole of what anyone can act on. A page of prose is a
      // fraction of a MiB, so a number far above the cap says the page is a full-colour plate
      // being re-encoded losslessly rather than that the cap is merely a little tight.
      return `this page encodes to ${mib(posted)}, over the Kokoro host's limit${error ? ` (${error})` : ''}`;
    case 429:
      return 'the Kokoro host is busy with other pages - try again in a moment';
    case 503:
      return `the Kokoro host cannot do OCR${error ? `: ${error}` : ''}`;
    default:
      return `ocr ${res.status}${code ? ` (${code})` : ''}${error ? `: ${error}` : ''}`;
  }
}

/** Fraction of page height at top/bottom where running heads and folios live. */
const FURNITURE_BAND = 0.1;
/** A furniture line is short; a body line that happens to sit high on the page is not. */
const FURNITURE_MAX_WORDS = 8;
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
 * Is this line set to the column's measure, the way justified body text is?
 *
 * This is the signal that survives a font change, and the reason it is needed: the word count and
 * the sentence-end escape below both fail on the SAME line at a larger font. "To tend the lamp
 * the harbour lamp, to" is seven words, sits at the top of the page, and ends mid-clause - it looks
 * exactly like a running head to every test except this one. Enlarge the type and most of the
 * book's lines become short enough to look that way.
 *
 * Two things have to hold, and the second matters as much as the first. A running head can span
 * the full width too - title at the left, folio at the right - but it does that with one large
 * gap in the middle, where justification stretches the space between every pair of words
 * together. So: full measure, AND no gap far wider than the rest of them.
 */
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

function setAsBody(line: RawWord[], measure: number): boolean {
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
  // paragraph ending in the band ("the responsibility the harbour should bear.") reaches nothing
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
  const lines = await (recognizer ?? recognizeViaHost)(blob, backend, options.signal);

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
