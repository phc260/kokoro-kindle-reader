// The OCR gate.
//
// Runs the real src/ocr/ over rendered book pages with known ground truth, across
// the matrix that produces SILENT failures: two-column vs single, light vs dark, default vs
// non-default font. Reports word error rate and per-page time.
//
//   bun run build
//   $env:KWR_PAIRING = 'kwr_8787_...'   # tray -> "Web pairing code"
//   bun run bench:ocr
//
// Recognition happens on the HOST now, so this needs kokoro-host running and paired. The page
// is served from localhost, not from an extension origin, so the host must be started with
// that origin allowed:
//
//   $env:KOKORO_ALLOWED_ORIGINS = 'http://localhost:<the port this prints>'
//
// Caveat that matters: these are pages this repo renders, not pages Amazon renders. They are
// clean synthetic text at the measured resolution, so the numbers here are an OPTIMISTIC bound
// - real captures carry the reader's antialiasing, hinting, and compression. Treat a failure
// here as fatal and a pass here as "proceed to real captures", never as the gate itself.

import path from 'node:path';
import { existsSync } from 'node:fs';
import puppeteer from 'puppeteer-core';

const here = import.meta.dir;
const dist = path.join(here, '..', 'dist', 'chrome');

if (!existsSync(path.join(dist, 'content.js'))) {
  console.error('missing dist/chrome/content.js (run `bun run build`)');
  process.exit(1);
}

// The pairing is required, not optional with a default: a bench that silently measured a
// different backend than the one under test would be worse than no bench.
const pairing = /^kwr_(\d{1,5})_([0-9a-f]{32,128})$/.exec((process.env.KWR_PAIRING ?? '').trim());
if (!pairing) {
  console.error('set KWR_PAIRING to the code from the tray menu ("Web pairing code")');
  process.exit(1);
}
const backend = { base: `http://127.0.0.1:${pairing[1]}`, token: pairing[2]! };

function findBrowser(): string {
  if (process.env.CHROME_PATH) return process.env.CHROME_PATH;
  const candidates =
    process.platform === 'win32'
      ? [
          'C:/Program Files/Google/Chrome/Application/chrome.exe',
          'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
          'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
        ]
      : ['/usr/bin/google-chrome', '/usr/bin/chromium', '/usr/bin/chromium-browser', '/snap/bin/chromium'];
  const hit = candidates.find((p) => existsSync(p));
  if (!hit) {
    console.error('no Chrome/Chromium found; set CHROME_PATH');
    process.exit(1);
  }
  return hit;
}

const server = Bun.serve({
  port: 0,
  async fetch(req) {
    const rel = new URL(req.url).pathname;
    const file = rel === '/content.js' ? path.join(dist, 'content.js') : path.join(here, rel === '/' ? 'ocr-fixture.html' : rel);
    const f = Bun.file(file);
    return (await f.exists()) ? new Response(f) : new Response('not found', { status: 404 });
  },
});

const browser = await puppeteer.launch({ executablePath: findBrowser(), headless: true, args: ['--no-sandbox'] });
const page = await browser.newPage();
page.on('pageerror', (e) => console.error('   page error:', e.message));
await page.goto(`http://localhost:${server.port}/ocr-fixture.html`, { waitUntil: 'load' });
await page.waitForFunction('window.benchReady === true', { timeout: 10_000 });

const CASES = [
  { name: 'single column, light, serif', opts: { columns: 1, dark: false } },
  { name: 'two column,    light, serif', opts: { columns: 2, dark: false } },
  { name: 'single column, DARK,  serif', opts: { columns: 1, dark: true } },
  { name: 'two column,    DARK,  serif', opts: { columns: 2, dark: true } },
  { name: 'single column, light, sans ', opts: { columns: 1, dark: false, font: 'Verdana, sans-serif' } },
  { name: 'copyright footer + folio  ', opts: { columns: 1, dark: false, footer: true } },
];

console.log(`serving on http://localhost:${server.port} - the host must allow that origin`);
console.log('warming the host engine (the two models load on the first page)...');

const rows = await page.evaluate(async (cases, backend) => {
  const ocr = window.kwr.ocr;

  // --- word error rate against ground truth
  const norm = (s: string) =>
    s.toLowerCase().replace(/[^a-z0-9'\s]/g, ' ').replace(/\s+/g, ' ').trim().split(' ').filter(Boolean);

  function wer(truth: string[], got: string[]): number {
    const d: number[][] = Array.from({ length: truth.length + 1 }, () => new Array(got.length + 1).fill(0));
    for (let i = 0; i <= truth.length; i++) d[i]![0] = i;
    for (let j = 0; j <= got.length; j++) d[0]![j] = j;
    for (let i = 1; i <= truth.length; i++)
      for (let j = 1; j <= got.length; j++)
        d[i]![j] = Math.min(
          d[i - 1]![j]! + 1,
          d[i]![j - 1]! + 1,
          d[i - 1]![j - 1]! + (truth[i - 1] === got[j - 1] ? 0 : 1),
        );
    return d[truth.length]![got.length]! / truth.length;
  }

  const truth = norm(window.GROUND_TRUTH);
  const out: any[] = [];

  // The first call pays for the host building both sessions; run one throwaway so the reported
  // times are steady-state.
  await ocr.recognize(await window.renderPage({ columns: 1 }), backend);

  for (const c of cases) {
    const blob = await window.renderPage(c.opts);
    const t0 = performance.now();
    const r = await ocr.recognize(blob, backend);
    const wall = performance.now() - t0;
    const got = norm(r.text);
    out.push({
      name: c.name,
      wer: wer(truth, got),
      columns: r.columns,
      inverted: r.inverted,
      furniture: r.furnitureDropped,
      dropped: r.furniture.map((f) => `${f.text} [${f.reason}]`),
      words: r.words.length,
      conf: r.meanConfidence,
      preprocessMs: r.timing.preprocessMs,
      totalMs: wall,
      sample: r.text.slice(0, 90).replace(/\n/g, ' '),
    });
  }
  return out;
}, CASES, backend);

await browser.close();
await server.stop(true);

// --- report -----------------------------------------------------------------------------
const BUDGET_MS = 2000;
const WER_LIMIT = 0.02;

console.log(
  '\n' +
    'case'.padEnd(30) +
    'WER'.padStart(7) +
    'cols'.padStart(6) +
    'inv'.padStart(5) +
    'furn'.padStart(6) +
    'conf'.padStart(7) +
    'prep'.padStart(8) +
    'total'.padStart(9),
);
console.log('-'.repeat(78));

let failed = 0;
for (const r of rows) {
  const ok = r.wer <= WER_LIMIT && r.totalMs <= BUDGET_MS;
  if (!ok) failed++;
  console.log(
    r.name.padEnd(30) +
      `${(r.wer * 100).toFixed(1)}%`.padStart(7) +
      String(r.columns).padStart(6) +
      (r.inverted ? 'yes' : 'no').padStart(5) +
      String(r.furniture).padStart(6) +
      r.conf.toFixed(0).padStart(7) +
      `${r.preprocessMs.toFixed(0)}ms`.padStart(8) +
      `${r.totalMs.toFixed(0)}ms`.padStart(9) +
      (ok ? '' : '   <-- FAIL'),
  );
}

console.log('\nsamples:');
for (const r of rows) console.log(`  ${r.name}: "${r.sample}..."`);

// Dropping body text is a silent failure - an accuracy check cannot see it, because the words
// it removed were correct. Print every drop so a wrong one is obvious on sight.
console.log('\nfurniture dropped (must contain no body text):');
for (const r of rows) console.log(`  ${r.name}: ${r.dropped.join(' | ') || '(none)'}`);

console.log(`\ngate: WER <= ${WER_LIMIT * 100}% and <= ${BUDGET_MS}ms per page`);
console.log(failed ? `${failed}/${rows.length} cases FAILED` : `all ${rows.length} cases passed`);
process.exit(failed ? 1 : 0);
