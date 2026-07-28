// OCR host, running in an offscreen document.
//
// Why this exists: Tesseract needs a Web Worker, and a worker script must be same-origin with
// the document that creates it. A content script's document origin is Amazon's, so
// `new Worker(chrome-extension://.../tesseract-worker.js)` is cross-origin from there. An
// offscreen document *is* the extension origin, so the worker is same-origin, the extension's
// own CSP applies instead of Amazon's, and wasm compiles under `wasm-unsafe-eval`.
//
// It receives a page image as base64 (extension messaging is JSON-only - Blobs and
// ArrayBuffers do not survive the hop) and returns the OcrResult, which is already plain data.

import { recognize, type OcrResult } from './content/ocr';

// --------------------------------------------------------------------------- audio out
//
// The AudioContext lives here for the same reason the OCR worker does: a service worker has no
// Web Audio API. This is also where the PCM is fetched (see `http-synth`), so it never has to
// cross extension messaging; frames are scheduled on one running cursor so consecutive chunks
// play gaplessly, and the queue reports back when it has actually drained.

let ctx: AudioContext | null = null;
let gain: GainNode | null = null;
/** Absolute AudioContext time at which the next frame should start. */
let cursor = 0;
/** Sources scheduled but not yet finished. `audio-status` reports it; that is how a caller knows the page has been heard. */
let queued = 0;
/**
 * Playback generation. Bumped by every `audio-start` and every stop, and carried on each
 * message that schedules or inspects audio. A chunk that was being synthesized when you pressed
 * Stop arrives with a stale epoch and is dropped instead of playing over the silence.
 */
let epoch = 0;

function audio(): { ctx: AudioContext; gain: GainNode } {
  if (!ctx || !gain) {
    // The host emits 24 kHz mono; letting the context run at that rate avoids a resample.
    ctx = new AudioContext({ sampleRate: 24000 });
    gain = ctx.createGain();
    gain.connect(ctx.destination);
    cursor = 0;
  }
  return { ctx, gain };
}

/**
 * Begin a playback generation. Called ONCE per page, not once per chunk - the cursor has to
 * survive across chunks or every chunk restarts the clock and the gapless scheduling in
 * `pushSamples` never gets a chance to work.
 */
function startStream(): number {
  const { ctx } = audio();
  queued = 0;
  // Start slightly ahead of "now" so the first frame is not already late.
  cursor = ctx.currentTime + 0.08;
  return ++epoch;
}

/** Seconds of audio scheduled but not yet heard. Stops advancing while suspended. */
function leadSeconds(): number {
  return ctx ? Math.max(0, cursor - ctx.currentTime) : 0;
}

function pushSamples(samples: Float32Array<ArrayBuffer>): void {
  const { ctx, gain } = audio();
  if (!samples.length) return;

  const buf = ctx.createBuffer(1, samples.length, ctx.sampleRate);
  buf.copyToChannel(samples, 0);

  const src = ctx.createBufferSource();
  src.buffer = buf;
  src.connect(gain);

  const at = Math.max(cursor, ctx.currentTime);
  src.start(at);
  cursor = at + buf.duration;

  queued++;
  src.onended = () => queued--;
}

function stopAudio(): void {
  epoch++; // anything still being synthesized for the old epoch is now unwanted
  queued = 0;
  // Dropping the context is the only reliable way to cancel already-scheduled sources.
  ctx?.close();
  ctx = null;
  gain = null;
  cursor = 0;
}

interface RunMessage {
  t: 'ocr-run';
  target: 'offscreen';
  b64: string;
  type?: string;
}

function b64ToBlob(b64: string, type = 'image/png'): Blob {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new Blob([bytes], { type });
}

type Message =
  | RunMessage
  | { t: 'audio-start'; target: 'offscreen'; sampleRate?: number }
  | { t: 'audio-status'; target: 'offscreen'; epoch: number }
  | { t: 'audio-stop' | 'audio-pause' | 'audio-resume'; target: 'offscreen' }
  | {
      t: 'http-synth';
      target: 'offscreen';
      epoch?: number;
      base: string;
      token: string;
      text: string;
      voice?: string;
      speed?: number;
    };

chrome.runtime.onMessage.addListener((msg: Message, _sender, sendResponse) => {
  if (!msg || msg.target !== 'offscreen') return;

  switch (msg.t) {
    case 'ocr-run':
      (async () => {
        const blob = b64ToBlob(msg.b64, msg.type);
        const result: OcrResult = await recognize(blob);
        sendResponse({ ok: true, result });
      })().catch((e) => sendResponse({ ok: false, error: String(e) }));
      return true; // keep the channel open for the async reply

    case 'audio-start':
      sendResponse({ ok: true, epoch: startStream() });
      return false;

    // How far ahead playback is buffered, and how much is still scheduled. Two callers, two
    // uses: the worker throttles on `lead` (synthesis outruns the ear by ~3.4x, so unthrottled
    // it would render a whole page before a minute of it had been heard, holding megabytes of
    // AudioBuffers and discarding all of it on Stop), and waits on `queued` reaching zero to
    // know the page has actually been heard rather than merely scheduled.
    case 'audio-status':
      sendResponse({ ok: true, stale: msg.epoch !== epoch, queued, lead: leadSeconds() });
      return false;

    // Fetching here rather than in the service worker is the whole point: the response is raw
    // f32 PCM, and an ArrayBuffer cannot survive extension messaging - it would have to be
    // base64'd through the worker at a 33% cost per frame.
    case 'http-synth':
      (async () => {
        // No epoch means a one-off utterance rather than a page: give it its own stream.
        const mine = msg.epoch ?? startStream();
        if (mine !== epoch) {
          sendResponse({ ok: true, stale: true });
          return;
        }

        const res = await fetch(`${msg.base}/synth`, {
          method: 'POST',
          headers: { authorization: `Bearer ${msg.token}`, 'content-type': 'application/json' },
          body: JSON.stringify({ text: msg.text, voice: msg.voice, speed: msg.speed ?? 1 }),
        });
        if (!res.ok) throw new Error(`synth ${res.status}: ${await res.text()}`);

        const pcm = new Float32Array(await res.arrayBuffer());

        // Synthesis of a chunk takes seconds; Stop can easily land inside that window.
        if (mine !== epoch) {
          sendResponse({ ok: true, stale: true });
          return;
        }

        // Resolve once SCHEDULED, not once heard. Waiting for the audio here is what made every
        // chunk boundary a silence - the caller's next request could not even be sent until
        // this chunk had finished playing.
        if (pcm.length) pushSamples(pcm); // empty = a punctuation-only chunk
        sendResponse({ ok: true, lead: leadSeconds() });
      })().catch((e) => sendResponse({ ok: false, error: String(e) }));
      return true;

    case 'audio-stop':
      stopAudio();
      sendResponse({ ok: true });
      return false;

    case 'audio-pause':
      void ctx?.suspend();
      sendResponse({ ok: true });
      return false;

    case 'audio-resume':
      void ctx?.resume();
      sendResponse({ ok: true });
      return false;
  }
  return false;
});

console.log('[kwr] offscreen OCR host ready');
