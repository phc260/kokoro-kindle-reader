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
