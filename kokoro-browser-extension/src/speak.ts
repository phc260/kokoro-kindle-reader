// The narration seam. Everything that turns a string into sound sits behind `Narrator`, so
// the engine is swappable without touching capture, OCR, or the page loop.
//
// Three implementations are anticipated:
//
//   ChromeTts   - chrome.tts, the platform engine. Chrome/Edge only; must run in the service
//                 worker, since chrome.* TTS is not exposed to content scripts.
//   WebSpeech   - speechSynthesis. Works in Chrome AND Firefox, and directly in a content
//                 script, so it is the portable default.
//   Kokoro      - the native backend (ARCHITECTURE.md). Better voices, install required.
//
// Platform reality worth knowing before choosing: on Windows both platform engines expose the
// SAPI voices and sound acceptable. On Linux, Chrome and Firefox have NO built-in voices - they
// route to speech-dispatcher, so a machine without it (or without espeak/festival behind it)
// reports an empty voice list and silently says nothing. `voices()` returning empty is a
// first-class outcome, not an edge case, and it is exactly why the Kokoro backend still earns
// its keep on Linux.

export interface VoiceInfo {
  name: string;
  lang?: string;
  /** Present on chrome.tts; a remote voice needs network and adds latency. */
  remote?: boolean;
}

export interface SpeakOptions {
  voiceName?: string;
  /** 0.1-10, 1 = normal. */
  rate?: number;
  pitch?: number;
  volume?: number;
  lang?: string;
}

/** Emitted as each word starts, when the engine supports boundaries. */
export interface WordBoundary {
  charIndex: number;
  charLength?: number;
  elapsedMs: number;
}

export interface Narrator {
  readonly kind: string;
  voices(): Promise<VoiceInfo[]>;
  /** Resolves when the utterance finishes; resolves early (not rejects) if stopped. */
  speak(text: string, opts?: SpeakOptions, onWord?: (b: WordBoundary) => void): Promise<void>;
  /**
   * Speak a pre-chunked page with synthesis overlapped against playback.
   *
   * Only engines that synthesize ahead of the ear implement this. Speaking chunk by chunk
   * through `speak()` is correct but not continuous for them: each chunk's synthesis becomes a
   * silence, measured at 85 SECONDS across a five-minute page (backend/test-pipeline.ts). The
   * platform engines have nothing to overlap - they own their own audio - so they leave it
   * undefined and `speakStream` drives them the plain way, one `speak()` per chunk.
   *
   * A STREAM of chunks, not an array, because a page's text does not all exist at once: a
   * two-column page is OCR'd a column at a time so the first word can be heard while the second
   * column is still being recognized. The chunks that arrive late must join the utterance already
   * playing - starting a second one would tear the first down (`startStream` in offscreen.ts) and
   * put a synthesis-length silence in the middle of the page.
   */
  speakAll?(
    chunks: AsyncIterable<string>,
    opts?: SpeakOptions,
    onWord?: (b: WordBoundary, chunkIndex: number) => void,
  ): Promise<void>;
  stop(): void;
  pause(): void;
  resume(): void;
}

// ------------------------------------------------------------------------ chrome.tts

/**
 * chrome.tts. Only constructible in an extension context that has the API - i.e. the service
 * worker or an extension page, NOT a content script. See background.ts for the message bridge
 * that lets the content script reach it.
 */
export class ChromeTtsNarrator implements Narrator {
  readonly kind = 'chrome.tts';

  static available(): boolean {
    return typeof chrome !== 'undefined' && !!chrome.tts?.speak;
  }

  async voices(): Promise<VoiceInfo[]> {
    const vs = await chrome.tts.getVoices();
    return vs.map((v) => ({ name: v.voiceName ?? '', lang: v.lang, remote: v.remote }));
  }

  speak(text: string, opts: SpeakOptions = {}, onWord?: (b: WordBoundary) => void): Promise<void> {
    const started = performance.now();
    return new Promise((resolve, reject) => {
      chrome.tts.speak(text, {
        voiceName: opts.voiceName,
        lang: opts.lang ?? 'en-US',
        rate: opts.rate ?? 1,
        pitch: opts.pitch ?? 1,
        volume: opts.volume ?? 1,
        enqueue: false,
        onEvent: (e) => {
          switch (e.type) {
            case 'word':
              onWord?.({
                charIndex: e.charIndex ?? 0,
                charLength: e.length,
                elapsedMs: performance.now() - started,
              });
              break;
            case 'end':
            case 'interrupted':
            case 'cancelled':
              resolve();
              break;
            case 'error':
              reject(new Error(e.errorMessage ?? 'chrome.tts error'));
              break;
          }
        },
      });
    });
  }

  stop(): void {
    chrome.tts.stop();
  }
  pause(): void {
    chrome.tts.pause();
  }
  resume(): void {
    chrome.tts.resume();
  }
}

// ------------------------------------------------------------------------- Web Speech

/**
 * speechSynthesis. Portable across Chrome and Firefox and usable straight from a content
 * script, which makes it the default until the Kokoro backend exists.
 */
export class WebSpeechNarrator implements Narrator {
  readonly kind = 'speechSynthesis';
  #current: SpeechSynthesisUtterance | null = null;

  static available(): boolean {
    return typeof speechSynthesis !== 'undefined';
  }

  /** Voices load asynchronously in Chrome; the first call can otherwise see an empty list. */
  voices(): Promise<VoiceInfo[]> {
    // `localService: false` means the engine is a NETWORK service - Chrome's bundled
    // "Google US English" and friends synthesize on Google's servers, so choosing one sends the
    // book's text off the machine. Surfaced here so the UI can say so.
    const map = (vs: SpeechSynthesisVoice[]) =>
      vs.map((v) => ({ name: v.name, lang: v.lang, remote: !v.localService }));
    const now = speechSynthesis.getVoices();
    if (now.length) return Promise.resolve(map(now));

    return new Promise((resolve) => {
      const done = () => resolve(map(speechSynthesis.getVoices()));
      speechSynthesis.addEventListener('voiceschanged', done, { once: true });
      setTimeout(done, 1000);
    });
  }

  async speak(text: string, opts: SpeakOptions = {}, onWord?: (b: WordBoundary) => void): Promise<void> {
    const u = new SpeechSynthesisUtterance(text);
    u.rate = opts.rate ?? 1;
    u.pitch = opts.pitch ?? 1;
    u.volume = opts.volume ?? 1;
    u.lang = opts.lang ?? 'en-US';

    if (opts.voiceName) {
      const v = speechSynthesis.getVoices().find((x) => x.name === opts.voiceName);
      if (v) u.voice = v;
    }

    const started = performance.now();
    this.#current = u;

    await new Promise<void>((resolve, reject) => {
      u.onboundary = (e) => {
        if (e.name === 'word' || e.name === undefined) {
          onWord?.({ charIndex: e.charIndex, charLength: e.charLength, elapsedMs: performance.now() - started });
        }
      };
      u.onend = () => resolve();
      u.onerror = (e) => (e.error === 'interrupted' || e.error === 'canceled' ? resolve() : reject(new Error(e.error)));
      speechSynthesis.speak(u);
    });

    this.#current = null;
  }

  stop(): void {
    speechSynthesis.cancel();
    this.#current = null;
  }
  pause(): void {
    speechSynthesis.pause();
  }
  resume(): void {
    speechSynthesis.resume();
  }
}

/**
 * Chunk sizes for Kokoro, in order; the last one repeats for the rest of the page.
 *
 * Not a guess - measured (backend/test-pipeline.ts). Synthesis runs ~3.4x realtime, so once a
 * chunk is playing it buys ~0.7s of lead per second of audio, and the lead is what hides the
 * next chunk's synthesis. A uniform size cannot win both ends of that: 400 chars everywhere is
 * gapless but takes 5.8s to say the first word, and 200 everywhere starts in 0.7s but has not
 * built enough lead to cover the second chunk. Ramping starts small and grows, which is the
 * only shape that gets a fast first word AND a lead that never runs out.
 */
export const PLAYBACK_RAMP = [80, 160, 280, 400];

/**
 * chrome.tts and speechSynthesis both degrade badly on very long strings - some engines cap the
 * utterance, others lose boundary events partway. A page is split into sentence-ish chunks and
 * spoken in sequence.
 *
 * `max` is either one size for every chunk, or a schedule indexed by chunk number whose last
 * entry repeats.
 */
export function chunk(text: string, max: number | readonly number[] = 400): string[] {
  const sizeAt = (i: number) => (typeof max === 'number' ? max : max[Math.min(i, max.length - 1)]!);

  // Sentences first, then clauses within any sentence that overshoots the smallest budget.
  // Sentence ends alone cannot hit the ramp's early sizes - the opening sentence of a chapter
  // is whatever length it is - and a chunk far under budget starves the one behind it. Commas,
  // semicolons and colons are natural pauses, so a chunk ending at one still sounds intended;
  // the separators survive the split, so repacking is lossless.
  const smallest = typeof max === 'number' ? max : Math.min(...max);
  const atoms: string[] = [];
  for (const sentence of text.split(/(?<=[.!?])\s+|\n{2,}/)) {
    const s = sentence.trim();
    if (!s) continue;
    if (s.length <= smallest) atoms.push(s);
    else for (const clause of s.split(/(?<=[,;:])\s+/)) if (clause.trim()) atoms.push(clause.trim());
  }

  const out: string[] = [];
  let buf = '';
  for (const atom of atoms) {
    const budget = sizeAt(out.length);
    // Overshooting the budget is cheaper than emitting a runt: a chunk well under budget
    // finishes playing before the next one has finished synthesizing, which is audible.
    const tooSmallToFlush = buf.length < budget * 0.6;
    if (buf && !tooSmallToFlush && buf.length + atom.length + 1 > budget) {
      out.push(buf);
      buf = atom;
    } else {
      buf = buf ? `${buf} ${atom}` : atom;
    }
  }
  if (buf) out.push(buf);
  return out;
}

// ------------------------------------------------------------------------ chunk offsets

/** Non-whitespace characters in `s` before `end`. */
function countInk(s: string, end = s.length): number {
  let n = 0;
  for (let i = 0; i < end && i < s.length; i++) if (!/\s/.test(s[i]!)) n++;
  return n;
}

/**
 * A page's chunks, plus the exact map from a boundary inside one back to the original text.
 *
 * The alignment counts NON-WHITESPACE characters rather than summing chunk lengths. `chunk()`
 * only ever drops or normalizes whitespace - it splits on it, trims, and rejoins with a single
 * space - so the sequence of non-whitespace characters survives untouched, and counting them is
 * exact. Summing lengths is not: it assumes one space between every pair of chunks, so a page
 * whose paragraphs are separated by a blank line loses a character at each one. That drift is
 * invisible for a sentence and about a word wide by the foot of a page, which is exactly where
 * a highlight is most obviously wrong.
 */
export interface ChunkPlan {
  pieces: string[];
  /** Rewrite a boundary reported against `pieces[i]` into one against the original text. */
  remap(b: WordBoundary, i: number): WordBoundary;
}

export function planChunks(text: string, max: number | readonly number[]): ChunkPlan {
  const pieces = chunk(text, max);

  // Original index of the nth non-whitespace character.
  const ink: number[] = [];
  for (let i = 0; i < text.length; i++) if (!/\s/.test(text[i]!)) ink.push(i);
  const at = (n: number) => (n < ink.length ? ink[n]! : text.length);

  const bases: number[] = [];
  let seen = 0;
  for (const p of pieces) {
    bases.push(seen);
    seen += countInk(p);
  }

  return {
    pieces,
    remap(b, i) {
      const piece = pieces[i] ?? '';
      const start = (bases[i] ?? 0) + countInk(piece, b.charIndex);
      const charIndex = at(start);
      // Measure the span in ink too, then take the index just past its last character - a word
      // whose source had a line break inside it stays one highlight rather than two.
      const span = countInk(piece.slice(b.charIndex, b.charIndex + (b.charLength ?? 0)));
      const charLength = span > 0 ? at(start + span - 1) + 1 - charIndex : b.charLength;
      return { ...b, charIndex, charLength };
    },
  };
}

// ------------------------------------------------------------------------ streaming a page

/**
 * A piece of a page's text, and where it starts within the page.
 *
 * The producer states `base` rather than it being inferred from the running total of part
 * lengths, because the two do not have to agree: a part is cut at the last sentence end, not at
 * the column boundary, so the pieces are slices of the page's text at positions only the producer
 * knows. Everything downstream reports offsets against the page, so this is what makes a boundary
 * from part two address the same string the highlight indexed.
 */
export interface TextPart {
  text: string;
  base: number;
}

export interface StreamPlan {
  chunks: AsyncGenerator<string>;
  /** Rewrite a boundary reported against the `i`th chunk into one against the whole page. */
  remap(b: WordBoundary, i: number): WordBoundary;
}

/**
 * A page's text as it arrives, pushed in and iterated out.
 *
 * Lives here rather than in background.ts, where it is used, because it is the one part of the
 * streaming path with real concurrency in it and nothing about it is chrome-specific.
 *
 * `speakStream` parks on this between parts, so EVERY way an utterance can end has to close it -
 * finishing, Stop, a superseded page, the port disconnecting. Miss one and the worker sits
 * awaiting a part nobody will send, holding the utterance open, and the page never finishes.
 */
export class PartQueue implements AsyncIterable<TextPart> {
  #queue: TextPart[] = [];
  #wake: (() => void) | null = null;
  #closed = false;

  push(part: TextPart): void {
    if (this.#closed) return;
    this.#queue.push(part);
    this.#release();
  }

  close(): void {
    this.#closed = true;
    this.#release();
  }

  get closed(): boolean {
    return this.#closed;
  }

  #release(): void {
    const wake = this.#wake;
    this.#wake = null;
    wake?.();
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<TextPart> {
    for (;;) {
      // Drain before parking, and re-check on waking: a part can land between the two, and a
      // close can land while parts are still queued - those still have to come out.
      while (this.#queue.length) yield this.#queue.shift()!;
      if (this.#closed) return;
      await new Promise<void>((resolve) => (this.#wake = resolve));
    }
  }
}

/**
 * Index just past the last sentence end at or after `from`, or `from` if there isn't one.
 *
 * Where a part may be cut. Each part is chunked and synthesized on its own, so the seam between
 * two of them is heard - which is fine between sentences and plainly wrong in the middle of one.
 * A column of a two-column page nearly always runs into the next mid-sentence, so the tail is
 * held back and yielded with the column that continues it.
 *
 * The lookahead is what stops a decimal point or a mid-token dot ("3.14") from counting.
 */
export function sentenceEnd(text: string, from: number): number {
  let last = -1;
  for (const m of text.slice(from).matchAll(/[.!?]["'”’)\]]*(?=\s|$)/g)) last = m.index + m[0].length;
  return last > 0 ? from + last : from;
}

/** One value as a stream, so a plain string goes down the same path as a column feed. */
async function* only<T>(value: T): AsyncGenerator<T> {
  yield value;
}

/**
 * Chunk a stream of text parts, keeping every boundary addressed to the whole page.
 *
 * `owners` is filled as chunks are yielded and read when boundaries come back, which is always
 * afterwards - a chunk has to be spoken before it can report a word.
 */
export function planStream(
  parts: AsyncIterable<TextPart>,
  first: number | readonly number[],
  rest: number | readonly number[],
): StreamPlan {
  const owners: { plan: ChunkPlan; local: number; base: number }[] = [];

  return {
    chunks: (async function* () {
      let opening = true;
      for await (const part of parts) {
        if (!part.text) continue;
        // Only the FIRST part ramps. The ramp buys a fast first word by starting small; a later
        // part is arriving mid-page with a lead already built, and restarting it there would
        // emit a runt chunk that finishes before the one behind it is synthesized.
        const plan = planChunks(part.text, opening ? first : rest);
        opening = false;
        for (let i = 0; i < plan.pieces.length; i++) {
          owners.push({ plan, local: i, base: part.base });
          yield plan.pieces[i]!;
        }
      }
    })(),

    remap(b, i) {
      const o = owners[i];
      if (!o) return b;
      const m = o.plan.remap(b, o.local);
      return { ...m, charIndex: o.base + m.charIndex };
    },
  };
}

/**
 * Speak a page that arrives in parts, through whichever narrator is in play.
 *
 * This is the one place that decides the chunk schedule, and it has to be somewhere that knows
 * which engine was actually chosen - the ramp is tuned to Kokoro's throughput and would only
 * add utterance boundaries for a platform voice.
 *
 * `onWord` reports offsets into the whole page's text, which is what the highlight keys on.
 */
export async function speakStream(
  n: Narrator,
  parts: AsyncIterable<TextPart>,
  opts?: SpeakOptions,
  onWord?: (b: WordBoundary) => void,
): Promise<void> {
  const settled = PLAYBACK_RAMP[PLAYBACK_RAMP.length - 1]!;
  const plan = n.speakAll ? planStream(parts, PLAYBACK_RAMP, settled) : planStream(parts, 400, 400);

  if (n.speakAll) {
    await n.speakAll(plan.chunks, opts, (b, i) => onWord?.(plan.remap(b, i)));
    return;
  }

  let i = 0;
  for await (const piece of plan.chunks) {
    const at = i++;
    await n.speak(piece, opts, (b) => onWord?.(plan.remap(b, at)));
  }
}
