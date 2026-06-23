// SAPI bridge (frontend side): the Rust pipe server (pipe_server.rs) relays each
// Kindle synth request to the webview as a `synth-request` event; we synthesize
// raw PCM with kokoro-js (WebGPU) and hand the bytes back via `synth_result`,
// which Rust writes over the named pipe. Lets the in-Kindle SAPI engine narrate
// with the same WebGPU engine the reader app uses.
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { initTTS, synthesizeRaw } from "./tts";
import { loadVoice } from "./voices";

// `rate` is the host's (Kindle's) rate-derived speed multiplier. The narrator,
// the user's speed multiplier and gain all live in this webview's localStorage —
// the same keys the reader UI writes (see App.tsx) — so the engine no longer
// carries them over the pipe (see WorkerProtocol.h).
type SynthRequest = { id: number; text: string; rate: number };
// The engine asks for the current gain when each chunk starts playing (the 'G'
// pipe command), so a volume change isn't frozen into already-synthesized PCM.
type GainRequest = { id: number };
// The engine asks once per Speak how many sentences to coalesce per chunk (the
// 'C' pipe command); we answer from the same "tts-chunk" key the reader UI writes.
type ChunkRequest = { id: number };

// Read a persisted numeric setting, clamped, falling back to `def` if unset/NaN.
function loadNum(key: string, def: number, lo: number, hi: number): number {
  const v = parseFloat(localStorage.getItem(key) ?? "");
  return Number.isFinite(v) ? Math.min(Math.max(v, lo), hi) : def;
}

let started = false;

/** Begin serving SAPI synth requests. Idempotent. */
export function startSapiBridge() {
  if (started) return;
  started = true;
  // Warm the model so the first Kindle request isn't slow.
  initTTS(() => {});
  void listen<SynthRequest>("synth-request", async (e) => {
    const { id, text, rate } = e.payload;
    // Fold the user's own settings (localStorage) over the host's live rate.
    // Gain is NOT applied here — it's queried by the engine at playback (see the
    // gain-request handler below), so a slider move isn't baked into prefetched
    // PCM. We return the raw synthesized samples.
    const voice = loadVoice();
    const speed = rate * loadNum("tts-speed", 1, 0.5, 2);
    try {
      const { audio } = await synthesizeRaw(text, voice, speed);
      // Raw little-endian f32 bytes; Rust reinterprets as the sample buffer.
      const bytes = new Uint8Array(audio.buffer, audio.byteOffset, audio.byteLength);
      await invoke("synth_result", { id, pcm: bytes });
    } catch (err) {
      console.error("[bridge] synth failed:", err);
      await invoke("synth_result", { id, pcm: new Uint8Array(0) });
    }
  });

  // The engine queries the current gain as each chunk begins playing; answer
  // from the same "tts-gain" key the reader UI writes (clamped 0–2 = 0–200%).
  void listen<GainRequest>("gain-request", (e) => {
    const gain = loadNum("tts-gain", 1, 0, 2);
    void invoke("gain_result", { id: e.payload.id, gain });
  });

  // The engine asks once per Speak for the per-chunk sentence count; answer from
  // the same "tts-chunk" key the reader UI writes (integer, clamped 2–8).
  void listen<ChunkRequest>("chunk-request", (e) => {
    const sentences = Math.round(loadNum("tts-chunk", 4, 2, 8));
    void invoke("chunk_result", { id: e.payload.id, sentences });
  });
}
