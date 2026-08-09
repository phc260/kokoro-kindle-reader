// Narrator backed by kokoro-host's loopback HTTP endpoint (kokoro-host/src/webserve.rs).
//
// The ONLY route to the backend. A native-messaging bridge was tried ahead of this and removed:
// it needs per-browser registry registration, two manifest dialects and a browser restart, it
// could never serve Firefox at all, and every symptom arrives as the same one string. This needs no
// registration, works in any browser, and can be reproduced with curl.
//
// What replaces the browser's gating lives in the host: 127.0.0.1 bind + origin allowlist +
// bearer token + Host check (see kokoro-host/src/webserve.rs). The pairing code is the cost of
// that - one paste, from the tray menu.
//
// The audio never passes through the service worker. The offscreen document does the fetch
// itself, so PCM arrives as an ArrayBuffer and goes straight into an AudioBuffer - extension
// messaging is JSON-only, so routing it through the worker would mean base64 and a 33% tax on
// every frame.
//
//   worker:    /status, orchestration          (small JSON)
//   offscreen: POST /synth -> ArrayBuffer -> AudioContext, and the word marks off its clock

import { planChunks, PLAYBACK_RAMP, type Narrator, type SpeakOptions, type VoiceInfo, type WordBoundary } from './speak';
import { langOf } from './voices';
import {
  onWordMarks,
  raceInterrupt,
  retuneStream,
  sendToOffscreen,
  sleep,
  startStream,
  tellOffscreen,
  waitForRoom,
} from './offscreen-client';

export interface Pairing {
  base: string;
  token: string;
}

const STORAGE_KEY = 'kwr.endpoint';

/** Matches `webserve::DEFAULT_PORT` in kokoro-host. Only used to probe for an unpaired daemon. */
export const DEFAULT_PORT = 8787;

export type Probe =
  /** Nothing accepted the connection. */
  | { state: 'absent'; base: string }
  /** Answered 401: alive, and waiting for a token it has not been given. */
  | { state: 'running'; base: string }
  /** Answered 403: alive, but this extension id is not in its allowlist. */
  | { state: 'origin-rejected'; base: string };

/**
 * Is a daemon listening, without a token to prove it?
 *
 * `host_permissions` covers `http://127.0.0.1/*`, so a worker fetch there is not subject to CORS
 * and the status code is readable. That is the whole trick: *any* HTTP reply means something is
 * listening, and the code says which kind of not-working this is. A refused connection throws,
 * and on loopback it throws immediately.
 */
export async function probeDaemon(port: number = DEFAULT_PORT): Promise<Probe> {
  const base = `http://127.0.0.1:${port}`;
  try {
    const res = await fetch(`${base}/status`, { signal: AbortSignal.timeout(1500) });
    if (res.status === 403) return { state: 'origin-rejected', base };
    return { state: 'running', base };
  } catch {
    return { state: 'absent', base };
  }
}

/** One sentence naming the actual next action, for the panel's status line. */
export function describeProbe(p: Probe): string {
  switch (p.state) {
    case 'absent':
      return `no Kokoro daemon on ${p.base} - start Kokoro Kindle Reader (the tray app)`;
    case 'origin-rejected':
      return `Kokoro daemon is running on ${p.base} but does not allow this extension id (${chrome.runtime.id})`;
    case 'running':
      return `Kokoro daemon is running on ${p.base} but this extension is not paired - open its options page and paste the pairing code`;
  }
}

/**
 * Why a request to a PAIRED host failed below HTTP, which is a different question.
 *
 * `describeProbe` reads a 401 as "not paired", and that is right only for the caller that already
 * knows there is no pairing. Said to someone who has one it is a wrong diagnosis - worse than a
 * vague one, because it sends them to re-paste a code that was never the problem. What a probe
 * means once a pairing exists depends on WHERE that pairing points:
 *
 *   * a different base from the one answering - the host moved port, so the saved code is stale;
 *   * the same base, answering a probe but not this request - the port is live, so the request
 *     itself is what failed. An over-cap body is the way that happens on this endpoint: the reply
 *     lands while the client is still uploading and is lost with the connection.
 */
export function describeUnreachable(base: string, p: Probe): string {
  if (p.state === 'absent')
    return base === p.base
      ? `no Kokoro daemon on ${base} - start Kokoro Kindle Reader (the tray app)`
      : `nothing is listening on ${base} (paired) or ${p.base} - start Kokoro Kindle Reader (the tray app)`;
  if (p.state === 'origin-rejected')
    return `Kokoro daemon is running on ${p.base} but does not allow this extension id (${chrome.runtime.id})`;
  if (base !== p.base)
    return `paired with ${base}, but the Kokoro daemon is on ${p.base} - copy a fresh pairing code from the tray`;
  return `the Kokoro daemon on ${base} is listening but the request did not complete - see the service worker console`;
}

/**
 * Why a request that carried a BODY failed below HTTP, asked with one that carries none.
 *
 * A large POST is the one request whose failure cannot distinguish its own causes. The host writes
 * its 401 without draining the body - deliberately, so an unauthenticated peer cannot make it copy
 * megabytes - and a reply written under an upload still in flight can be lost with the connection.
 * So a stale token and an unreachable host arrive here as the same nothing.
 *
 * A bodiless `/status` separates them, because it cannot lose its reply: the request fits in a
 * single segment, so there is no in-flight write for the close to destroy. Worth the extra
 * round-trip because it only happens on a path that has already failed, and because "re-pair" and
 * "the daemon is gone" send the user to completely different places.
 */
export async function diagnosePaired(p: Pairing): Promise<string> {
  try {
    const res = await fetch(`${p.base}/status`, {
      headers: { authorization: `Bearer ${p.token}` },
    });
    if (res.status === 401) return 'the Kokoro host rejected the pairing token - re-pair from the options page';
    if (res.status === 403) return `the Kokoro host does not allow this extension id (${chrome.runtime.id})`;
    if (res.ok)
      // Reachable and paired, so the pairing is not the question - but do NOT name a cause. Two
      // different failures land here and only one of them is size: an over-cap body, and a body
      // that stalled past the host's own request timeout (`REQUEST_TIMEOUT`), which drops the
      // connection with no response at all. "Check its size" is a wrong next action for the
      // second, and a wrong next action costs more than a vague one.
      return `the Kokoro host on ${p.base} is reachable and paired, so the pairing is not the problem - the request itself did not complete (too large, or slower than the host waits)`;
    return `the Kokoro host on ${p.base} answered ${res.status}`;
  } catch {
    // Even the bodiless request could not get through, so the pairing is not the question.
    return describeUnreachable(p.base, await probeDaemon());
  }
}

/** `kwr_<port>_<token>` - one opaque string is easier to paste correctly than two fields. */
export function parsePairing(s: string): Pairing | null {
  const m = /^kwr_(\d{1,5})_([0-9a-f]{32,128})$/.exec(s.trim());
  if (!m) return null;
  return { base: `http://127.0.0.1:${m[1]}`, token: m[2]! };
}

export async function savePairing(p: Pairing | null): Promise<void> {
  if (p) await chrome.storage.local.set({ [STORAGE_KEY]: p });
  else await chrome.storage.local.remove(STORAGE_KEY);
}

export async function loadPairing(): Promise<Pairing | null> {
  const got = await chrome.storage.local.get(STORAGE_KEY);
  return (got[STORAGE_KEY] as Pairing | undefined) ?? null;
}


export class KokoroHttpNarrator implements Narrator {
  readonly kind = 'kokoro (http)';
  #pairing: Pairing;

  constructor(pairing: Pairing) {
    this.#pairing = pairing;
  }

  get pairing(): Pairing {
    return this.#pairing;
  }

  /** Handshake. Also the warm-up: the daemon has the model loaded before the first page. */
  async status(): Promise<{ voice: string; voices: string[]; sampleRate: number }> {
    let res: Response;
    try {
      res = await fetch(`${this.#pairing.base}/status`, {
        headers: { authorization: `Bearer ${this.#pairing.token}` },
      });
    } catch (e) {
      // Every branch below reads a status off a response. A fetch that failed below HTTP has
      // none, and its message names nothing - not the host, not the port, not the endpoint - so
      // it is the one failure that has to be told where it was going. Which KIND of not-running
      // this is comes from `probeDaemon`, at the caller that has one.
      throw new Error(`could not reach ${this.#pairing.base}/status: ${String(e)}`);
    }
    if (res.status === 401) throw new Error('token rejected - re-pair from the options page');
    if (res.status === 403) throw new Error('origin rejected - the daemon does not allow this extension id');
    if (!res.ok) throw new Error(`status ${res.status}`);
    return await res.json();
  }

  async voices(): Promise<VoiceInfo[]> {
    const s = await this.status();
    // Local by construction - that is the entire point of running our own backend. The tag comes
    // from the id's first letter, so the `bf_*`/`bm_*` voices stop claiming to be American.
    return s.voices.map((name) => ({ name, lang: langOf(name) ?? 'en-US', remote: false }));
  }

  async speak(text: string, opts: SpeakOptions = {}, onWord?: (b: WordBoundary) => void): Promise<void> {
    await this.speakAll(
      (async function* () {
        yield text;
      })(),
      opts,
      (b) => onWord?.(b),
    );
  }

  /**
   * Synthesize ahead of playback, scheduling every chunk onto one AudioContext cursor.
   *
   * The daemon renders ~3.4x faster than the ear consumes, so after the first chunk there is
   * always more audio queued than time to play it - which is exactly what hides the next
   * chunk's synthesis. Two things keep that honest: the epoch, so Stop discards work already in
   * flight, and the lead cap, so a page does not render five minutes ahead and throw it away.
   *
   * That lead is also why word boundaries cannot be reported from here. By the time a chunk's
   * request is answered its audio may be half a minute from being heard, so the offscreen
   * document times the marks off its own audio clock and broadcasts them; this only forwards
   * them (see `onWordMarks`).
   *
   * `opts` is read on every send rather than once, and it is the LIVE object the caller is holding:
   * that is how a speed change lands on the page already playing. That is only half of it - a rate
   * applied to chunks not yet sent is still half a minute of the old speed, because the lead this
   * method works to build is exactly that much already-rendered audio. The other half is
   * `retuneStream`, which gives the lead back (see `retune` in offscreen.ts).
   */
  async speakAll(
    chunks: AsyncIterable<string>,
    opts: SpeakOptions = {},
    onWord?: (b: WordBoundary, i: number) => void,
  ): Promise<void> {
    const epoch = await startStream();
    const started = performance.now();
    const unsubscribe = onWord
      ? onWordMarks(epoch, (m) =>
          onWord(
            { charIndex: m.charIndex, charLength: m.charLength, elapsedMs: performance.now() - started },
            m.chunk,
          ),
        )
      : null;

    // ONE epoch for the whole feed. Chunks that only exist minutes in - the second column of a
    // page, recognized while the first is playing - schedule onto the cursor already running,
    // which is the same mechanism that makes consecutive chunks gapless.
    //
    // The chunks are kept as they are sent, because a speed change has to be able to ask for the
    // unheard ones again - at their ORIGINAL indices, so the boundaries a re-sent chunk produces
    // still remap onto the same words (`planStream` keys its owners by chunk order).
    const sent: string[] = [];
    let next = 0;
    let drained = false;
    const feed = chunks[Symbol.asyncIterator]();
    /**
     * The pull in flight, when a wait for more text was cut short by a speed change.
     *
     * Held rather than re-issued: `next()` consumes the value, so a dropped pull is a chunk of the
     * book nobody ever hears. Waiting for text is a real wait on a streaming page - the second
     * column of a two-column page is still being recognized - and it is the one wait that would
     * otherwise not notice the slider.
     */
    let pull: Promise<IteratorResult<string>> | null = null;

    /** The speed everything scheduled so far was rendered at. */
    let speed = opts.rate ?? 1;
    const changed = () => (opts.rate ?? 1) !== speed;

    /** Chunk `next` as the piece(s) it goes out in - one whole piece, normally. */
    let pending: { text: string; offset: number }[] = [];

    /**
     * The pieces of chunk `index` from character `from` onward.
     *
     * `ramp` re-enters `PLAYBACK_RAMP` for the first chunk after a flush, and it is the difference
     * between a change that is heard and one that leaves a hole. A flush hands back the lead, which
     * puts synthesis in exactly the state it is in at the start of a page - nothing buffered - and a
     * settled-size chunk takes ~5.8s to render. Whenever the chunk still playing had less than that
     * left, the reader heard a silence one to two sentences long. The ramp's first piece renders in
     * well under a second and the rest grow back to settled behind it, which is the same shape and
     * the same reasoning as the opening of a page.
     *
     * Cut by `chunk()`, so a piece still ends at a sentence or clause end rather than mid-phrase,
     * and each piece states where it starts in the chunk's text (counted in ink, since `chunk()`
     * normalizes whitespace) so its word marks stay addressed to the chunk.
     */
    const load = (index: number, ramp: boolean, from = 0): { text: string; offset: number }[] => {
      const text = sent[index]!;
      if (!ramp) return [{ text, offset: 0 }];
      const sub = planChunks(text, PLAYBACK_RAMP);
      return sub.pieces
        .map((text, i) => ({ text, offset: sub.remap({ charIndex: 0, elapsedMs: 0 }, i).charIndex }))
        .filter((p) => p.offset >= from);
    };

    try {
      for (;;) {
        // Take the new speed first, so nothing is sent at the old one after this point, and only
        // then give back the lead. Anything discarded is re-sent by the loop below.
        if (changed()) {
          speed = opts.rate ?? 1;
          const resume = await retuneStream(epoch);
          if (resume === 'stopped') return;
          if (resume) {
            next = resume.index;
            pending = load(next, true, resume.offset);
          }
          continue;
        }

        if (pending.length || next < sent.length || !drained) {
          if (!pending.length) {
            if (next === sent.length) {
              pull ??= feed.next();
              const it = await raceInterrupt(pull, changed);
              if (!it) continue; // retune first; the pull is still held and still owed a chunk
              pull = null;
              if (it.done) {
                drained = true;
                continue;
              }
              sent.push(it.value);
            }
            pending = load(next, false);
          }
          const room = await waitForRoom(epoch, changed);
          if (room === 'stopped') return;
          if (room === 'interrupted') continue; // retune first, then send at the new speed
          const piece = pending[0]!;
          const r = await sendToOffscreen({
            t: 'http-synth',
            epoch,
            index: next,
            offset: piece.offset,
            base: this.#pairing.base,
            token: this.#pairing.token,
            text: piece.text,
            voice: opts.voiceName,
            speed,
          });
          if (r.stale) return;
          pending.shift();
          // The chunk is only done when its last piece has gone out. Everything after the flushed
          // one goes whole again - by then the ramp has rebuilt a lead that covers a full chunk.
          if (!pending.length) next++;
          continue;
        }

        // Everything is scheduled; speak() must not resolve until it has actually been heard. The
        // speed can still change in here - a page's tail is up to the lead cap wide - and the check
        // at the top of the loop is what picks that up, which is why this is one loop and not two.
        const s = await sendToOffscreen({ t: 'audio-status', epoch });
        if (s.stale) return;
        if ((s.queued ?? 0) === 0 && (s.lead ?? 0) <= 0.05) return;
        await sleep(Math.min(1000, (s.lead ?? 0) * 1000 + 100));
      }
    } finally {
      // However this ended - drained, stopped, or thrown - the listener has to go, or a page's
      // worth of them accumulates on the worker and every later mark is delivered many times.
      unsubscribe?.();
      // A pull abandoned by Stop is nobody's to await now, so it must not report a rejection into
      // the void - an unhandled one is a console error on a path that worked.
      void pull?.catch(() => {});
      // The feed is driven by hand rather than by `for await`, so closing it is by hand too.
      await feed.return?.();
    }
  }

  stop(): void {
    tellOffscreen('audio-stop');
  }
  pause(): void {
    tellOffscreen('audio-pause');
  }
  resume(): void {
    tellOffscreen('audio-resume');
  }
}
