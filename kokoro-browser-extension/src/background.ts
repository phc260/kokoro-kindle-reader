// Service worker. Owns chrome.tts, which is not exposed to content scripts, and bridges it to
// the page over a long-lived port so word-boundary events can stream back as they happen.
//
// Protocol (content -> worker):  {t:'speak', text, options} | {t:'stop'} | {t:'pause'}
//                                {t:'resume'} | {t:'voices'}
//          (worker -> content):  {t:'word', ...} | {t:'end'} | {t:'error', message}
//                                {t:'voices', voices}
//
// When the Kokoro backend lands this is also where the native port and the audio graph live,
// for the reason given in ARCHITECTURE.md: the worker outlives a content-script reload, so
// navigation inside the reader cannot interrupt playback.

import { ChromeTtsNarrator, speakChunked, type Narrator, type SpeakOptions } from './speak';
import { KokoroHttpNarrator, describeProbe, loadPairing, probeDaemon } from './kokoro-http';

// --- engine selection ----------------------------------------------------------------------
// Kokoro over the tray app's loopback HTTP endpoint if one is paired, else the platform engine.
// Decided once and remembered, so a machine without the backend does not pay a failed probe per
// utterance.
//
// ONE transport, deliberately. A native-messaging bridge was tried ahead of this and removed
// rather than kept as a second route: two transports mean every failure has to be diagnosed
// twice, and the half that broke is never the half you are looking at. HTTP is the one that
// needs no per-browser registration, works outside a Chrome service worker, and can be
// reproduced with curl.
const platform = new ChromeTtsNarrator();
let chosen: Narrator | null = null;
let choosing: Promise<Narrator> | null = null;
/** Why Kokoro was not used, if it wasn't. Reported to the UI - see the `kokoro` port message. */
let engineError: string | null = null;

type Ready = { voice: string; voices: string[]; sampleRate: number };

/**
 * Connect to the paired daemon, or throw one sentence naming the actual next action.
 *
 * When nothing is paired the probe is what makes the message useful: "no daemon on :8787, start
 * the tray app" and "daemon is up, go paste the pairing code" are different problems, and
 * guessing between them is most of the time anyone loses here.
 */
async function connectKokoro(): Promise<{ narrator: KokoroHttpNarrator; ready: Ready }> {
  const pairing = await loadPairing().catch(() => null);
  if (!pairing) throw new Error(describeProbe(await probeDaemon()));

  await ensureOffscreen(); // audio has to have somewhere to play before we commit
  const narrator = new KokoroHttpNarrator(pairing);
  const ready = await narrator.status(); // handshake, and the warm-up that loads the model
  return { narrator, ready };
}

async function narratorFor(): Promise<Narrator> {
  if (chosen) return chosen;
  choosing ??= (async () => {
    try {
      const { narrator, ready } = await connectKokoro();
      console.log('[kwr] kokoro-host ready over HTTP:', narrator.pairing.base, ready.voices.length, 'voices');
      engineError = null; // a transport that WORKED must not leave an earlier failure on screen
      chosen = narrator;
    } catch (e) {
      // Not paired, tray app not running, token gone stale - all land here, and all mean "say
      // why and use the platform voice" rather than "fail to narrate".
      engineError = String(e);
      console.warn('[kwr] kokoro unavailable:', engineError);
      chosen = platform;
    }
    return chosen;
  })().finally(() => {
    choosing = null;
  });
  return choosing;
}

/** Force an engine; returns its kind. Selecting Kokoro reconnects, so it can fail. */
async function useEngine(which: 'kokoro' | 'platform'): Promise<string> {
  if (which === 'platform') {
    chosen = platform;
    return platform.kind;
  }
  const { narrator } = await connectKokoro();
  engineError = null;
  chosen = narrator;
  return narrator.kind;
}

// --- offscreen OCR host -------------------------------------------------------------------
// The content script cannot spawn Tesseract's worker itself (cross-origin worker script), so
// OCR runs in an offscreen document. This creates it on demand and relays requests to it.

let creating: Promise<void> | null = null;

async function ensureOffscreen(): Promise<void> {
  if (!chrome.offscreen) throw new Error('chrome.offscreen unavailable in this browser');
  if (await chrome.offscreen.hasDocument()) return;

  // createDocument throws if called twice concurrently, which two quick page turns will do.
  creating ??= chrome.offscreen
    .createDocument({
      url: 'offscreen.html',
      // WORKERS: Tesseract's worker. AUDIO_PLAYBACK: a service worker has no AudioContext,
      // so Kokoro's PCM is scheduled here too.
      reasons: [chrome.offscreen.Reason.WORKERS, chrome.offscreen.Reason.AUDIO_PLAYBACK],
      justification: 'Run the Tesseract OCR worker and play synthesized audio.',
    })
    .finally(() => {
      creating = null;
    });

  await creating;
}

chrome.runtime.onMessage.addListener((msg: { t?: string; b64?: string; type?: string }, _sender, sendResponse) => {
  if (msg?.t !== 'ocr') return;

  (async () => {
    await ensureOffscreen();
    const reply = await chrome.runtime.sendMessage({ t: 'ocr-run', target: 'offscreen', b64: msg.b64, type: msg.type });
    sendResponse(reply);
  })().catch((e) => sendResponse({ ok: false, error: String(e) }));

  return true; // async reply
});

chrome.runtime.onConnect.addListener((port) => {
  if (port.name !== 'narrate') return;

  let generation = 0;

  port.onMessage.addListener(async (msg: { t: string; id?: string; text?: string; options?: SpeakOptions; which?: string }) => {
    try {
      switch (msg.t) {
        case 'voices': {
          const n = await narratorFor();
          port.postMessage({ t: 'voices', voices: await n.voices(), engine: n.kind, engineError });
          break;
        }

        // Direct probe, so the reason a fallback happened is reachable from the page console
        // instead of only from the service worker's own devtools.
        case 'kokoro': {
          chosen = null; // force a fresh attempt rather than reporting a cached decision
          engineError = null;
          try {
            const { narrator, ready } = await connectKokoro();
            chosen = narrator;
            port.postMessage({ t: 'kokoro', ok: true, ready });
          } catch (e) {
            engineError = String(e);
            const paired = await loadPairing().catch(() => null);
            port.postMessage({ t: 'kokoro', ok: false, error: engineError,
              hint: paired ? `paired with ${paired.base}` : 'not paired - see the options page' });
          }
          break;
        }

        case 'engine':
          port.postMessage({ t: 'engine', engine: await useEngine(msg.which === 'kokoro' ? 'kokoro' : 'platform') });
          break;

        // The whole page arrives as one string and is chunked HERE, not in the content script:
        // the chunk schedule depends on which engine got picked, and only this side knows that.
        case 'speak': {
          const mine = ++generation;
          const n = await narratorFor();
          await speakChunked(n, msg.text ?? '', msg.options, (b) => {
            // A stale utterance can still emit a boundary after being superseded; drop it
            // rather than let it move the highlight backwards.
            if (mine === generation) port.postMessage({ t: 'word', id: msg.id, ...b });
          });
          // Always answer, even when superseded: a cancelled utterance still has a caller
          // awaiting it, and `Narrator.speak` resolves rather than rejects when stopped.
          port.postMessage({ t: 'end', id: msg.id });
          break;
        }

        case 'stop':
          generation++;
          (chosen ?? platform).stop();
          break;

        case 'pause':
          (chosen ?? platform).pause();
          break;

        case 'resume':
          (chosen ?? platform).resume();
          break;
      }
    } catch (e) {
      port.postMessage({ t: 'error', id: msg.id, message: String(e) });
    }
  });

  port.onDisconnect.addListener(() => {
    generation++;
    (chosen ?? platform).stop();
  });
});
