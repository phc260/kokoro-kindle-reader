// Chooses where OCR runs, and reports which route was taken.
//
// Preferred: the offscreen document (extension origin, so Tesseract's worker is same-origin and
// the extension's CSP applies instead of Amazon's).
//
// Fallback: in this content script. Needed for Firefox, which has no chrome.offscreen, and for
// the test harnesses, which load the bundle as a plain page script. It only works if the
// browser permits a worker from an extension URL under the page's origin - the exact
// uncertainty the offscreen route exists to remove.

import {
  preprocess,
  recognize,
  recognizeColumn,
  recognizeColumnChecked,
  type ColumnOcr,
  type OcrResult,
  type Prepared,
  type RecognizeOptions,
} from './ocr';

export type OcrRoute = 'offscreen' | 'in-page';

export interface RoutedOcr extends OcrResult {
  route: OcrRoute;
}

export interface RoutedColumn extends ColumnOcr {
  route: OcrRoute;
}

let route: OcrRoute | null = null;

/** Which route the last OCR used, or null before the first page. */
export function lastRoute(): OcrRoute | null {
  return route;
}

/** Force a route, for comparing them. */
export function useRoute(r: OcrRoute | null): void {
  route = r;
}

function blobToBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const fr = new FileReader();
    fr.onerror = () => reject(fr.error ?? new Error('FileReader failed'));
    fr.onload = () => {
      const s = String(fr.result);
      resolve(s.slice(s.indexOf(',') + 1)); // strip the data: prefix
    };
    fr.readAsDataURL(blob);
  });
}

/** A recognized result, or - for a column past the end of the page - just the column count. */
interface OcrOk<T> {
  ok: true;
  result?: T;
  columns?: number;
}

async function viaOffscreen<T>(blob: Blob, extra: Record<string, unknown> = {}): Promise<OcrOk<T>> {
  const b64 = await blobToBase64(blob);
  const reply = (await chrome.runtime.sendMessage({ t: 'ocr', b64, type: blob.type, ...extra })) as
    | OcrOk<T>
    | { ok: false; error: string }
    | undefined;

  if (!reply) throw new Error('no reply from the service worker (was it evicted?)');
  if (!reply.ok) throw new Error(reply.error);
  return reply;
}

const canOffscreen = () => typeof chrome !== 'undefined' && typeof chrome.runtime?.sendMessage === 'function';

/**
 * OCR one captured page image. Tries the offscreen document first and falls back to in-page on
 * failure, remembering which worked so later pages skip the failed attempt.
 */
export async function recognizePage(blob: Blob, options: RecognizeOptions = {}): Promise<RoutedOcr> {
  if (route === 'in-page' || !canOffscreen()) {
    route = 'in-page';
    return { ...(await recognize(blob, undefined, options)), route };
  }

  try {
    const reply = await viaOffscreen<OcrResult>(blob, { trial: options.trial });
    route = 'offscreen';
    return { ...reply.result!, route };
  } catch (e) {
    console.warn('[kwr] offscreen OCR failed, falling back to in-page:', String(e));
    const result = await recognize(blob, undefined, options);
    route = 'in-page';
    return { ...result, route };
  }
}

/**
 * OCR ONE column of a page, so the caller can start speaking the first while the second is still
 * being recognized. Null once `index` is past the last column - which is how a single-column page
 * announces itself.
 *
 * `key` identifies the render so the offscreen document can reuse its preprocessing between
 * columns; the in-page fallback keeps its own copy for the same reason.
 */
export async function recognizeColumnOf(blob: Blob, index: number, key: string): Promise<RoutedColumn | null> {
  const local = async (): Promise<RoutedColumn | null> => {
    if (inPagePrepared?.key !== key) inPagePrepared = { key, page: await preprocess(blob) };
    let page = inPagePrepared.page;
    if (index >= page.columns.length) return null;
    // Same missed-gutter check the offscreen route does. Without it this path - Firefox, and the
    // fallback whenever the offscreen document is unavailable - reads a two-column page whose
    // gutter is hidden by a figure straight across both columns, and narrates fluent nonsense.
    if (index === 0) {
      const checked = await recognizeColumnChecked(blob, page, 0);
      if (checked.prepared !== page) inPagePrepared = { key, page: (page = checked.prepared) };
      return { ...checked.result, route: 'in-page' };
    }
    return { ...(await recognizeColumn(page, index)), route: 'in-page' };
  };

  if (route === 'in-page' || !canOffscreen()) {
    route = 'in-page';
    return local();
  }

  try {
    const reply = await viaOffscreen<ColumnOcr>(blob, { column: index, key });
    route = 'offscreen';
    return reply.result ? { ...reply.result, route } : null;
  } catch (e) {
    console.warn('[kwr] offscreen OCR failed, falling back to in-page:', String(e));
    route = 'in-page';
    return local();
  }
}

/** The in-page fallback's equivalent of the offscreen document's prepared-page cache. */
let inPagePrepared: { key: string; page: Prepared } | null = null;
