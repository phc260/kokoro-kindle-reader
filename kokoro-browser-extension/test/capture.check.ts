// Exercises src/content/capture.ts against a fixture shaped like the measured reader:
// no iframes, deeply nested shadow hosts, the page as a single blob: <img>, decoys around
// it, progress text as real DOM text. See docs/kindle-web-reader-internals.md.
//
// This is not a substitute for running selftest() on a live book - Amazon changes the
// reader and the fixture cannot notice. What it does catch is regressions in the logic we
// own: shadow piercing, candidate scoring, natural-resolution capture, blob-identity page
// turns, and capture while the tab is hidden.
//
//   bun run build && bun run test:capture
//
// Requires a Chrome/Chromium/Edge on the machine. Set CHROME_PATH to override discovery.
//
// Named `.check.ts`, not `.test.ts`: this drives a browser and calls process.exit() when it is
// done, which under `bun test` would terminate the whole run and silently skip every file after
// it. `bun test` is for the pure `bun:test` files; these harnesses are run by name.

import { existsSync } from 'node:fs';
import path from 'node:path';
import puppeteer from 'puppeteer-core';

// `window.kwr` is declared by src/content/index.ts; only the fixture's own hooks are added
// here.
declare global {
  interface Window {
    revokePageBlob: () => void;
    __pageImg: HTMLImageElement;
    __preImg: HTMLImageElement;
    turnPage: () => Promise<string>;
    fixtureRerender: (w: number, h: number) => Promise<string>;
    fixtureTurn: boolean;
    fixtureFired: number;
    fixtureDelay: number;
    fixtureReady: boolean;
  }
}

const here = import.meta.dir;
const dist = path.join(here, '..', 'dist', 'chrome');

if (!existsSync(path.join(dist, 'content.js'))) {
  console.error('dist/chrome/content.js missing - run `bun run build` first');
  process.exit(1);
}

// --- browser discovery, both platforms -------------------------------------------------
function findBrowser(): string {
  if (process.env.CHROME_PATH) return process.env.CHROME_PATH;
  const candidates =
    process.platform === 'win32'
      ? [
          'C:/Program Files/Google/Chrome/Application/chrome.exe',
          'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
          'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
          'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
        ]
      : [
          '/usr/bin/google-chrome',
          '/usr/bin/chromium',
          '/usr/bin/chromium-browser',
          '/snap/bin/chromium',
          '/usr/bin/microsoft-edge',
        ];
  const hit = candidates.find((p) => existsSync(p));
  if (!hit) {
    console.error(`no Chrome/Chromium found. Tried:\n  ${candidates.join('\n  ')}\nSet CHROME_PATH.`);
    process.exit(1);
  }
  return hit;
}

// --- static server. Port 0 so a stray server from an earlier run can't collide. ---------
const server = Bun.serve({
  port: 0,
  async fetch(req) {
    const rel = new URL(req.url).pathname;
    const file = rel === '/content.js' ? path.join(dist, 'content.js') : path.join(here, rel === '/' ? 'fixture.html' : rel);
    const f = Bun.file(file);
    // The browser also asks for /favicon.ico; answer 404 rather than throwing ENOENT.
    return (await f.exists()) ? new Response(f) : new Response('not found', { status: 404 });
  },
});

// --- run -------------------------------------------------------------------------------
const results: { name: string; pass: boolean; detail: string }[] = [];
const check = (name: string, pass: boolean, detail: string) => results.push({ name, pass, detail });

const browser = await puppeteer.launch({ executablePath: findBrowser(), headless: true, args: ['--no-sandbox'] });
try {
  const page = await browser.newPage();
  await page.setViewport({ width: 1280, height: 900 });
  await page.goto(`http://localhost:${server.port}/fixture.html`, { waitUntil: 'load' });
  await page.waitForFunction('window.fixtureReady === true', { timeout: 10_000 });

  const r = await page.evaluate(async () => {
    const kwr = window.kwr;
    const chosen = kwr.findPageImage();
    const cands = kwr.candidates();
    const cap = await kwr.capture();
    return {
      lightDomImgs: document.querySelectorAll('img').length,
      shadowHosts: kwr.shadowHostCount(),
      candCount: cands.length,
      candNaturals: cands.map((c) => c.natural),
      chosenIsPage: chosen === window.__pageImg,
      chosenIsPreRender: chosen === window.__preImg,
      capture: { bytes: cap.bytes.size, type: cap.bytes.type, natural: cap.natural, dpr: cap.dpr },
      position: kwr.position(),
    };
  });

  check('light-DOM query finds no images (the walk must pierce shadowRoot)', r.lightDomImgs === 0, `querySelectorAll('img') = ${r.lightDomImgs}`);
  check('deepWalk finds every shadow host', r.shadowHosts === 16, `found ${r.shadowHosts}, expected 16`);
  check('small blob img (80px) rejected as UI chrome', r.candCount === 2, `candidates = ${r.candCount}, expected 2`);
  check('large non-blob img never a candidate', r.candNaturals.every((n) => n.w === 2388), JSON.stringify(r.candNaturals));
  check('picks the on-screen page over the off-screen pre-render', r.chosenIsPage && !r.chosenIsPreRender, `isPage=${r.chosenIsPage} isPreRender=${r.chosenIsPreRender}`);
  check('capture() reads the page bytes', r.capture.bytes > 1000 && r.capture.type === 'image/png', JSON.stringify(r.capture));
  check('capture is natural resolution, not CSS size', r.capture.natural.w === 2388 && r.capture.natural.h === 1681, JSON.stringify(r.capture.natural));
  check('devicePixelRatio read live, never assumed', typeof r.capture.dpr === 'number', `dpr=${r.capture.dpr}`);
  check('position parses "Page N of M" and percent', r.position.page === 364 && r.position.ofPages === 943 && r.position.percent === 36, JSON.stringify(r.position));

  const turn = await page.evaluate(async () => {
    const seen: { bytes: number; page: number | null }[] = [];
    const stop = window.kwr.onPageChange((p, pos) => seen.push({ bytes: p.bytes.size, page: pos.page }));
    await window.turnPage();
    await new Promise((res) => setTimeout(res, 1500));
    stop();
    return seen;
  });
  check('page turn detected by fresh blob URL', turn.length === 1, JSON.stringify(turn));
  check('turned page captured, position advanced', (turn[0]?.bytes ?? 0) > 1000 && turn[0]?.page === 365, JSON.stringify(turn));

  // Auto-advance: ArrowRight reaches the reader out of a shadow root, and - the case that matters -
  // a keypress that arrived and moved nothing is not reported as a turn.
  const advance = await page.evaluate(async () => {
    const src = () => window.__pageImg.currentSrc || window.__pageImg.src;
    const out: { want: string; got: boolean; moved: boolean; fired: number }[] = [];

    for (const [want, live] of [
      ['turns', true],
      ['none', false],
    ] as [string, boolean][]) {
      window.fixtureTurn = live;
      window.fixtureFired = 0;
      const before = src();
      const got = await window.kwr.turnPage({ waitMs: 400 });
      out.push({ want, got, moved: src() !== before, fired: window.fixtureFired });
    }
    return out;
  });

  const [turned, none] = advance as [(typeof advance)[0], (typeof advance)[0]];
  check('ArrowRight turns the page from inside the shadow tree', turned.got && turned.moved, JSON.stringify(turned));
  check('a keypress that moves nothing is not a turn', !none.got && !none.moved && none.fired === 1, JSON.stringify(none));

  // A resize re-renders the SAME page under a fresh blob URL. The reader does this on every resize
  // and zoom - counting it as a turn would have the loop OCR and narrate the page it just read.
  const reflow = await page.evaluate(async () => {
    window.fixtureTurn = false;
    const turning = window.kwr.turnPage({ waitMs: 500 });
    setTimeout(() => void window.fixtureRerender(1600, 1120), 120);
    const got = await turning;

    // Put the page back to the size the later checks assert, via a real turn this time.
    window.fixtureTurn = true;
    await window.turnPage();
    await window.__pageImg.decode();
    return { got, natural: { w: window.__pageImg.naturalWidth, h: window.__pageImg.naturalHeight } };
  });

  check('a re-render at a new size is not a page turn', !reflow.got, JSON.stringify(reflow));
  check('...and the fixture is back at page size for the checks below', reflow.natural.w === 2388, JSON.stringify(reflow.natural));

  // Stop cannot unsend an action. A turn dispatched by a loop that is then stopped lands anyway,
  // and a Play inside that beat would read the page about to be swapped - then advance off the one
  // it was swapped to, leaving it unread. `settleTurn` is what the next loop waits on.
  const pendingTurn = await page.evaluate(async () => {
    window.fixtureTurn = true;
    window.fixtureDelay = 300; // the reader renders the turn well after the dispatch returns
    const src = window.__pageImg.currentSrc || window.__pageImg.src;

    let stop = false;
    const turning = window.kwr.turnPage({ cancelled: () => stop });
    setTimeout(() => (stop = true), 60); // Stop, after the action is already out
    const got = await turning;
    const atStop = window.__pageImg.currentSrc || window.__pageImg.src;

    const settled = await window.kwr.settleTurn();
    window.fixtureDelay = 0;
    return { got, landedBeforeStop: atStop !== src, settled, moved: (window.__pageImg.currentSrc || window.__pageImg.src) !== src };
  });

  check(
    'a stopped turn is reported as no turn',
    !pendingTurn.got && !pendingTurn.landedBeforeStop,
    JSON.stringify(pendingTurn),
  );
  check('...and the next reader waits it out rather than reading the page it replaces', pendingTurn.settled && pendingTurn.moved, JSON.stringify(pendingTurn));

  // The caller captures and OCRs whatever is on screen when the turn is handed back, so what it
  // must be handed is the render that STAYED. A second one landing right behind the first - the
  // reader re-laying out, or the real turn arriving behind a re-render mistaken for it - has to be
  // the one the caller sees, or it narrates a page that is already gone.
  const settles = await page.evaluate(async () => {
    window.fixtureTurn = true;
    let second: string | null = null;
    const turning = window.kwr.turnPage({ waitMs: 1000 });
    setTimeout(() => void window.turnPage().then((s) => (second = s)), 60);
    const got = await turning;
    const showing = window.__pageImg.currentSrc || window.__pageImg.src;
    return { got, second: Boolean(second), showingIsLatest: showing === second };
  });

  check(
    'a turn is handed back only once the page holds still',
    settles.got && settles.second && settles.showingIsLatest,
    JSON.stringify(settles),
  );

  // A wait cut short by a Stop leaves the turn pending - for the reader after next, too. Consuming
  // it on the first Stop would let a Stop-Play-Stop-Play sequence start a reader on a page that is
  // about to be swapped, with nothing left to say so.
  const twoStops = await page.evaluate(async () => {
    window.fixtureTurn = true;
    window.fixtureDelay = 400;
    const src = window.__pageImg.currentSrc || window.__pageImg.src;

    let stopA = false;
    const a = window.kwr.turnPage({ cancelled: () => stopA });
    setTimeout(() => (stopA = true), 50);
    const first = await a;

    let stopB = false;
    const b = window.kwr.settleTurn(() => stopB);
    setTimeout(() => (stopB = true), 60);
    const second = await b;

    const third = await window.kwr.settleTurn();
    window.fixtureDelay = 0;
    return { first, second, third, moved: (window.__pageImg.currentSrc || window.__pageImg.src) !== src };
  });

  check(
    'a settle cut short by a second Stop does not consume the pending turn',
    !twoStops.first && !twoStops.second && twoStops.third && twoStops.moved,
    JSON.stringify(twoStops),
  );

  // ...but a turn watched to the end of its own budget is not pending, whatever budget that was.
  // Tying it to the constant instead would park the next reader for the remainder.
  const noWait = await page.evaluate(async () => {
    window.fixtureTurn = false;
    await window.kwr.turnPage({ waitMs: 200 });
    const at = Date.now();
    const settled = await window.kwr.settleTurn();
    window.fixtureTurn = true;
    return { settled, ms: Date.now() - at };
  });

  check('a turn that failed on a short budget leaves nothing pending', !noWait.settled && noWait.ms < 100, JSON.stringify(noWait));

  // The live reader revokes the blob URL as soon as the <img> has it, so fetching currentSrc
  // gives ERR_FILE_NOT_FOUND. Capture must read the decoded element instead.
  const revoked = await page.evaluate(async () => {
    window.revokePageBlob();
    const fetchFailed = await fetch(window.__pageImg.currentSrc).then(() => false).catch(() => true);
    const cap = await window.kwr.capture();
    return { fetchFailed, bytes: cap.bytes.size, natural: cap.natural };
  });
  check('capture survives a revoked blob URL', revoked.fetchFailed && revoked.bytes > 1000, JSON.stringify(revoked));
  check('...still at natural resolution', revoked.natural.w === 2388 && revoked.natural.h === 1681, JSON.stringify(revoked.natural));

  // The reason capture uses the decoded element rather than chrome.tabs.captureVisibleTab.
  await page.evaluate(() => Object.defineProperty(document, 'hidden', { value: true, configurable: true }));
  const hidden = await page.evaluate(async () => ({ hidden: document.hidden, bytes: (await window.kwr.capture()).bytes.size }));
  check('capture still works with the tab hidden', hidden.hidden === true && hidden.bytes > 1000, JSON.stringify(hidden));

  const st = await page.evaluate(async () => await window.kwr.selftest());
  const cap = st.capture as Record<string, unknown>;
  check('selftest() runs end to end', !cap.error && st.iframes === 0, JSON.stringify({ iframes: st.iframes, capture: cap }).slice(0, 200));
} finally {
  await browser.close();
  await server.stop(true);
}

let failed = 0;
for (const { name, pass, detail } of results) {
  console.log(`${pass ? 'PASS' : 'FAIL'}  ${name}`);
  if (!pass) {
    console.log(`      ${detail}`);
    failed++;
  }
}
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
