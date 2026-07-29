// Content-script side of narration: pick an engine and speak.
//
// chrome.tts lives in the service worker, so reaching it means a port round-trip. Web Speech
// works right here. Both are behind `Narrator` (src/speak.ts) so the page loop doesn't care
// which one is in play - and neither will care when Kokoro replaces them.

import {
  WebSpeechNarrator,
  planChunks,
  speakStream,
  type Narrator,
  type SpeakOptions,
  type TextPart,
  type VoiceInfo,
  type WordBoundary,
} from '../speak';

/**
 * Talks to the service worker's ChromeTtsNarrator over a port.
 *
 * Exported for `test/narrate.test.ts`: the port lifecycle is the one part of this file with real
 * concurrency in it, and it is not reachable through the module functions without a browser.
 */
export class PortNarrator implements Narrator {
  // The worker decides between Kokoro and the platform engine, so the real kind is only known
  // after it answers. Surfaced so the UI can say which one is actually speaking.
  kind = 'worker (engine pending)';
  #port: chrome.runtime.Port | null = null;
  /** Rejectors awaiting a reply ON `#port`, so a dying worker cannot hang the loop. */
  #pending = new Set<(e: Error) => void>();
  #seq = 0;

  /**
   * The live port, with the rejector set that belongs to it.
   *
   * The pairing matters. A port that has already died throws from `postMessage` BEFORE its
   * `onDisconnect` is delivered, so `speak` replaces it and posts again - and the old port's
   * event then arrives late. With one shared set and an unconditional `#port = null`, that late
   * event tore down the healthy replacement and rejected the retry it had just issued, so the
   * retry could never succeed and narration fell back to speechSynthesis for the session.
   * Scoping both to the connection means a stale disconnect can only affect its own requests.
   */
  #connect(): { port: chrome.runtime.Port; pending: Set<(e: Error) => void> } {
    if (this.#port) return { port: this.#port, pending: this.#pending };

    const port = chrome.runtime.connect({ name: 'narrate' });
    const pending = new Set<(e: Error) => void>();
    this.#port = port;
    this.#pending = pending;

    port.onDisconnect.addListener(() => {
      // An MV3 worker can be evicted, crash, or be torn down by an extension reload while a page
      // is mid-utterance. Nothing else would ever answer these, and `readBook` awaits them - so
      // silence here is a permanent hang, not a lost message.
      const why = new Error(chrome.runtime.lastError?.message ?? 'the extension worker disconnected');
      for (const reject of pending) reject(why);
      pending.clear();
      if (this.#port === port) {
        this.#port = null;
        this.#pending = new Set();
      }
    });

    return { port, pending };
  }

  /**
   * Fire-and-forget signal on an EXISTING port.
   *
   * `postMessage` on a port whose worker has gone throws "Attempting to use a disconnected port
   * object" synchronously, and `onDisconnect` may not have fired yet - so a cached port can look
   * alive and not be. That throw escaped `stop()` into a click handler as an uncaught error.
   * There is nothing useful to do about it: the audio died with the worker, which is what stop
   * was asking for. Never connects, either - waking a worker to tell it to stop is pointless.
   */
  #signal(msg: unknown): void {
    if (!this.#port) return;
    try {
      this.#port.postMessage(msg);
    } catch {
      this.#port = null;
    }
  }

  voices(): Promise<VoiceInfo[]> {
    return new Promise((resolve, reject) => {
      const { port: p, pending } = this.#connect();
      const done = () => {
        p.onMessage.removeListener(on);
        pending.delete(fail);
        clearTimeout(timer);
      };
      const fail = (e: Error) => {
        done();
        reject(e);
      };
      const on = (m: { t: string; voices?: VoiceInfo[]; engine?: string; engineError?: string | null }) => {
        if (m.t !== 'voices') return;
        done();
        if (m.engine) this.kind = m.engine;
        lastEngineError = m.engineError ?? null;
        resolve(m.voices ?? []);
      };

      // Answering this can mean launching the native host and loading the model, so the budget
      // is generous - but it must exist. With no reject path at all, a worker that never
      // answered left the panel on "Loading…" for the life of the tab.
      const timer = setTimeout(() => fail(new Error('the extension worker did not answer')), 60_000);
      p.onMessage.addListener(on);
      pending.add(fail);

      try {
        p.postMessage({ t: 'voices' });
      } catch (e) {
        fail(e instanceof Error ? e : new Error(String(e)));
      }
    });
  }

  speak(text: string, options?: SpeakOptions, onWord?: (b: WordBoundary) => void): Promise<void> {
    // Replies carry the request id. Without it, a stopped utterance's late `end` can resolve the
    // NEXT utterance's promise - the worker only learns a page was cancelled on its next status
    // poll, so its `end` can arrive after a new page has already started.
    const id = `u${++this.#seq}`;

    // `retry` covers the ordinary case of a worker that was evicted while idle: the first post
    // fails, a fresh connect wakes a new one, and the page speaks. Only a second failure is a
    // real fault worth reporting.
    const attempt = (retry: boolean): Promise<void> =>
      new Promise<void>((resolve, reject) => {
        const { port: p, pending } = this.#connect();
        const settle = (fn: () => void) => {
          p.onMessage.removeListener(on);
          pending.delete(reject);
          fn();
        };
        const on = (m: { t: string; id?: string; message?: string } & Partial<WordBoundary>) => {
          if (m.id !== id) return;
          if (m.t === 'word') onWord?.(m as WordBoundary);
          else if (m.t === 'end') settle(resolve);
          else if (m.t === 'error') settle(() => reject(new Error(m.message)));
        };
        p.onMessage.addListener(on);
        pending.add(reject);

        try {
          p.postMessage({ t: 'speak', id, text, options });
        } catch (e) {
          settle(() => {
            // Only drop the cache if this dead port is still the one cached; a concurrent
            // caller may already have replaced it.
            if (this.#port === p) {
              this.#port = null;
              this.#pending = new Set();
            }
            if (retry) resolve(attempt(false));
            else reject(e instanceof Error ? e : new Error(String(e)));
          });
        }
      });

    return attempt(true);
  }

  /**
   * Speak a page whose text arrives in parts, as ONE utterance on the worker's side.
   *
   * Not `speak()` per part: each call there is a separate utterance, and for the Kokoro narrator
   * a second utterance tears the first one's audio down (`startStream` in offscreen.ts). Sending
   * the parts under one id is what lets the second column of a page join the audio already
   * playing instead of interrupting it.
   *
   * The opening message is retried on a dead port exactly as `speak` retries; later parts are
   * not, because by then the worker is demonstrably alive - it answered the first one.
   */
  speakParts(
    parts: AsyncIterable<TextPart>,
    options?: SpeakOptions,
    onWord?: (b: WordBoundary) => void,
  ): Promise<void> {
    const id = `u${++this.#seq}`;

    return new Promise<void>((resolve, reject) => {
      let live = this.#connect();
      let done = false;

      const settle = (fn: () => void) => {
        done = true;
        live.port.onMessage.removeListener(on);
        live.pending.delete(fail);
        fn();
      };
      const fail = (e: Error) => settle(() => reject(e));
      const on = (m: { t: string; id?: string; message?: string } & Partial<WordBoundary>) => {
        if (m.id !== id) return;
        if (m.t === 'word') onWord?.(m as WordBoundary);
        else if (m.t === 'end') settle(resolve);
        else if (m.t === 'error') settle(() => reject(new Error(m.message)));
      };
      live.port.onMessage.addListener(on);
      live.pending.add(fail);

      /** Move the listeners to a fresh connection after the cached one turned out to be dead. */
      const reconnect = () => {
        live.port.onMessage.removeListener(on);
        live.pending.delete(fail);
        if (this.#port === live.port) {
          this.#port = null;
          this.#pending = new Set();
        }
        live = this.#connect();
        live.port.onMessage.addListener(on);
        live.pending.add(fail);
      };

      void (async () => {
        let opened = false;
        try {
          const open = (part: TextPart) => {
            const send = () =>
              live.port.postMessage({ t: 'speak', id, text: part.text, base: part.base, options, streaming: true });
            try {
              send();
            } catch {
              // The ordinary case of a worker evicted while idle: reconnect and say it again.
              // Only the opening message gets this - by the time later parts go out the worker
              // has answered, so a failure there is a real fault.
              reconnect();
              send();
            }
            opened = true;
          };

          for await (const part of parts) {
            if (done) return; // superseded or already failed; stop pulling text nobody wants
            if (opened) live.port.postMessage({ t: 'part', id, text: part.text, base: part.base });
            else open(part);
          }
          if (!opened) open({ text: '', base: 0 });

          if (!done) live.port.postMessage({ t: 'part', id, end: true });
        } catch (e) {
          const err = e instanceof Error ? e : new Error(String(e));
          if (done) return;
          if (!opened) {
            // Nothing was ever spoken and nothing will be. Reject, or the caller waits forever
            // for an `end` the worker has no reason to send. Tagged as a source failure so the
            // caller reports it instead of retrying an engine that would find the same nothing.
            fail(new SourceError(err));
            return;
          }
          // Mid-page: an OCR pass threw, or the port died. Close the feed so the worker finishes
          // what it already has rather than parking on a part that will never come - the page
          // ends short instead of hanging.
          console.warn('[kwr] page text stopped arriving:', String(err));
          try {
            live.port.postMessage({ t: 'part', id, end: true });
          } catch {
            // The port is gone; its disconnect handler rejects through `pending`.
          }
        }
      })();
    });
  }

  stop(): void {
    this.#signal({ t: 'stop' });
  }
  pause(): void {
    this.#signal({ t: 'pause' });
  }
  resume(): void {
    this.#signal({ t: 'resume' });
  }
}

/**
 * A failure that came from the TEXT SOURCE rather than the transport.
 *
 * The difference decides what to do about it: a dead worker is worth retrying on another engine,
 * but an OCR pass that threw leaves nothing to say, and falling back would swap the engine, find
 * the same empty feed, and report success on a page that was never read.
 */
class SourceError extends Error {
  constructor(readonly reason: Error) {
    super(reason.message);
    this.name = 'SourceError';
  }
}

let narrator: Narrator | null = null;
let lastEngineError: string | null = null;

/** Why the native backend was not used, if it wasn't. */
export function engineError(): string | null {
  return lastEngineError;
}

/**
 * Ask the worker to (re)attempt the native connection and report exactly what happened.
 * Answers "why am I hearing a platform voice" without opening the service worker's devtools.
 */
export function kokoroStatus(): Promise<unknown> {
  const p = chrome.runtime.connect({ name: 'narrate' });
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('worker did not answer')), 130_000);
    p.onMessage.addListener((m: { t: string }) => {
      if (m.t !== 'kokoro') return;
      clearTimeout(timer);
      resolve(m);
    });
    p.postMessage({ t: 'kokoro' });
  });
}

/**
 * Is there a background worker to bridge to?
 *
 * `chrome.runtime.connect` is NOT the test. It exists in any content script, including the
 * Firefox build — which ships no background script at all (see `build.ts`'s `skip` map; Firefox
 * has neither `chrome.tts` nor `chrome.offscreen`). There `connect` succeeds, `onDisconnect`
 * fires immediately, and every utterance rejects: not a fallback, a hard failure.
 *
 * The manifest is the honest signal, since it is the thing that actually decides whether a
 * worker exists.
 */
function hasWorker(): boolean {
  if (typeof chrome === 'undefined' || typeof chrome.runtime?.connect !== 'function') return false;
  try {
    return Boolean(chrome.runtime.getManifest().background);
  } catch {
    return false;
  }
}

export function getNarrator(): Narrator {
  // With a worker, everything goes through it — it owns both Kokoro and `chrome.tts`, neither of
  // which a content script can reach. Without one, speechSynthesis is all there is.
  narrator ??= hasWorker() ? new PortNarrator() : new WebSpeechNarrator();
  return narrator;
}

/** Force a specific engine, for comparing them. */
export function useEngine(which: 'chrome-tts' | 'web-speech'): string {
  narrator = which === 'chrome-tts' ? new PortNarrator() : new WebSpeechNarrator();
  return narrator.kind;
}

let stopped = false;

/**
 * Speak a page whose text arrives in parts - a two-column page, a column at a time, so the first
 * word is heard while the second column is still being recognized.
 *
 * Every part carries its own `base`, so `onWord` reports offsets into the WHOLE page's text
 * whichever part the word came from. That is what the highlight keys on.
 */
export async function narrateStream(
  parts: AsyncIterable<TextPart>,
  options?: SpeakOptions,
  onWord?: (charIndex: number, charLength: number | undefined) => void,
): Promise<void> {
  stopped = false;
  const engine = getNarrator();
  const report = (b: WordBoundary) => onWord?.(b.charIndex, b.charLength);

  // Over the port the parts go as ONE utterance and the worker chunks them, because the right
  // chunk schedule depends on the engine the worker chose (src/speak.ts, speakStream).
  if (engine instanceof PortNarrator) {
    try {
      await engine.speakParts(parts, options, report);
      return;
    } catch (e) {
      // The text never arrived - an OCR pass threw. Another engine would find the same nothing,
      // so report it rather than pretending to read the page.
      if (e instanceof SourceError) throw e.reason;
      // Firefox ships no background page at all, so the bridge fails on the first utterance.
      // Fall back once rather than making the caller know which browser it is on.
      console.warn('[kwr] worker bridge unavailable, falling back to speechSynthesis:', String(e));
      narrator = new WebSpeechNarrator();
    }
  }

  // NOTE: `parts` is a stream, so the fallback resumes from wherever the failed attempt left it
  // rather than restarting the page. That is the wanted behaviour for a worker that died
  // mid-page - the text already spoken is not spoken again.

  // Wrapped rather than passed straight to `speakStream`, so the loop re-checks `stopped`
  // between parts. speechSynthesis RESOLVES rather than rejects on cancel, so without the check
  // a Stop would be followed by every remaining chunk of the page being spoken in turn.
  const halting = (async function* () {
    for await (const part of parts) {
      if (stopped) return;
      yield part;
    }
  })();

  await speakStream(narrator!, halting, options, (b) => {
    if (!stopped) onWord?.(b.charIndex, b.charLength);
  });
}

/**
 * Speak a page's worth of text, sentence-chunked. `onWord` reports the character offset within
 * the ORIGINAL text, which is what highlighting keys on.
 */
export function narrate(
  text: string,
  options?: SpeakOptions,
  onWord?: (charIndex: number, charLength: number | undefined) => void,
): Promise<void> {
  return narrateStream(
    (async function* () {
      yield { text, base: 0 };
    })(),
    options,
    onWord,
  );
}

export function stop(): void {
  stopped = true;
  getNarrator().stop();
}

export function pause(): void {
  getNarrator().pause();
}

export function resume(): void {
  getNarrator().resume();
}

/** Full voice list for the panel's picker. */
export function voices(): Promise<VoiceInfo[]> {
  return getNarrator().voices();
}

/** Which engine is actually speaking - only accurate after voices() has resolved. */
export function engineKind(): string {
  return getNarrator().kind;
}

/**
 * Voice availability is a real failure mode, not a formality: a Linux box without
 * speech-dispatcher reports zero voices and then says nothing at all, silently.
 */
export async function checkVoices(): Promise<{ ok: boolean; count: number; sample: string[]; advice?: string }> {
  const vs = await getNarrator().voices();
  const english = vs.filter((v) => !v.lang || v.lang.toLowerCase().startsWith('en'));
  if (vs.length === 0) {
    return {
      ok: false,
      count: 0,
      sample: [],
      advice:
        'No TTS voices. On Linux install speech-dispatcher plus a synth (e.g. `sudo apt install speech-dispatcher espeak-ng`) and restart the browser. On Windows add voices under Settings > Time & language > Speech.',
    };
  }
  return { ok: english.length > 0, count: vs.length, sample: english.slice(0, 5).map((v) => v.name) };
}
