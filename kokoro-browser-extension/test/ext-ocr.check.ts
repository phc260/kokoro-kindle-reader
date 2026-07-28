// Does the OCR path actually work when loaded AS AN EXTENSION?
//
// The other tests load content.js as a plain page script, where everything shares one origin.
// That hides the question that decides whether readPage() works at all on read.amazon.com:
//
//   Tesseract spawns `new Worker(chrome-extension://<id>/vendor/tesseract-worker.js)` from a
//   content script whose document origin is Amazon's. Workers must be same-origin with the
//   document, so this may throw - and if it does, no amount of OCR quality matters.
//
// This loads the real built extension into Chrome, points it at a local page that mimics a
// rendered Kindle page, and drives the real bridge. A pass means the packaged extension can
// OCR; a failure names the fix (offscreen document).
//
//   bun run build && bun run test:ext
//
// REQUIRES a Chrome that still honours --load-extension. Chrome 137+ removed that switch for
// security, and neither --enable-unsafe-extension-debugging nor
// --disable-features=DisableLoadExtensionCommandLineSwitch restores it on 150. Point
// CHROME_PATH at a Chrome for Testing build:
//     bunx @puppeteer/browsers install chrome@stable
// Until then this SKIPS rather than fails, and the question is answered faster by running
// `await kwr.readPage()` on a live book.
//
// The manifest is copied to a temp dir with its match patterns retargeted at localhost -
// the shipped manifest is never modified.

import { cp, mkdtemp, rm } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import puppeteer from 'puppeteer-core';

const here = import.meta.dir;
const dist = path.join(here, '..', 'dist', 'chrome');

if (!existsSync(path.join(dist, 'content.js'))) {
  console.error('dist/chrome missing - run `bun run build` first');
  process.exit(1);
}

function findBrowser(): string {
  if (process.env.CHROME_PATH) return process.env.CHROME_PATH;
  const candidates =
    process.platform === 'win32'
      ? [
          'C:/Program Files/Google/Chrome/Application/chrome.exe',
          'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
        ]
      : ['/usr/bin/google-chrome', '/usr/bin/chromium', '/usr/bin/chromium-browser'];
  const hit = candidates.find((p) => existsSync(p));
  if (!hit) {
    console.error('no Chrome/Chromium found; set CHROME_PATH');
    process.exit(1);
  }
  return hit;
}

// --- serve a page shaped like a rendered reader page -------------------------------------
const server = Bun.serve({
  port: 0,
  async fetch(req) {
    const rel = new URL(req.url).pathname;
    if (rel === '/' || rel === '/index.html') {
      return new Response(PAGE, { headers: { 'content-type': 'text/html' } });
    }
    const f = Bun.file(path.join(here, rel));
    return (await f.exists()) ? new Response(f) : new Response('not found', { status: 404 });
  },
});
const origin = `http://localhost:${server.port}`;

// A shadow host holding one blob: <img> of rendered prose - enough for capture + OCR.
const PAGE = `<!doctype html><meta charset="utf-8"><title>ext ocr</title><body><div id="app"></div>
<script>
window.TRUTH = "Call me Ishmael. Some years ago never mind how long precisely having little or no money in my purse, and nothing particular to interest me on shore, I thought I would sail about a little and see the watery part of the world.";
(async () => {
  const c = document.createElement('canvas');
  c.width = 2388; c.height = 1681;
  const g = c.getContext('2d');
  g.fillStyle = '#fff'; g.fillRect(0,0,c.width,c.height);
  g.fillStyle = '#111'; g.font = '42px Georgia, serif'; g.textBaseline = 'top';
  let line = '', y = 150;
  for (const w of TRUTH.split(' ')) {
    const t = line ? line + ' ' + w : w;
    if (g.measureText(t).width > 2088 && line) { g.fillText(line, 150, y); y += 67; line = w; }
    else line = t;
  }
  if (line) g.fillText(line, 150, y);

  const blob = await new Promise(r => c.toBlob(r, 'image/png'));
  const host = document.createElement('div');
  document.getElementById('app').appendChild(host);
  const root = host.attachShadow({ mode: 'open' });
  const img = document.createElement('img');
  img.src = URL.createObjectURL(blob);
  img.style.cssText = 'width:796px;height:560px;display:block';
  root.appendChild(img);
  await img.decode();
  // Revoke immediately, exactly as the real reader does.
  URL.revokeObjectURL(img.src);
  window.ready = true;
})();
</script>`;

// --- copy the built extension, retarget its match patterns at localhost -------------------
const extDir = await mkdtemp(path.join(tmpdir(), 'kwr-ext-'));
await cp(dist, extDir, { recursive: true });
{
  const mf = path.join(extDir, 'manifest.json');
  const m = JSON.parse(await Bun.file(mf).text());
  // Chrome match patterns must NOT carry a port - the port is ignored and a pattern that
  // includes one is rejected outright, silently preventing the extension from loading.
  const pattern = 'http://localhost/*';
  for (const cs of m.content_scripts) cs.matches = [pattern];
  for (const war of m.web_accessible_resources ?? []) war.matches = [pattern];
  m.host_permissions = [pattern];
  await Bun.write(mf, JSON.stringify(m, null, 2));
}

const results: { name: string; pass: boolean; detail: string }[] = [];
const check = (name: string, pass: boolean, detail = '') => results.push({ name, pass, detail });

const browser = await puppeteer.launch({
  executablePath: findBrowser(),
  headless: true,
  args: [`--disable-extensions-except=${extDir}`, `--load-extension=${extDir}`, '--no-sandbox'],
});

try {
  const page = await browser.newPage();
  const consoleErrors: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error') consoleErrors.push(m.text());
  });
  page.on('pageerror', (e) => consoleErrors.push(e.message));

  await page.goto(origin, { waitUntil: 'load' });
  await page.waitForFunction('window.ready === true', { timeout: 15_000 });

  // The bridge is installed by net-probe.js in the page world; its presence proves the
  // extension actually loaded and injected.
  const bridged = await page
    .waitForFunction('typeof window.kwr?.readPage === "function"', { timeout: 10_000 })
    .then(() => true)
    .catch(() => false);
  if (!bridged) {
    console.log('SKIPPED - this Chrome did not load the unpacked extension.');
    console.log('  Chrome 137+ removed --load-extension; use a Chrome for Testing build via CHROME_PATH:');
    console.log('    bunx @puppeteer/browsers install chrome@stable');
    console.log('  Or answer it directly: run `await kwr.readPage()` on a live book.');
    await browser.close();
    server.stop(true);
    await rm(extDir, { recursive: true, force: true });
    process.exit(0);
  }
  check('extension injected and bridged `kwr` into the page world', bridged);

  {
    const out = await page.evaluate(async () => {
      try {
        const r = (await window.kwr.selftest()) as Record<string, unknown>;
        return { ok: true, selftest: r };
      } catch (e) {
        return { ok: false, error: String(e) };
      }
    });
    const cap = (out.selftest as { capture?: Record<string, unknown> } | undefined)?.capture;
    check('selftest() captures through the extension (revoked blob URL)', !!cap && !cap.error, JSON.stringify(cap ?? out).slice(0, 200));

    // The actual question.
    const ocr = await page.evaluate(async () => {
      const t0 = performance.now();
      try {
        const r = (await window.kwr.readPage()) as { text: string; words: unknown[]; columns: number };
        return { ok: true, ms: performance.now() - t0, chars: r.text.length, words: r.words.length, sample: r.text.slice(0, 80) };
      } catch (e) {
        return { ok: false, ms: performance.now() - t0, error: String(e) };
      }
    });

    check('readPage() runs OCR inside the extension', ocr.ok === true, ocr.ok ? '' : String(ocr.error));

    if (ocr.ok) {
      const truth = await page.evaluate(() => window.TRUTH as string);
      const norm = (s: string) => s.toLowerCase().replace(/[^a-z']+/g, ' ').trim();
      const got = norm(ocr.sample ?? '');
      check('OCR text matches the rendered prose', norm(truth).startsWith(got.slice(0, 40)), `got: "${ocr.sample}"`);
      console.log(`\n  OCR ran in ${Math.round(ocr.ms!)}ms, ${ocr.chars} chars, ${ocr.words} word boxes`);
    }

    const workerErr = consoleErrors.find((e) => /Worker|worker-src|Content Security Policy/i.test(e));
    check('no Worker/CSP error in the console', !workerErr, workerErr ?? '');
  }
} finally {
  await browser.close();
  server.stop(true);
  await rm(extDir, { recursive: true, force: true });
}

let failed = 0;
for (const { name, pass, detail } of results) {
  console.log(`${pass ? 'PASS' : 'FAIL'}  ${name}`);
  if (!pass) {
    console.log(`      ${detail}`);
    failed++;
  }
}
if (failed) {
  console.log('\nIf the Worker construction failed, the fix is to run Tesseract in an offscreen');
  console.log('document, where the origin is chrome-extension:// and Amazon\'s CSP does not apply.');
}
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
