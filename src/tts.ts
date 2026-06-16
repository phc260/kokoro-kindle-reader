// Main-thread client for the kokoro-js Web Worker. Exposes a request/response
// API: each synthesize() call resolves with a playable WAV object URL.

type Backend = "webgpu" | "wasm";

type WorkerOut =
  | { type: "loading" }
  | { type: "ready"; backend: Backend }
  | { type: "audio"; id: number; audio: Float32Array; samplingRate: number }
  | { type: "error"; id: number; message: string };

let worker: Worker | null = null;
let seq = 0;
const pending = new Map<number, { resolve: (url: string) => void; reject: (e: Error) => void }>();
let readyHandler: ((backend: Backend) => void) | null = null;
// Whether the model has already warmed up. The worker posts "ready" only once,
// so we remember it: a caller that registers after that (e.g. a remounted
// component) is notified immediately instead of waiting for a message that never
// comes again.
let ready = false;
let readyBackend: Backend = "webgpu";

function ensureWorker(): Worker {
  if (worker) return worker;
  worker = new Worker(new URL("./tts.worker.ts", import.meta.url), { type: "module" });
  worker.onmessage = (e: MessageEvent<WorkerOut>) => {
    const msg = e.data;
    switch (msg.type) {
      case "ready":
        ready = true;
        readyBackend = msg.backend;
        readyHandler?.(msg.backend);
        break;
      case "audio": {
        const p = pending.get(msg.id);
        if (p) {
          pending.delete(msg.id);
          p.resolve(URL.createObjectURL(encodeWav(msg.audio, msg.samplingRate)));
        }
        break;
      }
      case "error": {
        const p = pending.get(msg.id);
        if (p) {
          pending.delete(msg.id);
          p.reject(new Error(msg.message));
        } else {
          console.error("TTS worker:", msg.message);
        }
        break;
      }
    }
  };
  return worker;
}

/** Start loading the model and register a callback fired when it's ready. If the
 * model has already warmed up, the callback fires immediately. */
export function initTTS(onReady: (backend: Backend) => void) {
  readyHandler = onReady;
  ensureWorker();
  if (ready) onReady(readyBackend);
}

/** Synthesize `text` with `voice` and resolve with a WAV object URL (caller revokes it). */
export function synthesize(text: string, voice: string): Promise<string> {
  const w = ensureWorker();
  const id = ++seq;
  return new Promise<string>((resolve, reject) => {
    pending.set(id, { resolve, reject });
    w.postMessage({ type: "speak", id, text, voice });
  });
}

/** Cancel in-flight synthesis. Pending requests resolve with "" (an empty URL)
 * so awaiting callers unwind without throwing. */
export function stopTTS() {
  worker?.postMessage({ type: "stop" });
  for (const [, p] of pending) p.resolve("");
  pending.clear();
}

// Encode mono Float32 PCM [-1,1] as a 16-bit PCM WAV blob.
function encodeWav(samples: Float32Array, sampleRate: number): Blob {
  const n = samples.length;
  const buffer = new ArrayBuffer(44 + n * 2);
  const view = new DataView(buffer);
  const writeStr = (off: number, s: string) => {
    for (let i = 0; i < s.length; i++) view.setUint8(off + i, s.charCodeAt(i));
  };
  writeStr(0, "RIFF");
  view.setUint32(4, 36 + n * 2, true);
  writeStr(8, "WAVE");
  writeStr(12, "fmt ");
  view.setUint32(16, 16, true); // fmt chunk size
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true); // byte rate
  view.setUint16(32, 2, true); // block align
  view.setUint16(34, 16, true); // bits per sample
  writeStr(36, "data");
  view.setUint32(40, n * 2, true);
  let off = 44;
  for (let i = 0; i < n; i++) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    view.setInt16(off, s < 0 ? s * 0x8000 : s * 0x7fff, true);
    off += 2;
  }
  return new Blob([buffer], { type: "audio/wav" });
}
