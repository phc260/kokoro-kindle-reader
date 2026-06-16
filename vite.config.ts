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

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), ortMjsPlugin],

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
