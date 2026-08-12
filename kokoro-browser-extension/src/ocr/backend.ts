// The `POST /ocr` transport, and nothing else.
//
// This is the whole of what recognition costs the extension now: one authenticated fetch and the
// unpacking of one JSON reply. It was an OCR ENGINE - a wasm build, its language data, and a
// CSP relaxation to run it - and the size of this file against that is the point of the move.
//
// THERE IS NO FALLBACK AND THERE MUST NOT BE ONE. A missing or unhealthy host is a state to
// report, not a reason to run a second engine nobody has measured against these fixtures. The
// in-page route this file's ancestor kept existed because the old engine's worker had to be
// same-origin; today it would be the same fetch from the wrong origin, and a "fallback" that
// cannot work turns one legible failure into two - only ever in the case nobody can reproduce.
//
// WHAT IS POSTED IS THE PAGE AS THE READER RENDERED IT - original colour, neither flattened nor
// inverted. That is a reversal, and the reason is the engine change: the old engine's quality
// guide asked for dark ink on a light ground, so this used to hand it a grayscale, possibly
// inverted page. A detector whose job is to find four words inside an illustration needs every
// bit of that discarded contrast, and engine-specific preprocessing is the backend's to own.
// Checked on a rendered dark-theme fixture - light grey serif on near-black paper reads
// perfectly through the backend with no inversion at all, at full confidence. If a real dark
// capture ever fails, the fix goes in the BACKEND, next to the models that want it.
//
// Rectangles come back in the coordinate space of the image that was posted; `../ocr` adds the
// column's x offset exactly once.

/**
 * Where the OCR backend is, and what proves this client may use it.
 *
 * Passed in on every call rather than read from `chrome.storage` here. Two reasons, and the
 * second is the one that matters: this module stays free of any `chrome` dependency, so the
 * furniture rules can still be tested under bun without a browser; and the pairing already
 * travels this way for `/synth` (see the `http-synth` message), so there is one answer to
 * "who knows the token" instead of two.
 */
export interface OcrBackend {
  base: string;
  token: string;
}

/** One word exactly as the host reports it. The wire shape, before any policy touches it. */
export interface RawWord {
  text: string;
  confidence: number;
  bbox: { x0: number; y0: number; x1: number; y1: number };
}

/**
 * The version-1 `/ocr` response, frozen with the host before this adapter was written.
 *
 * `version` is in it so a host and an extension that disagree say so, instead of the mismatch
 * arriving as an undefined field halfway down a page.
 */
interface HostResponse {
  version: number;
  engine: string;
  /** Both models, named separately - either can be repinned without the other. */
  detector: string;
  recognizer: string;
  width: number;
  height: number;
  lines: { words?: RawWord[] }[];
  detectMs: number;
  recognizeMs: number;
  ocrMs: number;
}

/** What this client can parse. Matches `RESPONSE_VERSION` in `kokoro-ocr`. */
export const RESPONSE_VERSION = 1;

/**
 * The posted size, in the unit the host's own cap is written in.
 *
 * It is in the message because an over-cap POST is the failure most likely to arrive with no
 * response at all: a client streams the body, the host answers and closes, and whether the reply
 * is ever read is a race the client loses. The host now drains before refusing, so the legible
 * 413 is what should turn up - and if one doesn't, this number is what says whether size was the
 * question. A full-page colour plate is where it matters; a page of prose is a fraction of a MiB.
 */
const mib = (bytes: number): string => `${(bytes / (1024 * 1024)).toFixed(2)} MiB`;

/**
 * Post one prepared column to the host and unpack it into lines of words.
 *
 * The only way a page is recognized. Line grouping arrives from the backend rather than being
 * reassembled here: the whole point of a line is that the furniture rules judge one, and the
 * engine is what knows where one ends.
 */
export async function recognizeViaHost(
  image: Blob,
  backend: OcrBackend,
  signal?: AbortSignal,
): Promise<RawWord[][]> {
  let res: Response;
  try {
    res = await fetch(`${backend.base}/ocr`, {
      method: 'POST',
      headers: { authorization: `Bearer ${backend.token}`, 'content-type': 'image/png' },
      body: image,
      signal,
    });
  } catch (e) {
    // A rejected fetch is the ONE failure that arrives with no status, no body and no URL -
    // `TypeError: Failed to fetch` and nothing else - so it is the one that has to be given
    // those facts here. An abort is Stop working and is left alone; the caller knows.
    if (signal?.aborted) throw e;
    throw new Error(`could not reach ${backend.base}/ocr (posted ${mib(image.size)}): ${String(e)}`);
  }

  if (!res.ok) throw new Error(await describeFailure(res, image.size));

  const data = (await res.json()) as HostResponse;
  if (data.version !== RESPONSE_VERSION)
    throw new Error(
      `the host speaks /ocr v${data.version}, this extension speaks v${RESPONSE_VERSION} - update both`,
    );

  // Rectangles are already in the coordinate space of the image that was posted, whatever
  // scale the backend used internally. The column x-offset is added exactly once, downstream.
  return data.lines.map((l) => l.words ?? []).filter((words) => words.some((w) => w.text?.trim()));
}

/**
 * Turn a failed response into a sentence naming the next action.
 *
 * The host distinguishes its failures on purpose - a model that will not load, a timeout and a
 * page it could not read are three different problems, and they map to `unavailable`, `timeout`
 * and `recognize`. Losing that distinction here would put the browser engine's worst property
 * back: one "OCR failed" for everything, so the user retries the one thing retrying cannot fix.
 */
async function describeFailure(res: Response, posted: number): Promise<string> {
  let code = '';
  let error = '';
  try {
    const body = (await res.json()) as { code?: string; error?: string };
    code = body.code ?? '';
    error = body.error ?? '';
  } catch {
    // A body that is not JSON is still a failure; the status carries the rest.
  }
  switch (res.status) {
    case 401:
      return 'the Kokoro host rejected the pairing token - re-pair from the options page';
    case 403:
      return 'the Kokoro host does not allow this extension id';
    case 413:
      // The host's message states the LIMIT; only this side knows what was actually sent, and
      // the gap between the two is the whole of what anyone can act on. A page of prose is a
      // fraction of a MiB, so a number far above the cap says the page is a full-colour plate
      // being re-encoded losslessly rather than that the cap is merely a little tight.
      return `this page encodes to ${mib(posted)}, over the Kokoro host's limit${error ? ` (${error})` : ''}`;
    case 429:
      return 'the Kokoro host is busy with other pages - try again in a moment';
    case 503:
      return `the Kokoro host cannot do OCR${error ? `: ${error}` : ''}`;
    default:
      return `ocr ${res.status}${code ? ` (${code})` : ''}${error ? `: ${error}` : ''}`;
  }
}
