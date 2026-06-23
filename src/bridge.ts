// SAPI bridge (frontend side): the Rust pipe server (pipe_server.rs) owns the
// chunking — it splits each Kindle utterance and relays one `synth-request` event
// per chunk; we synthesize raw PCM with kokoro-js (WebGPU) and hand the bytes back
// via `synth_result`, which Rust streams over the named pipe to the engine. Lets
// the in-Kindle SAPI engine narrate with the same WebGPU engine the reader app uses.
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { initTTS, synthesizeRaw } from "./tts";
import { loadVoice } from "./voices";

// `rate` is the host's (Kindle's) rate-derived speed multiplier. The narrator,
// the user's speed multiplier and gain all live in this webview's localStorage —
// the same keys the reader UI writes (see App.tsx) — so the engine no longer
// carries them over the pipe (see WorkerProtocol.h).
type SynthRequest = { id: number; text: string; rate: number };
// pipe_server.rs asks for the current gain as each chunk is about to ship (it
// rides back in that PCM frame), so a volume change isn't frozen into already-
// synthesized/prefetched PCM.
type GainRequest = { id: number };
// pipe_server.rs asks once per utterance for the streaming knobs — sentences per
// chunk (drives the split) plus the pacing lead and sub-frame size (ms) — which we
// answer from the "tts-chunk" / "tts-lead" / "tts-subframe" keys the reader UI writes.
type StreamConfigRequest = { id: number };

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
    // Fold the user's own speed (localStorage) over the host's live rate.
    // Gain is NOT applied here — pipe_server.rs queries it separately and ships it
    // in each PCM frame (see the gain-request handler below), so a slider move
    // isn't baked into prefetched PCM. We return the raw synthesized samples.
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

  // pipe_server.rs queries the current gain as each chunk ships; answer from the
  // same "tts-gain" key the reader UI writes (clamped 0–2 = 0–200%).
  void listen<GainRequest>("gain-request", (e) => {
    const gain = loadNum("tts-gain", 1, 0, 2);
    void invoke("gain_result", { id: e.payload.id, gain });
  });

  // pipe_server.rs asks once per utterance for the streaming knobs; answer from
  // the same keys the reader UI writes: sentences/chunk (2–8), the pacing lead in
  // ms (how much audio stays buffered ahead — lower = snappier volume, riskier
  // underruns) and the sub-frame size in ms (gain re-read granularity). Rust
  // clamps these too; these ranges just keep the UI honest.
  void listen<StreamConfigRequest>("stream-config-request", (e) => {
    const sentences = Math.round(loadNum("tts-chunk", 4, 2, 8));
    const leadMs = Math.round(loadNum("tts-lead", 500, 50, 3000));
    const subframeMs = Math.round(loadNum("tts-subframe", 250, 20, 1000));
    void invoke("stream_config_result", { id: e.payload.id, sentences, leadMs, subframeMs });
  });
}
