// OCR host, running in an offscreen document.
//
// Why this exists: an offscreen document *is* the extension origin. It was built for the wasm
// engine's Web Worker (a worker script must be same-origin with the document that creates it,
// and a content script's document origin is Amazon's), and the engine's departure did not free
// it - `POST /ocr` is allowlisted for `chrome-extension://<id>` and nothing else, so the fetch
// that looks like it should be the direct one is exactly the one that 403s. The AudioContext
// needs an origin with a document too.
//
// It receives a page image as base64 (extension messaging is JSON-only - Blobs and
// ArrayBuffers do not survive the hop) and returns the OcrResult, which is already plain data.

import {
  preprocess,
  recognize,
  recognizeColumn,
  recognizeColumnChecked,
  type ColumnOcr,
  type OcrResult,
  type Prepared,
} from './content/ocr';
import { scheduleWords } from './word-timing';

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
 * The chunks scheduled on the cursor, in the order they were scheduled.
 *
 * Kept only so a SPEED change can throw away the ones nobody has heard yet - see `retune`. Pruned
 * as they finish, so this is at most the handful of chunks covering the lead.
 */
let sources: { src: AudioBufferSourceNode; at: number; end: number; index: number; offset: number }[] = [];
/**
 * Playback generation. Bumped by every `audio-start` and every stop, and carried on each
 * message that schedules or inspects audio. A chunk that was being synthesized when you pressed
 * Stop arrives with a stale epoch and is dropped instead of playing over the silence.
 */
let epoch = 0;
/** The `/synth` request in flight, so Stop can abandon it rather than wait it out. */
let inflight: AbortController | null = null;
/**
 * The `/ocr` requests in flight, for the same reason.
 *
 * A set rather than a single controller: a page being narrated and a reflow's trial re-OCR can
 * be recognizing at the same time, and Stop wants both.
 */
const ocrInflight = new Set<AbortController>();

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
  // A previous generation still has sources scheduled means it was superseded WITHOUT a Stop -
  // two `speak` messages overlapping. Tear it down rather than start on top of it. Two things go
  // wrong otherwise: its audio keeps playing under the new stream (only `stopAudio` closes the
  // context, so nothing else cancels a scheduled source), and its `onended` handlers fire
  // against the reset counter and drive `queued` NEGATIVE - after which the drain poll's
  // `queued === 0` never matches again and the new page never finishes.
  if (queued > 0) stopAudio();

  const { ctx } = audio();
  queued = 0;
  sources = [];
  clearMarks();
  // Start slightly ahead of "now" so the first frame is not already late.
  cursor = ctx.currentTime + 0.08;
  return ++epoch;
}

/** Seconds of audio scheduled but not yet heard. Stops advancing while suspended. */
function leadSeconds(): number {
  return ctx ? Math.max(0, cursor - ctx.currentTime) : 0;
}

/** Where a scheduled chunk sits on the AudioContext clock. */
interface Span {
  at: number;
  duration: number;
}

function pushSamples(samples: Float32Array<ArrayBuffer>, index: number, offset: number): Span | null {
  const { ctx, gain } = audio();
  if (!samples.length) return null;

  const buf = ctx.createBuffer(1, samples.length, ctx.sampleRate);
  buf.copyToChannel(samples, 0);

  const src = ctx.createBufferSource();
  src.buffer = buf;
  src.connect(gain);

  const at = Math.max(cursor, ctx.currentTime);
  src.start(at);
  cursor = at + buf.duration;

  queued++;
  // Scoped to the generation that scheduled it. `startStream`'s teardown above is the primary
  // guard; this is the one that holds if a source somehow outlives it, since a decrement from a
  // dead generation would corrupt the live count and there is no way to notice that happening.
  const mine = epoch;
  src.onended = () => {
    if (mine === epoch) queued--;
  };

  // Anything that has finished playing can no longer be retuned, so it is only ballast.
  sources = sources.filter((s) => s.end > ctx.currentTime);
  sources.push({ src, at, end: at + buf.duration, index, offset });

  return { at, duration: buf.duration };
}

/**
 * Throw away the scheduled-but-unheard tail, so a NEW speed is heard now rather than in half a
 * minute. Returns where synthesis has to pick up again - the chunk index and the character offset
 * within it - or undefined if the cursor held nothing spare.
 *
 * Speed is a synthesis parameter - the model predicts shorter phoneme durations, which is why it
 * does not change the pitch. Already-rendered samples therefore cannot be retuned; the only way a
 * speed change reaches the ear is to discard the audio rendered at the old one and ask for it
 * again. Up to `MAX_LEAD_S` of it can be sitting on the cursor, and until this existed that was
 * exactly how long a slider move took to be audible.
 *
 * The chunk being HEARD is never cut: it keeps playing to its end at the old speed, and the cursor
 * rewinds to just after it. Cutting mid-chunk would be an audible click in exchange for a couple of
 * seconds.
 *
 * The offset matters because a chunk can be scheduled in several pieces - which is what `speakAll`
 * does with the first chunk after a flush, to get a sound out before the lead is rebuilt. Reporting
 * only the index would resume at the START of a part-heard chunk on a second change, and the reader
 * would hear a sentence twice.
 */
function retune(): { index: number; offset: number } | undefined {
  if (!ctx) return undefined;

  const now = ctx.currentTime;
  const keep: typeof sources = [];
  /**
   * Where to resume. The FIRST thing dropped, not the lowest index found: everything is scheduled
   * in send order onto one cursor, so `at` rises monotonically and so do index and offset - the
   * first source past `now` is the earliest text nobody is going to hear.
   */
  let from: { index: number; offset: number } | undefined;

  for (const s of sources) {
    if (s.at <= now) {
      keep.push(s); // playing, or already played
      continue;
    }
    // Drop the handler BEFORE stopping: a stopped source fires `ended` too, and letting that
    // decrement as well would drive `queued` negative - after which the drain poll's `queued === 0`
    // never matches again and the page never finishes.
    s.src.onended = null;
    try {
      s.src.stop();
    } catch {
      // Already stopped, or the context went away under us. Either way it is not going to be heard.
    }
    s.src.disconnect();
    queued--;
    from ??= { index: s.index, offset: s.offset };
  }

  sources = keep.filter((s) => s.end > now);
  cursor = keep.reduce((end, s) => Math.max(end, s.end), now);
  // The marks of a chunk that is no longer going to play. Its replacement lays down its own.
  marks = marks.filter((m) => m.at < cursor);
  if (!marks.length) clearMarks();

  return from;
}

// ------------------------------------------------------------------------- word marks
//
// Kokoro returns audio and nothing else, so word boundaries are derived here (word-timing.ts
// splits a chunk's known duration across its words) and fired against the AUDIO clock.
//
// Against the audio clock specifically, not `setTimeout`. `ctx.currentTime` stops advancing
// while the context is suspended, so Pause freezes the highlight and Resume picks it up on the
// same word - which a wall-clock timer would have run straight past. It is also the clock the
// samples are actually scheduled on, so a mark cannot drift away from the sound it names.

interface Mark {
  /** AudioContext time. */
  at: number;
  epoch: number;
  chunk: number;
  charIndex: number;
  charLength: number;
}

/** Pending marks, earliest first. */
let marks: Mark[] = [];
let ticker: ReturnType<typeof setInterval> | null = null;

/**
 * Fine enough that the highlight lands within a frame or two of the word, coarse enough to be
 * free. Words run 200-500 ms, so this is well inside one.
 */
const TICK_MS = 40;

/**
 * `offset` is where `text` starts within chunk `chunkIndex` - non-zero when the chunk is being sent
 * in pieces. Added to every mark, so a boundary is always addressed to the whole chunk and the
 * narrator's `remap` needs to know nothing about the pieces.
 */
function queueMarks(ep: number, chunkIndex: number, text: string, span: Span, offset: number): void {
  for (const m of scheduleWords(text, span.duration)) {
    marks.push({
      at: span.at + m.at,
      epoch: ep,
      chunk: chunkIndex,
      charIndex: offset + m.charIndex,
      charLength: m.charLength,
    });
  }
  // Chunks are scheduled in order, so this is almost always already sorted; cheap insurance
  // against a reordering upstream putting the highlight into reverse.
  marks.sort((a, b) => a.at - b.at);
  ticker ??= setInterval(tick, TICK_MS);
}

function clearMarks(): void {
  marks = [];
  if (ticker !== null) {
    clearInterval(ticker);
    ticker = null;
  }
}

function tick(): void {
  if (!ctx) return clearMarks();

  const now = ctx.currentTime;
  // Only the LAST mark that has come due is sent. A highlight has one position, so the ones
  // behind it are already superseded by the time they would be delivered.
  let due: Mark | null = null;
  while (marks.length && marks[0]!.at <= now) {
    const m = marks.shift()!;
    if (m.epoch === epoch) due = m;
  }
  if (due) {
    void chrome.runtime
      .sendMessage({ t: 'kwr-word', epoch: due.epoch, chunk: due.chunk, charIndex: due.charIndex, charLength: due.charLength })
      // Nobody listening is normal: narration can be driven from the console with no page
      // waiting on boundaries. A missed highlight must never break playback.
      .catch(() => {});
  }
  if (!marks.length) clearMarks();
}

function stopAudio(): void {
  epoch++; // anything still being synthesized for the old epoch is now unwanted
  queued = 0;
  sources = [];
  clearMarks();
  // Abandon the request in flight, if any. Without this the fetch runs to completion and the
  // offscreen document sits waiting for audio nobody will hear.
  //
  // It does NOT reclaim the host's synth worker: that chunk was already dispatched and will
  // finish rendering. The exposure is bounded to one chunk (~4 sentences) because `speakAll`
  // sends them one at a time and bails on a stale epoch, so it will not queue more. Kindle
  // shares that one worker, so this is the difference between its next page waiting on one
  // abandoned chunk and waiting on a whole abandoned page.
  inflight?.abort();
  inflight = null;
  // Same for recognition. A Stop pressed while the second column is being read would otherwise
  // leave the host recognizing a page nobody is going to hear, in front of whatever the next
  // Play asks for. Aborting closes the socket, which is what the host watches for; a native
  // recognition already inside a model run finishes there (ORT cannot abandon one), and its
  // result is discarded.
  for (const ac of ocrInflight) ac.abort();
  ocrInflight.clear();
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
  /** Where the host is, and what proves this client may use it. Attached by the worker. */
  base: string;
  token: string;
  /** Recognize only this column. Absent means the whole page, every column. */
  column?: number;
  /** Identity of the render, so a second column reuses the first's preprocessing. */
  key?: string;
  /** This read is for word boxes only - keep every line and touch no furniture memory. */
  trial?: boolean;
}

/**
 * The last page prepared, so asking for its second column does not grayscale, invert and
 * gutter-detect the whole image again.
 *
 * One entry: the caller works through a page's columns in order and never goes back. A miss is
 * only ever a wasted preprocess, never a wrong answer, since the bytes come with every request.
 */
let prepared: { key: string; page: Prepared } | null = null;

async function prepareOnce(key: string | undefined, blob: Blob): Promise<Prepared> {
  if (key && prepared?.key === key) return prepared.page;
  const page = await preprocess(blob);
  prepared = key ? { key, page } : null;
  return page;
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
  | { t: 'audio-retune'; target: 'offscreen'; epoch: number }
  | { t: 'audio-stop' | 'audio-pause' | 'audio-resume'; target: 'offscreen' }
  | {
      t: 'http-synth';
      target: 'offscreen';
      epoch?: number;
      /** Which chunk of the page this is. Carried back on every word mark it produces. */
      index?: number;
      base: string;
      token: string;
      text: string;
      /** Where `text` starts within chunk `index`. Non-zero only for a chunk sent in pieces. */
      offset?: number;
      voice?: string;
      speed?: number;
    };

chrome.runtime.onMessage.addListener((msg: Message, _sender, sendResponse) => {
  if (!msg || msg.target !== 'offscreen') return;

  switch (msg.t) {
    case 'ocr-run':
      (async () => {
        const blob = b64ToBlob(msg.b64, msg.type);
        const backend = { base: msg.base, token: msg.token };
        // One controller per request, remembered so Stop can abandon a recognition in flight
        // rather than wait it out - the host reads the closed socket as a cancel.
        const ac = new AbortController();
        ocrInflight.add(ac);
        const opts = { trial: msg.trial === true, signal: ac.signal };
        try {
          if (msg.column === undefined) {
            const result: OcrResult = await recognize(blob, backend, opts);
            sendResponse({ ok: true, result });
            return;
          }
          const page = await prepareOnce(msg.key, blob);
          // Asking for a column that isn't there is how the caller learns the page is single
          // column: answer with the count rather than throwing, so it stops after the first.
          if (msg.column >= page.columns.length) {
            sendResponse({ ok: true, columns: page.columns.length });
            return;
          }
          // Only the first column can discover that the page was cut wrongly; if it did, the
          // corrected preparation replaces what is cached so the second column agrees with it.
          let result: ColumnOcr;
          if (msg.column === 0) {
            const checked = await recognizeColumnChecked(blob, page, 0, backend, opts);
            result = checked.result;
            if (msg.key && checked.prepared !== page) prepared = { key: msg.key, page: checked.prepared };
          } else {
            result = await recognizeColumn(page, msg.column, backend, opts);
          }
          sendResponse({ ok: true, result, columns: result.columns });
        } finally {
          ocrInflight.delete(ac);
        }
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

    // The speed changed mid-page. Drop the tail rendered at the old one and say from which chunk.
    // NOT a stop: the epoch stands, so the utterance carries on and the chunk being heard is
    // undisturbed - only the lead is given back.
    case 'audio-retune':
      if (msg.epoch !== epoch) sendResponse({ ok: true, stale: true });
      else sendResponse({ ok: true, resume: retune(), lead: leadSeconds() });
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

        const ac = new AbortController();
        inflight = ac;
        let res: Response;
        try {
          res = await fetch(`${msg.base}/synth`, {
            method: 'POST',
            headers: { authorization: `Bearer ${msg.token}`, 'content-type': 'application/json' },
            body: JSON.stringify({ text: msg.text, voice: msg.voice, speed: msg.speed ?? 1 }),
            signal: ac.signal,
          });
        } catch (e) {
          // An abort is Stop working as intended, not a failure - report it as stale so the
          // caller unwinds quietly instead of surfacing "AbortError" as a synthesis error.
          if (ac.signal.aborted) {
            sendResponse({ ok: true, stale: true });
            return;
          }
          // Not an abort, so it is a fetch that never got a status: say where it was going, the
          // way the OCR post does. `synth <status>` below covers everything the host answered.
          throw new Error(`could not reach ${msg.base}/synth: ${String(e)}`);
        } finally {
          if (inflight === ac) inflight = null;
        }
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
        const offset = msg.offset ?? 0;
        const span = pcm.length ? pushSamples(pcm, msg.index ?? 0, offset) : null; // empty = punctuation-only
        // The word marks can only be laid down once the chunk has a place on the clock - which
        // is here, since `pushSamples` is what decides where that is.
        if (span) queueMarks(mine, msg.index ?? 0, msg.text, span, offset);
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
