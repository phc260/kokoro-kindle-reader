import { copyFileSync, createReadStream } from "fs";
import { resolve } from "path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const host = process.env.TAURI_DEV_HOST;

// ONNX Runtime Web (used by @huggingface/transformers under kokoro-js)
// dynamically imports these files at runtime. Vite forbids importing files from
// public/ as ES modules, so we serve them via a dev-server middleware (from
// node_modules) and copy them to dist/ on build. The `.jsep.*` variants provide
// the WebGPU/WebNN execution provider. The TTS worker sets `wasmPaths = "/"`.
const ORT_MJS = ["ort-wasm-simd-threaded.mjs", "ort-wasm-simd-threaded.jsep.mjs"];
const ORT_WASM = ["ort-wasm-simd-threaded.wasm", "ort-wasm-simd-threaded.jsep.wasm"];

const ortMjsPlugin = {
  name: "ort-mjs",
  configureServer(server: any) {
    server.middlewares.use((req: any, res: any, next: any) => {
      const pathname = req.url?.split("?")[0];
      const mjs = ORT_MJS.find((f) => pathname === `/${f}`);
      if (mjs) {
        res.setHeader("Content-Type", "application/javascript");
        createReadStream(resolve(`node_modules/onnxruntime-web/dist/${mjs}`)).pipe(res);
        return;
      }
      const wasm = ORT_WASM.find((f) => pathname === `/${f}`);
      if (wasm) {
        res.setHeader("Content-Type", "application/wasm");
        createReadStream(resolve(`node_modules/onnxruntime-web/dist/${wasm}`)).pipe(res);
        return;
      }
      next();
    });
  },
  closeBundle() {
    for (const f of [...ORT_MJS, ...ORT_WASM]) {
      copyFileSync(`node_modules/onnxruntime-web/dist/${f}`, `dist/${f}`);
    }
  },
};

// ONNX Runtime's emscripten glue (bundled into the worker via
// @huggingface/transformers) carries a *fallback* wasm locator:
//   new URL("ort-wasm-simd-threaded.jsep.wasm", import.meta.url)
// Vite treats `new URL(<literal>, import.meta.url)` as an asset reference and
// emits the 21 MB wasm into dist/assets/ — but at runtime ORT's locateFile is
// overridden by `wasmPaths = "/"` (see tts.worker.ts), so that emitted copy is
// never fetched; the real wasm is the one ortMjsPlugin copies to the site root.
// Rewrite the fallback to that same root path so Vite stops emitting the dead
// duplicate. Must run `pre`, before Vite's built-in asset/url plugin.
const ortDropDeadWasmPlugin = {
  name: "ort-drop-dead-wasm",
  enforce: "pre" as const,
  transform(code: string, id: string) {
    if (!id.includes("onnxruntime-web") && !id.includes("@huggingface/transformers")) return;
    if (!code.includes('new URL("ort-wasm-simd-threaded')) return null;
    const out = code.replace(
      /new URL\((["'])(ort-wasm-simd-threaded[^"']*\.wasm)\1,\s*import\.meta\.url\)/g,
      (_m, _q, file) => JSON.stringify("/" + file),
    );
    return out === code ? null : { code: out, map: null };
  },
};

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), ortDropDeadWasmPlugin, ortMjsPlugin],

  // The ORT glue is bundled into the TTS *web worker*, and Vite builds workers
  // with a separate plugin pipeline that does NOT inherit `plugins` above — so
  // the dead-wasm rewrite must be registered here too or it never runs.
  worker: {
    plugins: () => [ortDropDeadWasmPlugin],
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    // "127.0.0.1" (not false/localhost): Vite 7 binds ::1 for localhost while
    // the Tauri CLI polls the IPv4 devUrl, so it never sees the server.
    host: host || "127.0.0.1",
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri` and the native build
      //    trees (C++ rebuilds there crash the watcher / trigger reloads)
      ignored: ["**/src-tauri/**", "**/kokoro-sapi/**", "**/node_modules/**"],
    },
  },
}));
