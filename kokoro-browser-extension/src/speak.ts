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
   * undefined and `speakChunked` drives them the plain way.
   */
  speakAll?(chunks: string[], opts?: SpeakOptions, onWord?: (b: WordBoundary, chunkIndex: number) => void): Promise<void>;
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

// ----------------------------------------------------------------------------- factory

/**
 * Best narrator available in THIS context. A content script gets Web Speech (chrome.tts is not
 * exposed there); the service worker gets chrome.tts.
 */
export function createNarrator(): Narrator {
  if (ChromeTtsNarrator.available()) return new ChromeTtsNarrator();
  if (WebSpeechNarrator.available()) return new WebSpeechNarrator();
  throw new Error('no TTS engine available in this context');
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

/**
 * Speak a whole page through whichever narrator is in play, chunked to suit it.
 *
 * This is the one place that decides the chunk schedule, and it has to be somewhere that knows
 * which engine was actually chosen - the ramp is tuned to Kokoro's throughput and would only
 * add utterance boundaries for a platform voice.
 *
 * `onWord` reports offsets into the ORIGINAL text, with each chunk's start added back.
 */
export async function speakChunked(
  n: Narrator,
  text: string,
  opts?: SpeakOptions,
  onWord?: (b: WordBoundary) => void,
): Promise<void> {
  const pieces = chunk(text, n.speakAll ? PLAYBACK_RAMP : 400);

  // Chunks rejoin with a single space, so each start offset is the running total. Approximate
  // where the source had other whitespace - good enough to pick a word, which is all
  // highlighting needs.
  const bases: number[] = [];
  let at = 0;
  for (const p of pieces) {
    bases.push(at);
    at += p.length + 1;
  }

  if (n.speakAll) {
    await n.speakAll(pieces, opts, (b, i) => onWord?.({ ...b, charIndex: (bases[i] ?? 0) + b.charIndex }));
    return;
  }

  for (let i = 0; i < pieces.length; i++) {
    await n.speak(pieces[i]!, opts, (b) => onWord?.({ ...b, charIndex: bases[i]! + b.charIndex }));
  }
}
