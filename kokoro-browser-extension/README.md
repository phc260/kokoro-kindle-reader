# kokoro-browser-extension

Reads **Kindle Cloud Reader** (`read.amazon.com`) aloud with Kokoro, by capturing the rendered
page, OCR'ing it locally, and narrating it through `kokoro-host`.

```
read.amazon.com  ──capture──▶  Tesseract (offscreen doc)  ──text──▶  narrator
       ▲                              │                                 │
       └── highlight ◀── word boxes ◀──┘       loopback HTTP 127.0.0.1:8787
                    ◀── word marks ◀──────────────────┴──┐
                                                         │
                                        kokoro-host (webserve.rs) ──▶ Kokoro-82M
```

Nothing leaves the machine. The OCR runs in WASM in the browser; the synthesis runs in the tray
app you already have installed for Kindle for PC.

## Why OCR at all

Kindle Cloud Reader renders the book to a canvas. There is no selectable text to read — the
page is pixels by the time the extension can see it. So the pipeline captures the canvas and
OCRs it, which is also why `vendor/` is 17 MB and why a first page is slower than the rest.

It is also why the **word highlight** is a box floated over the page rather than a styled range:
there is no text node to wrap and no selection to set. Tesseract already reports a bounding box
per word, so the mark is that box mapped through the image's live on-screen rect.

## A page is read a column at a time

A two-column page is recognized one column at a time, and the first is spoken while the second is
still being OCR'd — about a second off the wait for the first word, on a wait of four or so.

That only works because the columns feed **one utterance**. A second `speak` would start a second
audio stream, and starting one tears the first down (`startStream` in `src/offscreen.ts`), so the
page would go silent for as long as the new stream's first chunk takes to synthesize — roughly as
long as the wait that was saved. So the text goes over the port in parts (`{t:'part'}`) into a
single `PartQueue`, and the late chunks schedule onto the cursor already running.

Two rules keep the seam from being audible or visible:

- **Parts are cut at a sentence end, not at the column boundary.** A column nearly always runs
  into the next one mid-sentence, and each part is chunked and synthesized separately, so cutting
  there would break the delivery in the middle of a sentence. The tail is carried and yielded with
  the column that continues it.
- **Every part states its `base`** — where it sits in the page's text — rather than that being
  inferred from a running total of part lengths. The two do not agree, precisely because of the
  sentence cut above, and a boundary from the second column has to address the same string the
  highlight indexed.

`kwr.readPage()` still recognizes the whole page in one go; it has nothing to overlap with.

## Turning the page

Play reads on: when a page finishes, `turnPage()` advances the reader and the next page is
captured, OCR'd and spoken.

**One action does it: `ArrowRight`.** That is the reader's own shortcut — its pages turn on the
left and right arrows — and it works with the browser window minimized, which nothing that depends
on hit-testing a point can promise. It is dispatched on the page image, so a `composed` event
bubbles out through every shadow root to whatever is listening, with `keyCode` set by hand
(handlers still branch on it and the constructor leaves it at 0).

Clicking a next-page control found by accessible name, and tapping the forward half of the page,
both worked in the fixture and are gone. They could only ever have run once the key had already
failed — which is exactly when firing more untested actions at the reader is least wise — and a
second path that runs only in the case you cannot reproduce is the trap this project already
refuses for [transports](#the-transport).

**A turn is claimed only on evidence: a new `blob:` URL at an unchanged layout.** The keypress
having gone out is not a turn. Nor is a new URL on its own: the reader renders to the viewport, so
a resize or a zoom gives the *same* page a fresh URL with the text reflowed — the thing
`followReflow` exists for — and counting one as a turn would have the loop OCR and narrate the page
it just read. A render is rejected only on positive proof that the layout changed under it: the
viewport or the rendered size actually differing.

Those two do not catch everything, and the code says which case they miss: a **font-size** change
reflows the page at the same viewport and the same rendered size, so it looks exactly like a turn.
Nothing cheap can tell those apart — the only difference is the text, and reading that is an OCR
pass. What keeps it from costing a page is that an accepted turn is handed back only once the page
**holds still**: if the turn we asked for lands behind a re-render that was mistaken for it, the
caller still captures the render that stayed, instead of narrating a page that is already gone.

Nothing advancing is what the **last page of the book** looks like — and, indistinguishably, what a
reader that has stopped answering the arrow keys looks like. Both get the same treatment: the loop
says so and waits for a page turn by hand, which then carries on from wherever the reader actually
is, and which is also what absorbs a turn that was merely slow. That fallback is a path the reader
uses, not a spare one kept warm.

## Where the word boundaries come from

The highlight needs to know *when* each word is said, and the two kinds of engine answer that
very differently.

- **The platform engines report it.** `chrome.tts` fires a `word` event and `speechSynthesis`
  fires `onboundary`, both carrying a character offset. Nothing to compute.
- **Kokoro does not.** `POST /synth` returns a block of f32 PCM and nothing else. The model
  predicts a duration per phoneme internally, but the stock `model.onnx` exposes only the
  waveform output, so there is no alignment to ask for short of re-exporting the model.

So on the Kokoro path the marks are **estimated**, in `src/word-timing.ts`: the chunk's duration
is known exactly (it is the sample count), and that duration is split across the chunk's words by
syllable count plus a beat for whatever punctuation follows each one. Two things keep an estimate
usable:

1. **The error resets every chunk.** A chunk is one to four sentences, so nothing accumulates
   across a page — the worst case is being a word out mid-chunk, not a paragraph out at the foot
   of the page.
2. **The marks run on the audio clock, not on `setTimeout`.** They are scheduled in
   `AudioContext` time inside the offscreen document, so Pause freezes the highlight on the word
   being spoken and Resume carries on from it. A wall-clock timer would have run the highlight to
   the end of the page while the audio sat suspended.

Offsets are mapped back to the page by counting **non-whitespace characters**, not by summing
chunk lengths — `chunk()` only ever drops or normalizes whitespace, so the ink is an exact
alignment where the running total drifts a character at every paragraph break.

## Changing the speed while it reads

The lead that makes playback gapless is what makes the speed slider hard: by the time you move it,
up to `MAX_LEAD_S` (30) seconds of the page has **already been rendered at the old speed**. And
Kokoro's `speed` is a synthesis parameter — the model predicts shorter phoneme durations, which is
why speeding up does not raise the pitch — so those samples cannot be adjusted afterwards. (Web
Audio's `playbackRate` could, and would sound like a chipmunk doing it.)

So a change is applied in two steps, both of them necessary:

1. **The rate is read per chunk, from a live object.** The panel keeps one `SpeakOptions` object and
   writes `rate` into it; `speakAll` reads it as it sends each chunk. Over the port that object is
   cloned at `speak` time, so the worker is told separately (`{t:'options', rate}`) and writes into
   its own copy. Before this the panel built a fresh object per Play, which froze the speed for the
   whole book — the slider moved and nothing happened, ever.
2. **The unheard tail is discarded and asked for again.** `audio-retune` stops every source that has
   not started, rewinds the cursor to the end of the chunk being heard, drops that audio's word
   marks, and reports where synthesis has to pick up; the send loop resumes from there, at the
   **same chunk indices**, so the boundaries a re-sent chunk produces still remap onto the same
   words.
3. **The first chunk after the flush re-enters `PLAYBACK_RAMP`.** A flush hands back the lead, which
   puts synthesis in exactly the state it is in at the start of a page — nothing buffered — and a
   settled-size chunk takes ~5.8 s to render. Sending one whole meant that whenever the chunk still
   playing had less than that left, you heard a silence **one to two sentences long**. So the chunk
   is re-sent in ramp-sized pieces, cut by `chunk()` at sentence and clause ends: the first renders
   in well under a second and the rest grow back to settled behind it. Chunks after that one go out
   whole again, because by then the lead covers a full chunk.

A piece carries the character `offset` where it starts within its chunk, and the offscreen document
adds that to every word mark it derives. So a boundary is always addressed to the whole chunk and the
narrator's `remap` never learns that pieces exist. It is also what lets a *second* speed change land
mid-ramp without repeating a sentence: the flush reports chunk **and** offset, so it resumes at the
piece after the one being heard rather than at the top of a part-heard chunk.

The chunk playing when you let go of the slider finishes at the old speed — cutting it mid-word
would be an audible click in exchange for a second or two.

For "when you let go" to be true, every wait between the slider and the flush has to be able to
notice it. The send loop spends its time in two: the throttle, once the lead is full, and the pull
for more text — which on a two-column page is the whole time the second column is being recognized,
and has no upper bound at all. The throttle's nap is sliced and the pull is raced, and the raced
pull is **held** rather than asked for again, because an iterator's value is consumed by the call
that produced it. The service worker publishes the live options *before* it picks an engine for the
same reason: on a first Play that step connects to the daemon and loads the model, and a slider
moved inside those seconds would find nothing to write to.

The retune fires on the slider's `change`, not `input`: a drag emits dozens of `input` events and
each committed value costs a real re-synthesis on the one synth worker Kindle also queues behind.
The readout follows the drag; the audio changes when you let go.

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
only transport that *could* reach Firefox (see below), and can be reproduced with `curl`:

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
| `src/content/highlight.ts` | The word mark: `charIndex` → OCR word → bbox → screen rect, in its own shadow root. Relocates the word when a resize reflows the page |
| `src/background.ts` | Service worker. Picks the engine (Kokoro if paired, else `chrome.tts`), owns the port to the page, and feeds a page's parts into one utterance |
| `src/speak.ts` | The narration seam: chunk schedule, the part stream and its offset mapping, where a part may be cut |
| `src/offscreen.ts` | Offscreen document: the Tesseract worker **and** the AudioContext — a service worker has neither. Fires the word marks off the audio clock, and gives back the lead on a speed change |
| `src/offscreen-client.ts` | The pacing rules: lead cap, throttle loop, epoch handling, word-mark subscription, and the waits a speed change has to be able to cut short |
| `src/word-timing.ts` | Splits a chunk's known duration across its words. Kokoro's only source of boundaries |
| `src/kokoro-http.ts` | The narrator, pairing storage, and the daemon probe |
| `scripts/make-key.ts` | Regenerates the pinned extension identity. Read its header before running it |

## Browser support

**Chrome and Edge are supported. Firefox is not — the build exists but has no Kokoro path.**

Firefox ships no background script or offscreen document (`build.ts`'s `skip` map), because it
has neither `chrome.tts` nor `chrome.offscreen`. But `getNarrator()` reaches the backend
*through* that worker, so with no worker there is nothing wired to the HTTP endpoint. The
Firefox build falls back to `speechSynthesis` and narrates in a platform voice.

The architecture does not prevent Firefox — a content script there can `fetch` and own its own
`AudioContext` directly, which is exactly the shape the HTTP transport allows and native
messaging never could. Finishing it means a content-script narrator that reads the pairing from
`chrome.storage` and posts to `/synth` itself. That is unbuilt and untested; don't describe
Firefox as working until it is.

## Checks

```bash
bun run typecheck     # tsc --noEmit
bun test test/        # chunking + offsets, part streaming, word timing, word lookup, furniture,
                      # voices, tar, the speed-change re-send, and manifest/permission drift guards
```

The manifest tests exist because permission drift fails **silently**: Chrome simply omits the
API, the feature-detect returns false, and a fallback engages with nothing logged anywhere.

## Gotchas

- **`key` and `gecko.id` are identity, not configuration.** Changing either changes the
  extension id, which the daemon's origin allowlist matches — every request then 403s. They are
  called out as do-not-touch in `.claude/commands/bump-version.md`.
- **`kwr` is in the isolated world.** In DevTools, switch the console's context dropdown from
  "top" to "Kokoro Kindle Reader" or the global isn't there.
- **A line missing from the narration is a furniture drop, not a synthesis bug.** `ocr.ts`
  removes running heads and folios, and they OCR *perfectly* — so no confidence or accuracy check
  can point at a mistake. Every page logs what it withheld (`[kwr] not narrated: …`) with the
  reason; that log is the first place to look.
- **A running head is read once per session, then never again — and so is the first page number.**
  That is deliberate, not a bug. A section heading and a running head are the same object on the
  page — short, near the edge, no closing punctuation — so nothing is removed on a first sighting;
  it has to turn up on *another* page first. A folio is caught by its **slot** rather than its
  text, since the number changes every page. Guessing from appearance instead cost four real
  section headings on a chapter-per-page layout, silently. Use `resetFurnitureMemory()` when
  switching books.
- **A page read across a missed gutter is detected and read again.** `findGutter` needs a band
  free of ink over almost the whole page height, so one figure or rule crossing the gutter hides
  it — and Tesseract then joins the columns line by line into sentences that are fluent and wrong,
  with every word real and the confidence high. `looksInterleaved` spots the huge mid-line gap
  that justification never produces and asks for a second look with the test relaxed.
- **The highlight only draws on the page it was measured on.** The boxes belong to one specific
  render, and the reader swaps the whole bitmap on a page turn, so the captured `blob:` URL is
  checked before every draw. A mark that never appears usually means the page turned under the
  OCR, not that the lookup is wrong.
- **A resize is a re-render, not a rescale.** The reader renders to the viewport, so resizing or
  zooming produces a new `blob:` URL with the text reflowed. `followReflow` re-OCRs it and
  `relocate` finds the current word again by matching its neighbours — but the narration is not
  re-anchored, because it is still speaking the text captured when the page started. A word the
  reflow pushed onto the next page simply goes unmarked until one that is still visible comes up.
- **Icons are not stored here.** `build.ts` copies `icons/32x32.png` and `128x128.png` from the
  repo root so the toolbar and the tray can't drift apart. They are in Git LFS.
