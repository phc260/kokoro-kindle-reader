// Fallback fetcher, injected into the PAGE's world only if an isolated-world fetch of the
// blob: URL fails. Blob URLs are origin-scoped and a content script shares the page origin,
// so this should be dead code on every browser that behaves - but "should" is doing a lot of
// work in a document nobody at Amazon wrote for us, and losing pixel access is fatal to the
// whole project. Cheap insurance.
//
// Blobs are structured-cloneable, so the bytes cross the world boundary via postMessage
// without base64 or a copy through a data: URL.

interface CaptureRequest {
  __kwr: 'capture-request';
  id: string;
  src: string;
}

addEventListener('message', async (e: MessageEvent) => {
  const d = e.data as CaptureRequest | undefined;
  if (e.source !== window || !d || d.__kwr !== 'capture-request') return;

  // Only ever fetch same-origin blob: URLs - never an arbitrary URL handed to us.
  if (typeof d.src !== 'string' || !d.src.startsWith('blob:')) {
    postMessage({ __kwr: 'capture-result', id: d.id, ok: false, error: 'refused: not a blob: URL' }, location.origin);
    return;
  }

  try {
    const res = await fetch(d.src);
    if (!res.ok) throw new Error(`status ${res.status}`);
    postMessage({ __kwr: 'capture-result', id: d.id, ok: true, blob: await res.blob() }, location.origin);
  } catch (err) {
    postMessage({ __kwr: 'capture-result', id: d.id, ok: false, error: String(err) }, location.origin);
  }
});
