// Investigation tool. Runs in the PAGE's world at document_start, before the reader's own
// code, and wraps fetch/XHR so every request the reader makes is recorded.
//
// Why this layer: the reader never holds the book as text. It fetches batches of pages from
// /renderer/render as an uncompressed tar of glyph outlines (glyphs.json) plus positions
// (page_data_*.json), draws them to a canvas, and converts that to the blob: <img> we capture.
// The characters exist only as glyph IDs, and the ID->character mapping is reshuffled every few
// pages. So this probe answers the questions that decide the whole design:
//
//   1. Is that still how it works?           -> which endpoints are hit, and what comes back
//   2. Do positions come with the page?      -> exact word boxes would beat OCR bbox estimates
//   3. Where does the page image come from?  -> canvas (client-rendered) or network (server-rendered)
//
// It records metadata and keeps the most recent archives in memory for inspection. It does not
// decode glyph mappings, and nothing is sent anywhere.

import { listTar, readEntryText, type TarEntry } from '../tar';

interface Record_ {
  url: string;
  method: string;
  status: number;
  type: string | null;
  bytes: number;
  at: number;
  tar?: { name: string; size: number }[];
}

interface BlobOrigin {
  url: string;
  size: number;
  type: string;
  /** Where it was created - the frames tell us canvas vs network. */
  stack: string;
  at: number;
}

const MAX_ARCHIVES = 4;
const INTERESTING = /\/(renderer\/render|service\/|startReading|metadata)/i;

const records: Record_[] = [];
const archives = new Map<string, ArrayBuffer>();
const blobOrigins: BlobOrigin[] = [];

function note(rec: Record_, body?: ArrayBuffer) {
  if (body && /renderer\/render/i.test(rec.url)) {
    try {
      const entries = listTar(body);
      if (entries.length) {
        rec.tar = entries.map((e) => ({ name: e.name, size: e.size }));
        archives.set(rec.url, body);
        while (archives.size > MAX_ARCHIVES) archives.delete(archives.keys().next().value!);
      }
    } catch {
      /* not a tar; the metadata alone is still worth having */
    }
  }
  records.push(rec);
}

// --- fetch ------------------------------------------------------------------------------
const origFetch = window.fetch;
const wrappedFetch = async function (this: unknown, ...args: Parameters<typeof fetch>) {
  const res = await origFetch.apply(this as never, args);
  try {
    const url = typeof args[0] === 'string' ? args[0] : (args[0] as Request).url ?? String(args[0]);
    if (INTERESTING.test(url)) {
      const clone = res.clone();
      const body = await clone.arrayBuffer();
      note(
        {
          url,
          method: (args[1]?.method ?? 'GET').toUpperCase(),
          status: res.status,
          type: res.headers.get('content-type'),
          bytes: body.byteLength,
          at: Date.now(),
        },
        body,
      );
    }
  } catch {
    /* never let the probe break the reader */
  }
  return res;
};
// Object.assign so static members (fetch.preconnect) survive the wrap.
window.fetch = Object.assign(wrappedFetch, origFetch);

// --- XHR --------------------------------------------------------------------------------
const origOpen = XMLHttpRequest.prototype.open;
const origSend = XMLHttpRequest.prototype.send;
XMLHttpRequest.prototype.open = function (method: string, url: string | URL, ...rest: unknown[]) {
  (this as XMLHttpRequest & { __kwr?: { method: string; url: string } }).__kwr = { method, url: String(url) };
  return origOpen.apply(this, [method, url, ...rest] as never);
};
XMLHttpRequest.prototype.send = function (...args: unknown[]) {
  const meta = (this as XMLHttpRequest & { __kwr?: { method: string; url: string } }).__kwr;
  if (meta && INTERESTING.test(meta.url)) {
    this.addEventListener('load', () => {
      const body = this.response instanceof ArrayBuffer ? this.response : undefined;
      note(
        {
          url: meta.url,
          method: meta.method,
          status: this.status,
          type: this.getResponseHeader('content-type'),
          bytes: body?.byteLength ?? Number(this.getResponseHeader('content-length') ?? 0),
          at: Date.now(),
        },
        body,
      );
    });
  }
  return origSend.apply(this, args as never);
};

// --- where do the page images come from? ------------------------------------------------
// A blob created right after canvasToBlob means the page is rendered ON THE CLIENT, which is
// what makes the glyph data present at all. If page images arrived straight off the network,
// the client would hold no text-like data whatsoever and OCR would be the only conceivable
// route - so this one line of evidence decides a lot.
const origCreate = URL.createObjectURL;
URL.createObjectURL = function (obj: Blob | MediaSource): string {
  const url = origCreate.call(this, obj);
  try {
    if (obj instanceof Blob && obj.size > 10_000) {
      blobOrigins.push({
        url: url.slice(0, 60),
        size: obj.size,
        type: obj.type,
        stack: (new Error().stack ?? '').split('\n').slice(1, 6).join(' | '),
        at: Date.now(),
      });
      if (blobOrigins.length > 20) blobOrigins.shift();
    }
  } catch {
    /* ignore */
  }
  return url;
};

// --- reporting --------------------------------------------------------------------------
/** Real archive members, excluding libarchive's PAX metadata pseudo-entries. */
const real = (e: TarEntry) => !e.name.includes('PaxHeaders');

function summary() {
  const byUrl = new Map<string, { n: number; bytes: number; type: string | null }>();
  for (const r of records) {
    const key = r.url.split('?')[0]!;
    const cur = byUrl.get(key) ?? { n: 0, bytes: 0, type: r.type };
    byUrl.set(key, { n: cur.n + 1, bytes: cur.bytes + r.bytes, type: r.type });
  }

  const latest = [...archives.entries()].pop();
  let archive: unknown = null;
  if (latest) {
    const [url, buf] = latest;
    const entries = listTar(buf);
    archive = {
      url: url.slice(0, 120),
      totalBytes: buf.byteLength,
      entries: entries.filter(real).map((e) => ({ name: e.name, size: e.size })),
      // Peek at the head of each member so the shape is visible without dumping a book.
      // PaxHeaders.X/* are libarchive's PAX metadata records, not book data - skipping them
      // matters because they are tiny and would otherwise crowd out every real file.
      peek: Object.fromEntries(
        entries
          .filter((e) => real(e) && /\.json$/.test(e.name))
          .map((e: TarEntry) => [e.name, readEntryText(buf, e).slice(0, 400)]),
      ),
    };
  }

  return {
    endpoints: [...byUrl.entries()].map(([url, v]) => ({ url: url.slice(0, 120), ...v })),
    requests: records.length,
    archivesHeld: archives.size,
    archive,
    blobOrigins,
    verdict: archives.size
      ? 'renderer/render tar captured - inspect `archive.entries` and `archive.peek`'
      : 'no tar captured yet - turn a few pages with the probe loaded, then re-run',
  };
}

// --- bridge to the isolated world --------------------------------------------------------
addEventListener('message', (e: MessageEvent) => {
  if (e.source !== window || (e.data as { __kwr?: string })?.__kwr !== 'net-summary') return;
  postMessage({ __kwr: 'net-summary-result', summary: summary() }, location.origin);
});

(window as unknown as { __kwrNet: unknown }).__kwrNet = { summary, records, archives, blobOrigins };

// --- page-world handle for the isolated-world API ----------------------------------------
// So `await kwr.selftest()` works in the default `top` console context, without hunting for
// the context dropdown. Calls are forwarded to index.ts, which enforces its own allowlist.
const BRIDGED = [
  'selftest',
  'route',
  'position',
  'shadowHostCount',
  'readPage',
  'speakPage',
  'readBook',
  'dumpCapture',
  'net',
  'stop',
  'checkVoices',
  'kokoro',
] as const;

function call(method: string, args: unknown[]): Promise<unknown> {
  const id = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      removeEventListener('message', on);
      reject(new Error(`kwr.${method}() timed out - is the content script loaded? (reload the tab)`));
    }, 120_000); // OCR and narration are slow; this is a stall guard, not a deadline

    const on = (e: MessageEvent) => {
      const d = e.data as { __kwr?: string; id?: string; ok?: boolean; value?: unknown; error?: string };
      if (e.source !== window || d?.__kwr !== 'cmd-result' || d.id !== id) return;
      clearTimeout(timer);
      removeEventListener('message', on);
      d.ok ? resolve(d.value) : reject(new Error(d.error));
    };

    addEventListener('message', on);
    postMessage({ __kwr: 'cmd', id, method, args }, location.origin);
  });
}

const bridge = Object.fromEntries(
  BRIDGED.map((m) => [m, (...args: unknown[]) => call(m, args)]),
) as Record<(typeof BRIDGED)[number], (...args: unknown[]) => Promise<unknown>>;

(window as unknown as { kwr: unknown }).kwr = bridge;

console.log(
  '[kwr] network probe armed (page world, document_start)\n' +
    `      kwr bridged into this context: ${BRIDGED.join(', ')}\n` +
    '      also: __kwrNet.summary()',
);
