// Bun.build -> dist/chrome and dist/firefox. Two entry points for now (content script + the
// main-world fallback); background/options join them in later phases.
//
// Bun's bundler rather than esbuild: one fewer dependency, and Bun is already the runtime.
// `format: "iife"` matters - a content script is a classic script, so ESM output would fail
// on the first `export`.
//
// Runs the same on Windows and Linux - paths go through node:path, never string concat.

import { cp, mkdir, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';

const root = import.meta.dir;
const dist = path.join(root, 'dist');

const targets = [
  { name: 'chrome', manifest: 'manifest.chrome.json' },
  { name: 'firefox', manifest: 'manifest.firefox.json' },
];

/** entry file -> emitted name. The emitted names are what the manifests reference. */
const entries: Record<string, string> = {
  'content.js': 'src/content/index.ts',
  'main-world.js': 'src/content/main-world.ts',
  'background.js': 'src/background.ts',
  'net-probe.js': 'src/content/net-probe.ts',
  'offscreen.js': 'src/offscreen.ts',
  'options.js': 'src/options.ts',
};

/** Firefox has neither chrome.tts nor chrome.offscreen, so it ships neither piece. */
const skip: Record<string, string[]> = { firefox: ['background.js', 'offscreen.js'] };

await rm(dist, { recursive: true, force: true });

for (const t of targets) {
  const outdir = path.join(dist, t.name);
  await mkdir(outdir, { recursive: true });

  for (const [outName, entry] of Object.entries(entries)) {
    if (skip[t.name]?.includes(outName)) continue;
    const result = await Bun.build({
      entrypoints: [path.join(root, entry)],
      outdir,
      format: 'iife',
      target: 'browser',
      sourcemap: 'inline',
      naming: { entry: outName },
    });

    if (!result.success) {
      for (const log of result.logs) console.error(log);
      throw new Error(`build failed: ${entry}`);
    }

    const bytes = result.outputs[0]?.size ?? 0;
    console.log(`  ${t.name}/${outName}  ${(bytes / 1024).toFixed(1)}kb`);
  }

  const manifest = await Bun.file(path.join(root, t.manifest)).json();
  await Bun.write(path.join(outdir, 'manifest.json'), JSON.stringify(manifest, null, 2));

  // Nothing to stage for OCR. Recognition is `POST /ocr` on the host, so the package carries
  // no engine, no wasm and no language data - the ~17 MiB `vendor/` copy this used to make is
  // the whole reason the migration was worth doing.
  if (!skip[t.name]?.includes('offscreen.js')) {
    await cp(path.join(root, 'offscreen.html'), path.join(outdir, 'offscreen.html'));
  }
  await cp(path.join(root, 'options.html'), path.join(outdir, 'options.html'));

  // The extension shares the project's icons rather than carrying its own copies - same source
  // the exes, the tray and the installer use, so the browser's toolbar and Kindle's tray can
  // never show different art. They live at the REPO ROOT (and are Git LFS), one level up.
  for (const icon of ['32x32.png', '128x128.png']) {
    await cp(path.join(root, '..', 'icons', icon), path.join(outdir, 'icons', icon));
  }

  console.log(`built ${t.name} -> ${path.relative(root, outdir)}`);
}

// --- stage to a local disk -----------------------------------------------------------------
// Chrome loads unpacked extensions poorly from removable or network drives - the symptom is a
// reload that spins forever with no error in the card. Copy the Chrome build somewhere local
// and load THAT folder instead; the manifest `key` keeps the extension id stable across the
// move.
//
// Defaults under the home directory rather than a hardcoded path, so this works on someone
// else's machine and on Linux.
const stage =
  process.env.KWR_STAGE_DIR ||
  (process.argv.includes('--stage') ? path.join(os.homedir(), 'kokoro-ext') : '');
if (stage) {
  await rm(stage, { recursive: true, force: true });
  await cp(path.join(dist, 'chrome'), stage, { recursive: true });
  console.log(`staged chrome build -> ${stage}`);
}
