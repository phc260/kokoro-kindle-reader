// Client for the offscreen document's audio graph: the sender, the lead cap, the throttle loop.
//
// Separate from kokoro-http.ts because scheduling and backpressure are not transport concerns —
// they were extracted when there were two narrators sharing one AudioContext, and the split has
// outlived that: the pacing rules are stated once here, where they can be read without wading
// through the fetch code, and offscreen.ts is the only other place that needs to agree.
//
// See offscreen.ts for the other end.

/**
 * Cap on scheduled-but-unheard audio, in seconds. The host synthesizes several times faster than
 * the ear consumes, so without a cap a page renders to the end long before a minute of it has
 * been heard — megabytes of AudioBuffers, all discarded on Stop. Big enough to cover any one
 * chunk's synthesis several times over; small enough that Stop wastes at most this much work.
 */
export const MAX_LEAD_S = 30;

export const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

export interface OffscreenReply {
  ok: boolean;
  error?: string;
  /** Set when the message carried an epoch that is no longer the current one. */
  stale?: boolean;
  epoch?: number;
  /** Seconds of audio scheduled but not yet heard. */
  lead?: number;
  queued?: number;
}

/** Send one message to the offscreen document, throwing if it reports failure. */
export async function sendToOffscreen(msg: Record<string, unknown>): Promise<OffscreenReply> {
  const reply = (await chrome.runtime.sendMessage({ ...msg, target: 'offscreen' })) as
    | OffscreenReply
    | undefined;
  if (!reply) throw new Error('no reply from the offscreen document');
  if (reply.ok === false) throw new Error(reply.error ?? 'offscreen audio failed');
  return reply;
}

/**
 * Fire-and-forget transport control (stop/pause/resume). Deliberately swallows failure: these
 * are called from `void`-returning methods with no caller to catch, so `sendToOffscreen`'s throw
 * would surface as an unhandled rejection in the service worker. And the only ways it fails —
 * the offscreen document is gone, or playback already stopped — both mean the audio is not
 * playing, which is what the caller wanted.
 */
export function tellOffscreen(t: string): void {
  void chrome.runtime.sendMessage({ target: 'offscreen', t }).catch(() => {});
}

/** Begin a playback generation. Called ONCE per page — see offscreen.ts's `startStream`. */
export async function startStream(): Promise<number> {
  const r = await sendToOffscreen({ t: 'audio-start' });
  return r.epoch ?? 0;
}

/** One word coming due, as the offscreen document's audio clock reports it. */
export interface WordMarkMessage {
  /** Index of the chunk this offset is measured against. */
  chunk: number;
  charIndex: number;
  charLength: number;
}

/**
 * Word marks for ONE playback generation. Returns an unsubscribe.
 *
 * These arrive as broadcasts rather than replies: a mark is due when the audio reaches it, which
 * is long after the request that scheduled the chunk was answered. The epoch filter is what keeps
 * a superseded page from moving the current page's highlight - marks for a stopped generation
 * are dropped at the source too, but a message already in flight when Stop lands would otherwise
 * still be delivered.
 */
export function onWordMarks(epoch: number, cb: (m: WordMarkMessage) => void): () => void {
  const listener = (msg: { t?: string; epoch?: number } & Partial<WordMarkMessage>): void => {
    if (msg?.t !== 'kwr-word' || msg.epoch !== epoch) return;
    cb({ chunk: msg.chunk ?? 0, charIndex: msg.charIndex ?? 0, charLength: msg.charLength ?? 0 });
  };
  chrome.runtime.onMessage.addListener(listener);
  return () => chrome.runtime.onMessage.removeListener(listener);
}

/**
 * Block while the buffer is full. Returns true if playback was cancelled meanwhile.
 *
 * Also what keeps the MV3 service worker alive mid-page: a message every couple of seconds
 * resets its idle timer.
 */
export async function waitForRoom(epoch: number): Promise<boolean> {
  for (;;) {
    const s = await sendToOffscreen({ t: 'audio-status', epoch });
    if (s.stale) return true;
    const lead = s.lead ?? 0;
    if (lead < MAX_LEAD_S) return false;
    await sleep(Math.min(2000, (lead - MAX_LEAD_S) * 1000 + 250));
  }
}
