// ALL Amazon DOM assumptions live in this file. Nothing outside it may assume anything
// about how read.amazon.com is structured. See docs/kindle-web-reader-internals.md for the
// measurements this is built on, and CLAUDE.md for why it is contained this way.
//
// The one-paragraph version: the reader puts NO book text in the DOM. Each page is rendered
// off-screen and shown as a single <img> whose src is a same-origin blob: URL with a fresh
// UUID per render. There are no iframes but ~37 shadow hosts, so every walk must pierce
// shadowRoot. Discovery is structural - never by selector, id, or class name.

export interface Route {
  onReader: boolean;
  asin: string | null;
  href: string;
}

export interface Candidate {
  img: HTMLImageElement;
  src: string;
  cssArea: number;
  visibleArea: number;
  natural: { w: number; h: number };
}

export interface PageImage {
  /** The blob: URL this page was captured from. Doubles as the page's identity. */
  src: string;
  bytes: Blob;
  natural: { w: number; h: number };
  css: { w: number; h: number };
  /** Read live - never assume the 1.5 from the internals doc; that was one display. */
  dpr: number;
  capturedAt: number;
}

export interface Position {
  page: number | null;
  ofPages: number | null;
  percent: number | null;
  /** The raw matched text, for debugging. Null when Reading Progress is switched off. */
  raw: string | null;
}

/** Below this edge length an image is chrome (icon, cover thumb), not a rendered page. */
const MIN_PAGE_EDGE = 200;
/** How often to re-check the page image identity. Cheap: one property read per tick. */
const POLL_MS = 250;
/** The blob URL must hold still this long before a page is considered settled. */
const SETTLE_MS = 120;

// ---------------------------------------------------------------------------- route

/**
 * The reader is an SPA at `read.amazon.com/?asin=<ASIN>` - a query param, NOT a `/reader`
 * path. Opening a book does not reload the document, so route changes must be observed
 * through the History API rather than waited for as navigations.
 */
export function route(): Route {
  const href = location.href;
  const onAmazonReader = /(^|\.)read\.amazon\.[a-z.]+$/.test(location.hostname);
  const asin = new URLSearchParams(location.search).get('asin');
  return { onReader: onAmazonReader && !!asin, asin, href };
}

/**
 * Fire `cb` whenever the SPA route changes. Patches pushState/replaceState (they emit no
 * event) and listens for popstate. Returns an unsubscribe that restores the originals.
 */
export function onRouteChange(cb: (r: Route) => void): () => void {
  const h = history as History & { pushState: History['pushState']; replaceState: History['replaceState'] };
  const origPush = h.pushState;
  const origReplace = h.replaceState;
  let last = location.href;

  const fire = () => {
    if (location.href === last) return;
    last = location.href;
    cb(route());
  };

  h.pushState = function (...args) {
    const r = origPush.apply(this, args as Parameters<History['pushState']>);
    queueMicrotask(fire);
    return r;
  };
  h.replaceState = function (...args) {
    const r = origReplace.apply(this, args as Parameters<History['replaceState']>);
    queueMicrotask(fire);
    return r;
  };
  addEventListener('popstate', fire);

  return () => {
    h.pushState = origPush;
    h.replaceState = origReplace;
    removeEventListener('popstate', fire);
  };
}

// ----------------------------------------------------------------------- shadow walk

/**
 * Depth-first walk of every element in the document, piercing shadow roots. A plain
 * `document.querySelectorAll` finds nothing useful here - the page image lives inside one
 * of ~37 shadow hosts.
 */
export function deepWalk(visit: (el: Element) => void, root: Document | ShadowRoot = document): void {
  const stack: Element[] = Array.from(root.children ?? []);
  while (stack.length) {
    const el = stack.pop()!;
    visit(el);
    if (el.shadowRoot) stack.push(...Array.from(el.shadowRoot.children));
    stack.push(...Array.from(el.children));
  }
}

/** Count shadow hosts. Purely diagnostic - the internals doc measured 37. */
export function shadowHostCount(): number {
  let n = 0;
  deepWalk((el) => {
    if (el.shadowRoot) n++;
  });
  return n;
}

// ------------------------------------------------------------------- image discovery

/**
 * Every blob:-backed <img> in the tree, scored. The reader may hold more than one (the
 * adjacent page can be pre-rendered off-screen), so the caller picks by score rather than
 * by "the first one found".
 */
export function candidates(): Candidate[] {
  const out: Candidate[] = [];
  const vw = innerWidth;
  const vh = innerHeight;

  deepWalk((el) => {
    if (!(el instanceof HTMLImageElement)) return;
    const src = el.currentSrc || el.src;
    if (!src.startsWith('blob:')) return;

    const r = el.getBoundingClientRect();
    if (r.width < MIN_PAGE_EDGE || r.height < MIN_PAGE_EDGE) return;

    // Intersection with the viewport, so an off-screen pre-render scores below the page
    // actually being displayed.
    const ix = Math.max(0, Math.min(r.right, vw) - Math.max(r.left, 0));
    const iy = Math.max(0, Math.min(r.bottom, vh) - Math.max(r.top, 0));

    out.push({
      img: el,
      src,
      cssArea: r.width * r.height,
      visibleArea: ix * iy,
      natural: { w: el.naturalWidth, h: el.naturalHeight },
    });
  });

  // Largest visible area wins; fall back to largest laid-out area when nothing intersects
  // the viewport (can happen while the tab is hidden or mid-transition).
  out.sort((a, b) => b.visibleArea - a.visibleArea || b.cssArea - a.cssArea);
  return out;
}

export function findPageImage(): HTMLImageElement | null {
  return candidates()[0]?.img ?? null;
}

/**
 * Is a book actually open and rendered here?
 *
 * Stricter than `route()` on purpose. The ASIN in the URL only says a book was *requested* -
 * the library, a loading state, and an expired session all keep it. Requiring a rendered page
 * image means the UI only appears when there is something to read.
 */
export function isReaderActive(): boolean {
  return route().onReader && findPageImage() !== null;
}

/**
 * Call `cb(true)` when a book becomes readable and `cb(false)` when it stops being readable,
 * including across SPA navigations. Returns an unsubscribe.
 */
export function onReaderActive(cb: (active: boolean) => void): () => void {
  let last: boolean | null = null;

  const tick = () => {
    const now = isReaderActive();
    if (now === last) return;
    last = now;
    cb(now);
  };

  tick();
  const timer = setInterval(tick, 500);
  const offRoute = onRouteChange(tick);

  return () => {
    clearInterval(timer);
    offRoute();
  };
}

// ---------------------------------------------------------------------------- capture

/**
 * Read the page image's bytes at full natural resolution.
 *
 * **Draw the element; do not refetch its URL.** The reader revokes each blob: URL immediately
 * after the <img> has loaded it - the element keeps showing the decoded bitmap, but the URL is
 * dead, so `fetch(currentSrc)` returns ERR_FILE_NOT_FOUND. Measured on a live book; an earlier
 * probe that fetched successfully must have caught the URL inside its brief lifetime.
 *
 * Drawing works because the blob is same-origin, so the canvas is not tainted (verified
 * READABLE - see docs/kindle-web-reader-internals.md). Sizing the canvas to
 * naturalWidth/naturalHeight also keeps full resolution rather than the CSS-scaled compositing
 * result, and - unlike `chrome.tabs.captureVisibleTab` - it works with the tab backgrounded.
 *
 * The fetch paths remain as fallbacks for the case where the URL is still alive but the canvas
 * refuses.
 */
export async function capture(img?: HTMLImageElement): Promise<PageImage> {
  const el = img ?? findPageImage();
  if (!el) throw new Error('capture: no blob: page image found');
  const src = el.currentSrc || el.src;

  const bytes = await readPixels(el, src);
  const r = el.getBoundingClientRect();

  return {
    src,
    bytes,
    natural: { w: el.naturalWidth, h: el.naturalHeight },
    css: { w: r.width, h: r.height },
    dpr: devicePixelRatio,
    capturedAt: Date.now(),
  };
}

let mainWorldReady: Promise<void> | null = null;

/** Draw the decoded element into a canvas. Independent of the blob URL still being alive. */
async function canvasBytes(el: HTMLImageElement): Promise<Blob> {
  const w = el.naturalWidth;
  const h = el.naturalHeight;
  if (!w || !h) throw new Error('image has no decoded dimensions yet');

  const canvas = new OffscreenCanvas(w, h);
  const ctx = canvas.getContext('2d');
  if (!ctx) throw new Error('no 2d context');
  ctx.drawImage(el, 0, 0);
  // Throws SecurityError if the canvas is tainted, which would mean the same-origin assumption
  // has stopped holding - a finding worth surfacing rather than swallowing.
  return await canvas.convertToBlob({ type: 'image/png' });
}

async function readPixels(el: HTMLImageElement, src: string): Promise<Blob> {
  const errors: string[] = [];

  try {
    return await canvasBytes(el);
  } catch (e) {
    errors.push(`canvas=${e}`);
  }

  // Only reachable while the URL is still alive; kept because it is cheaper than an encode and
  // returns the reader's own bytes untouched.
  try {
    const res = await fetch(src);
    if (!res.ok) throw new Error(`status ${res.status}`);
    return await res.blob();
  } catch (e) {
    errors.push(`fetch=${e}`);
  }

  try {
    return await captureViaPageWorld(src);
  } catch (e) {
    errors.push(`pageWorld=${e}`);
  }

  throw new Error(`capture failed: ${errors.join('; ')}`);
}

/**
 * Fallback path: ask a script running in the page's own world to fetch the blob and post
 * it back. Blobs are structured-cloneable, so the bytes cross the boundary intact.
 */
function captureViaPageWorld(src: string): Promise<Blob> {
  if (!mainWorldReady) {
    mainWorldReady = new Promise<void>((resolve, reject) => {
      const s = document.createElement('script');
      s.src = chrome.runtime.getURL('main-world.js');
      s.onload = () => {
        s.remove();
        resolve();
      };
      s.onerror = () => reject(new Error('main-world.js failed to load'));
      (document.head || document.documentElement).appendChild(s);
    });
  }

  const id = `kwr-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  return mainWorldReady.then(
    () =>
      new Promise<Blob>((resolve, reject) => {
        const timer = setTimeout(() => {
          removeEventListener('message', onMsg);
          reject(new Error('main-world capture timed out'));
        }, 10_000);

        const onMsg = (e: MessageEvent) => {
          const d = e.data;
          if (e.source !== window || !d || d.__kwr !== 'capture-result' || d.id !== id) return;
          clearTimeout(timer);
          removeEventListener('message', onMsg);
          d.ok ? resolve(d.blob as Blob) : reject(new Error(String(d.error)));
        };

        addEventListener('message', onMsg);
        postMessage({ __kwr: 'capture-request', id, src }, location.origin);
      }),
  );
}

// -------------------------------------------------------------------------- readiness

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * Resolve once a page image is decoded and its blob URL has stopped changing. Guards
 * against capturing a half-swapped page mid-turn.
 */
export async function waitForSettled(timeoutMs = 8000): Promise<HTMLImageElement> {
  const deadline = Date.now() + timeoutMs;
  let lastSrc: string | null = null;
  let stableSince = 0;

  while (Date.now() < deadline) {
    const el = findPageImage();
    if (el) {
      const src = el.currentSrc || el.src;
      if (src !== lastSrc) {
        lastSrc = src;
        stableSince = Date.now();
      } else if (Date.now() - stableSince >= SETTLE_MS) {
        try {
          await el.decode();
        } catch {
          // A decode failure mid-swap is transient; fall through and re-check.
        }
        if ((el.currentSrc || el.src) === src && el.naturalWidth > 0) return el;
      }
    }
    await sleep(POLL_MS / 4);
  }
  throw new Error('waitForSettled: no stable page image within timeout');
}

// ------------------------------------------------------------------ page-turn detect

/**
 * Call `cb` each time the displayed page changes. Identity is the blob URL - each render
 * gets a fresh UUID, which is the primary and most reliable signal.
 *
 * The parsed page number is deliberately NOT used to detect the turn: the reader's Reading
 * Progress rows are user-disableable, so it may be absent entirely. It is only ever a
 * confirmation, attached here for the caller's benefit.
 */
export function onPageChange(cb: (page: PageImage, pos: Position) => void): () => void {
  let lastSrc: string | null = findPageImage()?.currentSrc ?? null;
  let busy = false;
  let stopped = false;

  const tick = async () => {
    if (busy || stopped) return;
    const el = findPageImage();
    const src = el ? el.currentSrc || el.src : null;
    if (!src || src === lastSrc) return;

    busy = true;
    try {
      const settled = await waitForSettled();
      const settledSrc = settled.currentSrc || settled.src;
      if (settledSrc !== lastSrc) {
        lastSrc = settledSrc;
        cb(await capture(settled), position());
      }
    } catch {
      // Leave lastSrc alone so the next tick retries rather than silently skipping a page.
    } finally {
      busy = false;
    }
  };

  const timer = setInterval(tick, POLL_MS);
  // A page turn is a DOM mutation too; this just makes the response feel immediate rather
  // than waiting out the poll interval.
  const mo = new MutationObserver(() => void tick());
  mo.observe(document.documentElement, { subtree: true, childList: true, attributes: true, attributeFilter: ['src'] });

  return () => {
    stopped = true;
    clearInterval(timer);
    mo.disconnect();
  };
}

// --------------------------------------------------------------------------- position

const PAGE_OF = /Page\s+(\d+)\s+of\s+(\d+)/i;
const PERCENT = /(\d{1,3})\s*%/;

/**
 * Best-effort read of the reader's own progress text ("Page 364 of 943 * 36%"). This is
 * real DOM text, but the user can switch Reading Progress off, so every field is nullable
 * and callers must treat absence as normal - never as an error.
 */
export function position(): Position {
  const out: Position = { page: null, ofPages: null, percent: null, raw: null };

  deepWalk((el) => {
    if (out.page !== null && out.percent !== null) return;
    if (el.children.length) return; // leaf nodes only - avoids re-scanning whole subtrees
    const t = el.textContent;
    if (!t || t.length > 120) return;

    const m = PAGE_OF.exec(t);
    if (m && out.page === null) {
      out.page = Number(m[1]);
      out.ofPages = Number(m[2]);
      out.raw = t.trim();
    }
    const p = PERCENT.exec(t);
    if (p && out.percent === null) {
      const v = Number(p[1]);
      if (v <= 100) {
        out.percent = v;
        out.raw ??= t.trim();
      }
    }
  });

  return out;
}

// -------------------------------------------------------------------------- selftest

/**
 * Dump everything this module believes about the current tab. First thing to run when
 * capture breaks: it says which assumption stopped holding.
 */
export async function selftest(): Promise<Record<string, unknown>> {
  const r = route();
  const cands = candidates();
  const hosts = shadowHostCount();

  let captured: Record<string, unknown> | { error: string };
  try {
    const p = await capture(await waitForSettled());
    captured = {
      src: p.src.slice(0, 64),
      bytes: p.bytes.size,
      type: p.bytes.type,
      natural: p.natural,
      css: p.css,
      dpr: p.dpr,
    };
  } catch (e) {
    captured = { error: String(e) };
  }

  return {
    route: r,
    shadowHosts: hosts,
    iframes: document.querySelectorAll('iframe').length,
    bodyInnerTextChars: document.body.innerText.length,
    candidates: cands.map((c) => ({
      src: c.src.slice(0, 48),
      cssArea: Math.round(c.cssArea),
      visibleArea: Math.round(c.visibleArea),
      natural: c.natural,
    })),
    position: position(),
    capture: captured,
    expectations: {
      shadowHosts: '~37 (0 means the walk is not piercing shadowRoot)',
      iframes: '0 (write-ups claiming nested iframes are stale)',
      bodyInnerTextChars: '~217, all UI chrome (a large number would mean text IS in the DOM)',
    },
  };
}

/**
 * Save the current page image to disk, so real captures can be fed to the offline OCR
 * matrix (ROADMAP Phase 0). Run it once per cell of the grid: two-column and single,
 * white and black, default font and not.
 */
export async function dumpCapture(name?: string): Promise<string> {
  const p = await capture(await waitForSettled());
  const pos = position();
  const label =
    name ??
    ['kindle', route().asin ?? 'noasin', pos.page ? `p${pos.page}` : `t${Date.now()}`].join('-');
  const ext = (p.bytes.type.split('/')[1] || 'png').replace('jpeg', 'jpg');
  const filename = `${label}.${ext}`;

  const url = URL.createObjectURL(p.bytes);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 30_000);

  return `${filename} (${p.natural.w}x${p.natural.h}, ${p.bytes.size} bytes, dpr ${p.dpr})`;
}
