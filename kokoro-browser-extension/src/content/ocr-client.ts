// The content script's way of asking for a page to be recognized.
//
// ONE ROUTE: content script -> service worker -> offscreen document -> `POST /ocr` on the host.
// The hop through the offscreen document is not ceremony left over from the old wasm engine.
// Under MV3 a content script's `fetch` carries the PAGE's origin (`https://read.amazon.com`),
// and the host's allowlist admits `chrome-extension://<id>` and nothing else - so the request
// that looks like it should be the direct one is exactly the one that 403s. The offscreen
// document is an extension-origin context, which is also why the PCM fetch already lives there.
//
// There is no fallback. The in-page route this file used to keep existed because Tesseract's
// worker had to be same-origin; with recognition on the host it would be the same fetch from
// the wrong origin, and a "fallback" that cannot work is worse than none - it turns one legible
// failure into two, and only ever runs in the case nobody can reproduce. A missing or unhealthy
// host is reported and the page is left alone.

import { assertAttached } from './alive';
import type { ColumnOcr, OcrResult, RecognizeOptions } from './ocr';

/** A recognized result, or - for a column past the end of the page - just the column count. */
interface OcrOk<T> {
  ok: true;
  result?: T;
  columns?: number;
}

async function blobToBase64(blob: Blob): Promise<string> {
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

/**
 * Base64 rather than the bytes, because extension messaging is JSON-only.
 *
 * It costs 33 % on the way in and it is worth paying: the alternative is the content script
 * holding the pairing token, and the token is the only thing standing between the host and
 * every other script on the machine. It never leaves the extension's own contexts.
 */
async function viaOffscreen<T>(blob: Blob, extra: Record<string, unknown> = {}): Promise<OcrOk<T>> {
  // Before the base64, which is the expensive part of a page and pure waste once this script has
  // been orphaned - and before the raw `TypeError` that reading `sendMessage` off nothing throws.
  assertAttached();
  const b64 = await blobToBase64(blob);
  const reply = (await chrome.runtime.sendMessage({ t: 'ocr', b64, type: blob.type, ...extra })) as
    | OcrOk<T>
    | { ok: false; error: string }
    | undefined;

  if (!reply) throw new Error('no reply from the service worker (was it evicted?)');
  if (!reply.ok) throw new Error(reply.error);
  return reply;
}

/** OCR one captured page image, every column of it. The console path. */
export async function recognizePage(blob: Blob, options: RecognizeOptions = {}): Promise<OcrResult> {
  const reply = await viaOffscreen<OcrResult>(blob, { trial: options.trial });
  return reply.result!;
}

/**
 * OCR ONE column of a page, so the caller can start speaking the first while the second is
 * still being recognized. Null once `index` is past the last column - which is how a
 * single-column page announces itself.
 *
 * `key` identifies the render so the offscreen document can reuse its preprocessing between
 * columns.
 */
export async function recognizeColumnOf(blob: Blob, index: number, key: string): Promise<ColumnOcr | null> {
  const reply = await viaOffscreen<ColumnOcr>(blob, { column: index, key });
  return reply.result ?? null;
}
