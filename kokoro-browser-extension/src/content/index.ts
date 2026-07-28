// Content-script entry. Everything is exposed on a single global so it can be driven from the
// console.
//
// NOTE: this runs in the isolated world, so `kwr` is NOT visible in the page's own console
// context. In DevTools, switch the console's context dropdown (top-left, usually reading
// "top") to "Kokoro Kindle Reader" - the extension's manifest name - first.

import * as capture from './capture';
import * as ocr from './ocr';
import { recognizePage, lastRoute, useRoute, type RoutedOcr } from './ocr-client';
import * as narrate from './narrate';
import { mountPanel, type PanelHandle } from './panel';

let stopLoop: (() => void) | null = null;
let panel: PanelHandle | null = null;

const api = {
  ...capture,
  ocr,
  narrate,
  lastRoute,
  useRoute,

  /**
   * What the reader fetched, and what was in it. The page-world probe (net-probe.ts) hooks
   * fetch/XHR at document_start; this asks it for a summary.
   *
   * Turn several pages first - pages arrive in batches, so a fresh tab may have captured
   * nothing yet.
   */
  net(): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        removeEventListener('message', on);
        reject(new Error('no reply from the page-world probe - is net-probe.js loading at document_start?'));
      }, 5000);

      const on = (e: MessageEvent) => {
        if (e.source !== window || (e.data as { __kwr?: string })?.__kwr !== 'net-summary-result') return;
        clearTimeout(timer);
        removeEventListener('message', on);
        resolve((e.data as { summary: unknown }).summary);
      };

      addEventListener('message', on);
      postMessage({ __kwr: 'net-summary' }, location.origin);
    });
  },

  /** Watch page turns and log each one. Returns the stop function. */
  watch(): () => void {
    console.log('[kwr] watching for page turns...');
    return capture.onPageChange((page, pos) => {
      console.log('[kwr] page turn', {
        src: page.src.slice(0, 48),
        natural: page.natural,
        bytes: page.bytes.size,
        position: pos,
      });
    });
  },

  /** Capture -> OCR the current page, without speaking. Returns the recognized text. */
  async readPage(): Promise<RoutedOcr> {
    const img = await capture.capture(await capture.waitForSettled());
    const result = await recognizePage(img.bytes);
    console.log(
      `[kwr] OCR via ${result.route}, ${result.columns} column(s)${result.inverted ? ', inverted' : ''}, ` +
        `${result.words.length} words, conf ${result.meanConfidence.toFixed(1)}, ` +
        `${result.timing.totalMs.toFixed(0)}ms (preprocess ${result.timing.preprocessMs.toFixed(0)}ms)`,
    );
    console.log(result.text.slice(0, 400) + (result.text.length > 400 ? '\n...' : ''));
    return result;
  },

  /** The whole loop for one page: capture -> OCR -> speak. */
  async speakPage(options?: Parameters<typeof narrate.narrate>[1]): Promise<void> {
    const voices = await narrate.checkVoices();
    if (!voices.ok) {
      console.error('[kwr] no usable voice.', voices.advice ?? '');
      return;
    }
    const result = await api.readPage();
    if (!result.text.trim()) {
      console.warn('[kwr] OCR returned no text - nothing to speak');
      return;
    }
    await narrate.narrate(result.text, options, (charIndex) => {
      // Highlighting hangs off this: charIndex -> the OCR word -> its bbox -> a screen rect.
      void charIndex;
    });
  },

  /** Read continuously, advancing as the reader turns pages. */
  async readBook(options?: Parameters<typeof narrate.narrate>[1]): Promise<void> {
    if (stopLoop) {
      console.warn('[kwr] already reading - call kwr.stop() first');
      return;
    }
    let cancelled = false;
    /** Releases the page-turn wait, when there is one parked. */
    let wake: (() => void) | null = null;

    const mine = (stopLoop = () => {
      cancelled = true;
      narrate.stop();
      // Stop must end the loop NOW, not whenever the reader happens to turn a page. Without
      // this the loop stayed parked on onPageChange with its listener registered, and a page
      // turn minutes later would wake a loop nobody was waiting for.
      wake?.();
      stopLoop = null;
    });

    try {
      while (!cancelled) {
        await api.speakPage(options);
        if (cancelled) break;
        // Auto-advance is Phase 4; for now wait for the reader's own page turn.
        console.log('[kwr] page finished - turn the page to continue');
        await new Promise<void>((resolve) => {
          const done = () => {
            off();
            wake = null;
            resolve();
          };
          const off = capture.onPageChange(done);
          wake = done;
        });
      }
    } finally {
      // However the loop ended - Stop, or a thrown OCR/synthesis/port error - the reader has to
      // be startable again. Leaking this handle wedged every later press behind "already
      // reading", with no way to clear it from the panel because Stop is disabled when idle.
      // Guarded by identity so a fast Stop-then-Play cannot have its new loop cleared by the
      // old one unwinding.
      if (stopLoop === mine) stopLoop = null;
    }
  },

  /** Probe the native backend and report exactly what happened. */
  kokoro(): Promise<unknown> {
    return narrate.kokoroStatus();
  },

  stop(): void {
    stopLoop?.();
    narrate.stop();
    console.log('[kwr] stopped');
  },

  /** Show/hide the control panel by hand; it also mounts itself when a book is open. */
  panel: {
    show(): void {
      panel?.destroy();
      panel = mountPanel({
        readBook: (o) => api.readBook(o),
        stop: () => api.stop(),
        pause: () => narrate.pause(),
        resume: () => narrate.resume(),
        voices: () => narrate.voices(),
        engine: () => narrate.engineKind(),
        engineError: () => narrate.engineError(),
        position: () => capture.position(),
      });
    },
    hide(): void {
      panel?.destroy();
      panel = null;
    },
  },
};

/** The console/test-facing surface. Exported for typing only; the bundle is an IIFE. */
export type ContentApi = typeof api;

declare global {
  interface Window {
    kwr: ContentApi;
  }
}

window.kwr = api;

// --- debug bridge to the page world ------------------------------------------------------
// `kwr` lives here in the isolated world, which means DevTools only reaches it after switching
// the console's context dropdown - a step that is easy to miss and gives a bare
// "kwr is not defined" when you do. net-probe.ts mirrors these onto the page's `window.kwr`
// and forwards the calls here.
//
// Only this allowlist is reachable, and results are JSON round-tripped, so the page cannot
// reach into extension internals or be handed live objects. It is still a debug affordance -
// Amazon's own scripts could call these too. Drop the bridge before this is anything but a
// personal tool.
const REMOTE = new Set([
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
]);

addEventListener('message', async (e: MessageEvent) => {
  const d = e.data as { __kwr?: string; id?: string; method?: string; args?: unknown[] };
  if (e.source !== window || d?.__kwr !== 'cmd') return;

  const reply = (body: Record<string, unknown>) =>
    postMessage({ __kwr: 'cmd-result', id: d.id, ...body }, location.origin);

  try {
    const method = d.method ?? '';
    if (!REMOTE.has(method)) throw new Error(`not exposed over the bridge: ${method}`);
    const fn = method === 'checkVoices' ? narrate.checkVoices : (api as Record<string, unknown>)[method];
    if (typeof fn !== 'function') throw new Error(`no such method: ${method}`);

    const value = await (fn as (...a: unknown[]) => unknown).apply(api, d.args ?? []);
    // Round-trip so nothing non-cloneable (DOM nodes, functions) can escape.
    reply({ ok: true, value: JSON.parse(JSON.stringify(value ?? null)) });
  } catch (err) {
    reply({ ok: false, error: String(err) });
  }
});

const r = capture.route();
console.log(
  `[kwr] loaded on ${r.href}\n` +
    `      onReader=${r.onReader} asin=${r.asin}\n` +
    `      try: await kwr.selftest() | await kwr.readPage() | await kwr.speakPage() | kwr.stop()\n` +
    `           await kwr.dumpCapture() | await kwr.net()`,
);

// The reader is an SPA: opening a book does not reload the document, so the route has to be
// observed rather than waited for.
capture.onRouteChange((next) => {
  console.log('[kwr] route change', next);
});

// Mount the panel only when a book is actually open and rendered - not on the library, not on a
// loading state. Unmounts again when the reader goes away.
capture.onReaderActive((active) => {
  if (active && !panel) {
    api.panel.show();
    console.log('[kwr] reader detected - panel mounted');
  } else if (!active && panel) {
    api.panel.hide();
  }
});
