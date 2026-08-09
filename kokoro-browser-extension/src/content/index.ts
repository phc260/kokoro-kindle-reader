// Content-script entry. Everything is exposed on a single global so it can be driven from the
// console.
//
// NOTE: this runs in the isolated world, so `kwr` is NOT visible in the page's own console
// context. In DevTools, switch the console's context dropdown (top-left, usually reading
// "top") to "Kokoro Kindle Reader" - the extension's manifest name - first.

import * as capture from './capture';
import * as ocr from './ocr';
import { recognizePage, recognizeColumnOf } from './ocr-client';
import { sentenceEnd, type TextPart } from '../speak';
import * as narrate from './narrate';
import { mountPanel, type PanelHandle } from './panel';
import { mountHighlight, type HighlightHandle } from './highlight';

let stopLoop: (() => void) | null = null;
let panel: PanelHandle | null = null;
let highlight: HighlightHandle | null = null;
/**
 * Which page currently owns the (shared, single) highlight.
 *
 * Stop clears `stopLoop` synchronously, so Play can start the next page while the one it
 * cancelled is still unwinding through the worker. Without this the old page's cleanup runs
 * afterwards and blanks the new page's word list - the mark then never appears again until the
 * page turns.
 */
let markOwner = 0;

/**
 * Furniture is the only way a line of the book can go unread, and it is invisible by
 * construction: the dropped text OCR'd perfectly, so no confidence or accuracy check can point
 * at it. `ocr.ts` has always collected the reasons; nothing ever showed them, which made "a line
 * was skipped" a bug with no evidence anywhere.
 */
function logDropped(furniture: { text: string; reason: string }[]): void {
  if (furniture.length) {
    console.log('[kwr] not narrated:', furniture.map((f) => `"${f.text}" [${f.reason}]`).join(', '));
  }
}

/**
 * Capture and OCR the page that is on screen now, all of it.
 *
 * The console path. Narration uses `readColumns` instead, which recognizes a column at a time so
 * it can start speaking before the whole page is done.
 */
async function scanPage(): Promise<{ page: capture.PageImage; result: ocr.OcrResult }> {
  const page = await capture.capture(await capture.waitForSettled());
  const result = await recognizePage(page.bytes);
  console.log(
    `[kwr] OCR on the host, ${result.columns} column(s)${result.inverted ? ', inverted' : ''}, ` +
      `${result.words.length} words, conf ${result.meanConfidence.toFixed(1)}, ` +
      `${result.timing.totalMs.toFixed(0)}ms (preprocess ${result.timing.preprocessMs.toFixed(0)}ms)`,
  );
  logDropped(result.furniture);
  return { page, result };
}

/**
 * A page's text, a column at a time, as parts the narrator can start speaking immediately.
 *
 * This is where the wait for the first word is bought back. A two-column page took the whole
 * page's OCR before a sound came out, even though everything needed to start existed after the
 * first column - about a second of the four spent staring at a silent page. Now the second column
 * is recognized underneath the first one being read.
 *
 * Two rules make the seam inaudible:
 *
 *   1. A part is cut at the last SENTENCE end, not at the column boundary. The tail of a column
 *      almost always runs into the next one mid-sentence, and each part is chunked separately, so
 *      cutting there would break the delivery in the middle of a sentence. The remainder is
 *      carried and yielded with the column that continues it.
 *   2. Every part carries its `base` in the page's text, so a word boundary from the second
 *      column still addresses the same string the highlight indexed.
 */
async function* readColumns(
  page: capture.PageImage,
  mark: HighlightHandle,
  owns: () => boolean,
): AsyncGenerator<TextPart> {
  /** The page's text so far. Everything downstream reports offsets into this. */
  let text = '';
  /** How much of it has been handed to the narrator. */
  let sent = 0;
  let total = 1;

  for (let i = 0; i < total; i++) {
    const col = await recognizeColumnOf(page.bytes, i, page.src);
    if (!col) break;
    total = col.columns;

    // Joined with a single '\n' - the same join `joinColumns` uses, so the console path and this
    // one produce identical offsets for the same page.
    const base = text ? text.length + 1 : 0;
    text = text ? `${text}\n${col.text}` : col.text;

    // The second column can land after a Stop-then-Play has handed the mark to the next page;
    // pointing it back at this one would replace that page's boxes with these.
    if (owns()) {
      const words = col.words.map((w) => ({ ...w, charStart: w.charStart + base }));
      if (i === 0) mark.page({ src: page.src, natural: page.natural, words, inverted: col.inverted });
      else mark.extend(words);
    }

    console.log(
      `[kwr] OCR on the host, column ${i + 1}/${total}${col.inverted ? ', inverted' : ''}, ` +
        `${col.words.length} words, conf ${col.meanConfidence.toFixed(1)}, ` +
        `${col.timing.totalMs.toFixed(0)}ms`,
    );
    logDropped(col.furniture);

    // The last column has nothing left to run into, so it goes whole.
    const cut = i === total - 1 ? text.length : sentenceEnd(text, sent);
    if (cut > sent) {
      yield { text: text.slice(sent, cut), base: sent };
      sent = cut;
    }
  }

  if (!text.trim()) console.warn('[kwr] OCR returned no text - nothing to speak');
}

/**
 * Keep the mark valid while the reader re-renders under it.
 *
 * Resizing or zooming the window makes the reader re-render the page: a fresh blob: URL, a fresh
 * layout, and text reflowed onto different lines. The boxes we OCR'd stop describing what is on
 * screen, so highlight.ts correctly refuses to draw on them - and without this it would then stay
 * hidden for the rest of the page, which is what a resize looked like from the outside.
 *
 * So each new render is OCR'd again and handed over as a reflow. The narration is untouched: it
 * is still speaking the text captured at the start of the page, and only the boxes have moved.
 * A render that is NOT the same passage - the reader turning the page out from under a narration
 * still finishing the last one - simply fails to match and draws nothing, which is right.
 *
 * Returns an unsubscribe.
 */
function followReflow(mark: HighlightHandle): () => void {
  let done = false;
  let inflight = false;
  let latest: capture.PageImage | null = null;

  const pump = async (): Promise<void> => {
    if (inflight) return;
    inflight = true;
    try {
      // A resize drag settles several times over; each new render supersedes the one being
      // recognized, so the loop re-checks rather than queueing an OCR pass per twitch.
      while (!done && latest) {
        const next = latest;
        latest = null;
        // `trial`: this read exists only to re-locate word boxes, and its text is thrown away.
        // Letting it run the furniture rule would be wrong twice over - a resize repaginates, so
        // its page fingerprint differs from the read being narrated and the same heading would
        // count as seen on a second page, and a heading dropped here has no boxes for the
        // highlight to relocate onto.
        const result = await recognizePage(next.bytes, { trial: true });
        if (done || latest) continue;
        mark.reflow({ src: next.src, natural: next.natural, words: result.words, inverted: result.inverted });
      }
    } catch (e) {
      // The mark stays hidden until the next render lands. Never let this take down the page
      // loop - the audio is fine, and a missing highlight is not worth stopping narration for.
      console.warn('[kwr] re-OCR after a re-render failed; the mark stays hidden:', String(e));
    } finally {
      inflight = false;
    }
  };

  const off = capture.onPageChange((p) => {
    latest = p;
    void pump();
  });

  return () => {
    done = true;
    off();
  };
}

const api = {
  ...capture,
  ocr,
  narrate,

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
  async readPage(): Promise<ocr.OcrResult> {
    const { result } = await scanPage();
    console.log(result.text.slice(0, 400) + (result.text.length > 400 ? '\n...' : ''));
    return result;
  },

  /** The whole loop for one page: capture -> OCR -> speak, marking each word as it is said. */
  async speakPage(options?: Parameters<typeof narrate.narrate>[1]): Promise<void> {
    // Thrown, never returned. `readBook` calls this per page and turns the page after it, so a
    // quiet return here is not "this page was skipped" - it is the reader flipping through the
    // whole book in silence while the panel still says "Reading…", losing the place it started
    // from and reporting "Finished." at the end of it. The one case that reliably produces it is
    // the host answering with an empty voice list (no model, or a voices dir that never
    // downloaded), which is precisely when the reader needs to be told.
    const voices = await narrate.checkVoices();
    if (!voices.ok) {
      throw new Error(`no usable voice. ${voices.advice ?? narrate.engineError() ?? ''}`.trim());
    }

    const page = await capture.capture(await capture.waitForSettled());
    const mark = (highlight ??= mountHighlight());
    const mine = ++markOwner;
    const unfollow = followReflow(mark);

    try {
      // charIndex -> the OCR word -> its bbox -> a screen rect. The offsets are against the
      // page's assembled text, which is what `words[].charStart` indexes into.
      await narrate.narrateStream(readColumns(page, mark, () => mine === markOwner), options, (charIndex) => {
        if (mine === markOwner) mark.at(charIndex);
      });
    } finally {
      // Whatever ended the page - finished, stopped, or a synthesis error - the mark must not
      // be left sitting on the last word it reached, and the watcher must not outlive it.
      //
      // Guarded by identity: a Stop followed straight away by Play starts the next page while
      // this one is still unwinding, and clearing then would blank the page that replaced it.
      unfollow();
      if (mine === markOwner) mark.page(null);
    }
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
      // A turn fired by a loop that has since been stopped can still land - `dispatchEvent` is
      // synchronous, the reader's render is not. Absorbing it here is what stops a Stop-then-Play
      // inside that beat from reading the page that is about to be swapped and then advancing off
      // the one it was swapped to, which would leave that page unread.
      await capture.settleTurn(() => cancelled);

      while (!cancelled) {
        await api.speakPage(options);
        if (cancelled) break;

        // Auto-advance. `turnPage` reports only what it can prove - a new page - so a false here
        // is "nothing moved", which is the last page of the book and a reader that has stopped
        // answering the arrow keys, indistinguishably. Both want the same thing: stay in the loop,
        // say so, and let a turn by hand carry on from wherever the reader actually is.
        if (await capture.turnPage({ cancelled: () => cancelled })) {
          const pos = capture.position();
          console.log(`[kwr] page turned${pos.page ? ` - now page ${pos.page}` : ''}`);
          continue;
        }
        if (cancelled) break;

        console.log('[kwr] could not turn the page - end of the book? Turn it yourself to continue.');
        panel?.status('Waiting for a page turn - turn the page to continue.');
        await new Promise<void>((resolve) => {
          const done = () => {
            off();
            wake = null;
            resolve();
          };
          const off = capture.onPageChange(done);
          wake = done;
        });
        // The wait is also released by Stop, which has its own status to show.
        if (!cancelled) panel?.status('Reading…', 'busy');
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
    highlight?.clear();
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
        retune: (rate) => narrate.retune(rate),
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

/** Tear down the in-page UI. The highlight goes with the panel - both belong to an open book. */
function unmountUi(): void {
  panel?.destroy();
  panel = null;
  highlight?.destroy();
  highlight = null;
}

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
    unmountUi();
  }
});
