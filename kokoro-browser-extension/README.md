# kokoro-browser-extension

Reads **Kindle Cloud Reader** (`read.amazon.com`) aloud with Kokoro, by capturing the rendered
page, OCR'ing it locally, and narrating it through `kokoro-host`.

```
read.amazon.com  ──capture──▶  Tesseract (offscreen doc)  ──text──▶  narrator
                                                                        │
                                        loopback HTTP 127.0.0.1:8787 ───┘
                                                     │
                                        kokoro-host (webserve.rs) ──▶ Kokoro-82M
```

Nothing leaves the machine. The OCR runs in WASM in the browser; the synthesis runs in the tray
app you already have installed for Kindle for PC.

## Why OCR at all

Kindle Cloud Reader renders the book to a canvas. There is no selectable text to read — the
page is pixels by the time the extension can see it. So the pipeline captures the canvas and
OCRs it, which is also why `vendor/` is 17 MB and why a first page is slower than the rest.

## Setup

**1. Build.** `dist/` is not committed; regenerate it:

```bash
bun install && bun run build.ts --stage
```

`--stage` copies the Chrome build to `~/kokoro-ext`. Use it if this repo lives on a removable
or network drive — Chrome loads unpacked extensions from those poorly, and the symptom is a
reload that spins forever with no error on the card. The manifest `key` keeps the extension id
stable across the move.

**2. Load it.** `chrome://extensions` → Developer mode → Load unpacked → the `dist/chrome`
folder (or `~/kokoro-ext`). Confirm the id shown is `acbnkbiijeckelpogcboafgllhccbngm`; if it
isn't, the `key` is missing and pairing will 403.

**3. Pair.** With Kokoro Kindle Reader running, right-click the tray icon → **Web pairing
code**, and paste the `pairing` value into the extension's options page. Once per browser.

Without step 3 the extension still works — it falls back to the platform TTS voice and says so
in the panel's status line. That is the intended degradation, not a failure.

## The transport

**Loopback HTTP, and nothing else.** A native-messaging bridge was built and tried ahead of it,
then removed. Two transports meant every failure had to be diagnosed twice, and the half that
broke was never the half you were looking at. HTTP needs no per-browser registration, is the
only route that works in Firefox, and can be reproduced with `curl`:

```bash
curl -H "Authorization: Bearer $TOKEN" -H "Origin: chrome-extension://$ID" http://127.0.0.1:8787/status
```

That is the whole transport. If that command works and the extension doesn't, the bug is in the
extension.

The daemon's four checks (127.0.0.1 bind, origin allowlist, constant-time bearer token, `Host`
header) live in [`kokoro-host/src/webserve.rs`](../kokoro-host/src/webserve.rs).

## Layout

| Path | What it does |
|---|---|
| `src/content/` | Runs on the page: capture, OCR client, the on-page panel, narration driver |
| `src/background.ts` | Service worker. Picks the engine (Kokoro if paired, else `chrome.tts`) and owns the port to the page |
| `src/offscreen.ts` | Offscreen document: the Tesseract worker **and** the AudioContext — a service worker has neither |
| `src/offscreen-client.ts` | The pacing rules: lead cap, throttle loop, epoch handling |
| `src/kokoro-http.ts` | The narrator, pairing storage, and the daemon probe |
| `scripts/make-key.ts` | Regenerates the pinned extension identity. Read its header before running it |

**Firefox ships no background script or offscreen document** (`build.ts`'s `skip` map): it has
neither `chrome.tts` nor `chrome.offscreen`. A content script there can `fetch` and own its own
`AudioContext` directly, which is exactly why the HTTP transport is the one that reaches it.

## Checks

```bash
bun run typecheck     # tsc --noEmit
bun test test/        # chunking, voices, tar, and manifest/permission drift guards
```

The manifest tests exist because permission drift fails **silently**: Chrome simply omits the
API, the feature-detect returns false, and a fallback engages with nothing logged anywhere.

## Gotchas

- **`key` and `gecko.id` are identity, not configuration.** Changing either changes the
  extension id, which the daemon's origin allowlist matches — every request then 403s. They are
  called out as do-not-touch in `.claude/commands/bump-version.md`.
- **`kwr` is in the isolated world.** In DevTools, switch the console's context dropdown from
  "top" to "Kokoro Kindle Reader" or the global isn't there.
- **Icons are not stored here.** `build.ts` copies `icons/32x32.png` and `128x128.png` from the
  repo root so the toolbar and the tray can't drift apart. They are in Git LFS.
