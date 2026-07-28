// Chooses where OCR runs, and reports which route was taken.
//
// Preferred: the offscreen document (extension origin, so Tesseract's worker is same-origin and
// the extension's CSP applies instead of Amazon's).
//
// Fallback: in this content script. Needed for Firefox, which has no chrome.offscreen, and for
// the test harnesses, which load the bundle as a plain page script. It only works if the
// browser permits a worker from an extension URL under the page's origin - the exact
// uncertainty the offscreen route exists to remove.

import { recognize, type OcrResult } from './ocr';

export type OcrRoute = 'offscreen' | 'in-page';

export interface RoutedOcr extends OcrResult {
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

async function viaOffscreen(blob: Blob): Promise<OcrResult> {
  const b64 = await blobToBase64(blob);
  const reply = (await chrome.runtime.sendMessage({ t: 'ocr', b64, type: blob.type })) as
    | { ok: true; result: OcrResult }
    | { ok: false; error: string }
    | undefined;

  if (!reply) throw new Error('no reply from the service worker (was it evicted?)');
  if (!reply.ok) throw new Error(reply.error);
  return reply.result;
}

const canOffscreen = () => typeof chrome !== 'undefined' && typeof chrome.runtime?.sendMessage === 'function';

/**
 * OCR one captured page image. Tries the offscreen document first and falls back to in-page on
 * failure, remembering which worked so later pages skip the failed attempt.
 */
export async function recognizePage(blob: Blob): Promise<RoutedOcr> {
  if (route === 'in-page' || !canOffscreen()) {
    route = 'in-page';
    return { ...(await recognize(blob)), route };
  }

  try {
    const result = await viaOffscreen(blob);
    route = 'offscreen';
    return { ...result, route };
  } catch (e) {
    console.warn('[kwr] offscreen OCR failed, falling back to in-page:', String(e));
    const result = await recognize(blob);
    route = 'in-page';
    return { ...result, route };
  }
}
