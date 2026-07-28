// Stage Tesseract's worker, wasm core, and language data into extension/vendor/.
//
// MV3 forbids remote code, and the whole project is meant to work offline, so nothing may be
// pulled from a CDN at runtime - tesseract.js's defaults do exactly that and must be overridden
// with the paths this script produces. See src/content/ocr.ts.
//
//   bun run vendor
//
// Language data is fetched once from the tessdata CDN and committed to vendor/ (gitignored by
// size; re-run on a fresh clone).

import { mkdir } from 'node:fs/promises';
import path from 'node:path';

const root = path.join(import.meta.dir, '..');
const vendor = path.join(root, 'vendor');
await mkdir(vendor, { recursive: true });

// --- worker + wasm core, copied out of node_modules -------------------------------------
// `simd-lstm` is the right core for this job: LSTM-only (we never use the legacy engine) and
// SIMD, which every browser we target has had for years. Its non-SIMD sibling is ~3x slower.
const copies: [string, string][] = [
  ['tesseract.js/dist/worker.min.js', 'tesseract-worker.js'],
  ['tesseract.js-core/tesseract-core-simd-lstm.wasm.js', 'tesseract-core-simd-lstm.wasm.js'],
];

for (const [from, to] of copies) {
  const src = Bun.file(path.join(root, 'node_modules', from));
  if (!(await src.exists())) throw new Error(`missing ${from} - run \`bun install\` first`);
  await Bun.write(path.join(vendor, to), src);
  console.log(`  ${to}  ${((await src.size) / 1024).toFixed(0)}kb`);
}

// --- language data ----------------------------------------------------------------------
// Three tiers exist. `fast` is integer-quantized and roughly 3-4x quicker than `best`; which
// one this project ships is decided by the Phase 0 measurements, not by taste, so the bench
// fetches all of them and the extension bundles the winner.
const TIERS = {
  fast: 'https://tessdata.projectnaptha.com/4.0.0_fast/eng.traineddata.gz',
  standard: 'https://tessdata.projectnaptha.com/4.0.0/eng.traineddata.gz',
  best: 'https://tessdata.projectnaptha.com/4.0.0_best/eng.traineddata.gz',
};

const want = process.argv.includes('--all') ? Object.keys(TIERS) : ['fast', 'standard'];

for (const tier of want as (keyof typeof TIERS)[]) {
  const dir = path.join(vendor, `tessdata-${tier}`);
  await mkdir(dir, { recursive: true });
  const out = path.join(dir, 'eng.traineddata.gz');

  if (await Bun.file(out).exists()) {
    console.log(`  tessdata-${tier}/eng.traineddata.gz  (cached)`);
    continue;
  }

  const res = await fetch(TIERS[tier]);
  if (!res.ok) throw new Error(`fetch ${tier} failed: ${res.status}`);
  const bytes = await res.arrayBuffer();
  await Bun.write(out, bytes);
  console.log(`  tessdata-${tier}/eng.traineddata.gz  ${(bytes.byteLength / 1024 / 1024).toFixed(1)}mb`);
}

console.log('vendored ->', path.relative(root, vendor));
