# kokoro-ocr

PP-OCR detection + recognition for the Kindle Cloud Reader path. Page image in, lines of words
with rectangles out.

This is the engine only. It decides **nothing** about what gets narrated.

## Why two models rather than one engine

The first version of this crate was native Tesseract, and a single real Cloud Reader
picture-book page withdrew it: four sparse lines of large serif type in the corner of a
full-page illustration. Tesseract's own quality guide says its page segmentation expects a page
of text, and the fix would have been a growing pile of layout heuristics.

PP-OCR separates the two jobs by construction. A DBNet **detector** emits a text-probability
map over the page, and only the regions it finds are handed to a CTC **recognizer**. That also
fixes a product-policy defect Tesseract could not: it returns a running head and its folio as
**two** lines, so `repeatsAcrossPages` can match the head — where Tesseract merged them into
`FIELD-GUIDE TO HARBOURS 293`, whose text changes every page, so the head was narrated forever
(OCR_EXPERIMENT.md, Result 3).

## The boundary, and why it is here

The extension used to run Tesseract.js in a worker — ~17 MiB of wasm and language data in the
package, a `wasm-unsafe-eval` CSP allowance, and an engine re-warmed every browser session.
This crate is that recognition moved to the host, reached over one authenticated loopback
endpoint (`POST /ocr`, in [`kokoro-host/src/webserve.rs`](../kokoro-host/src/webserve.rs)).

What did **not** move, and must not:

| stays in `src/content/ocr.ts` | lives here |
|---|---|
| dark-page detection, gutter detection, column split | image decode, the model-specific resize and normalization |
| missed-gutter retry (`looksInterleaved` → re-split) | session lifetime, detection, recognition, reading order |
| furniture suppression + its cross-page memory | request bounds, scheduling, cancellation, engine health |
| text cleanup, character offsets, reflow relocation | model assets, pinning, integrity |

Those left-hand rules can silently remove a line of the book, and they were paid for in four
separate content losses. Swapping the engine underneath them is already the whole change;
moving them at the same time would make any regression impossible to attribute. So this crate
returns the **raw structure they already consume** — lines in reading order, each a list of
words with text, confidence and a rectangle — and nothing more.

A later shared-policy extraction may move preprocessing and furniture state in here for Linux
reuse. That is a separate, reviewed change.

## Layout

| file | what it is |
|---|---|
| `src/lib.rs` | the public API: `Rect`/`Word`/`Line`/`Page`, `Limits`, `Error`, `Assets`, the pinned digests, `probe()` |
| `src/engine.rs` | the bounded worker: queue, job lifecycle, `Ocr`/`JobHandle`, the deadline seams |
| `src/session.rs` | the two ONNX sessions and the dictionary that decodes what the second one emits |
| `src/prep.rs` | decode, bounds, crop, resample |
| `src/detect.rs` | DBNet post-processing: threshold, connected components, score gate, unclip, reading order |
| `src/recognize.rs` | line crop → 48 px → CTC greedy decode → words with x-positions |
| `examples/ocr-check.rs` | run the real models over one PNG, with no browser and no host |

## Things that are the way they are for a reason

- **The public API is target-neutral.** No HTTP, browser, Windows-UI, named-pipe or synthesis
  types appear in it, so the Linux blueprint reuses the crate unchanged and everything that
  knows about the transport stays in `webserve.rs`.
- **It does not initialize ONNX Runtime, and must not.** `ort`'s own guidance is that a library
  crate lets the application create the environment. The host does it in `main`, **before
  anything that could build a session is spawned** — not from whichever worker gets there first.
  That ordering is load-bearing: `native_synth::init_ort` names the staged DLL by absolute path,
  while `ort`'s lazy fallback honours `ORT_DYLIB_PATH` first, so a race between the two workers
  is a race between two different runtimes.
- **The `ort` dependency mirrors kokoro-host's exactly** — same `=2.0.0-rc.12` pin, same
  `load-dynamic`, same `default-features = false`. Two `ort` versions in one process would be
  two `OrtApi` tables against one library, and ort's defaults include *downloading* a runtime,
  which a host that stages its own must never do.
- **There is no feature switch and no stub engine.** Tesseract needed one: the FFI was a link
  dependency, so the crate could not build at all on a machine without a 9 MB native stack.
  These models are loaded at run time, so `cargo test` runs here with nothing provisioned and a
  host with nothing staged reports `missing`.
- **The image arrives in colour and is neither flattened nor inverted.** That is a reversal of
  the Tesseract path, which wanted dark ink on a light ground. A detector whose job is to find
  four words inside an illustration needs that contrast, and the models are trained on scenes
  rather than scans. Checked on a rendered dark-theme fixture: light-on-dark reads perfectly
  with no inversion. A real dark-theme Cloud Reader capture is still on the corpus gate.
- **There is no page-wide upscaling, and its absence is deliberate.** Under Tesseract, scale was
  the dominant accuracy lever — 16.19 % word error to 0.95 % from doubling the input alone —
  because Tesseract reads whatever resolution it is handed. The recognizer here resizes every
  detected line to a fixed 48 px height from the **source** pixels, so small type is upsampled
  for free, per line. A 2x in front of that would resample twice and quadruple the detector's
  input for nothing.
- **The detector's post-processing is a documented simplification.** Upstream takes contours,
  fits a minimum-area rectangle and offsets the polygon with a Vatti clip; this takes connected
  components and axis-aligned boxes with the equivalent offset for a rectangle. For horizontal
  book text they agree — the fitted rectangle IS the axis-aligned one when nothing is rotated.
  A skewed capture is where they part, and it is the first thing to revisit if a real page comes
  back wrong. It is also why the full v5 detector scored badly in the probe: that was evidence
  about this post-processing, not about the model.
- **A region is scored over its BOX, not over its own pixels.** Every pixel of a connected
  component is above the threshold by definition, so scoring those would score 1.0 for
  everything and throw the gate away. What tells a line of type from a hard edge in an
  illustration is how solidly the region is filled.
- **Reading order's "same line" band is half the page's own median box height**, not a pixel
  count. Reading order can silently reorder the book, so it runs on evidence the page provides:
  upstream's fixed 10 px is right at one capture size and wrong at every other, and the reader
  controls that size through the viewport.
- **Word boxes come from CTC timesteps.** Detection returns *line* boxes, but `hasOutlierGap`
  measures the gap between consecutive *words* — so an engine that could not produce word boxes
  could not drive the extension's policy at all. The timestep at which a character fires is its
  x-position, and the space class is what splits words. Measured against Tesseract's
  pixel-exact boxes for 170 words: x0 out by a mean of 1.78 px, x1 by 3.00 px.
- **The dictionary's leading empty sentinel is dropped and a space class is appended.** The
  first is the upstream file's own placeholder for the blank, which this decoder supplies at
  class 0 — keeping it would shift the entire alphabet by one and decode every character as its
  neighbour. The second is what `use_space_char` exported, and it is the only thing that
  separates words. The class count is re-checked against the model on every run, because a
  dictionary and a recognizer that disagree read out as fluent, confident, entirely wrong text.
- **Rectangles come back in the SUBMITTED image's coordinate space**, and the mapping rounds
  **outward** — floor the near edges, ceil the far ones. Rounding both the same way shrinks
  every box by up to a pixel a side, and these boxes are what `hasOutlierGap` measures word
  spacing with; systematically widening the gaps is how justified body text starts looking like
  a running head. It also clips ascenders off the crop the recognizer is given.
- **Two sessions, on a thread of their own.** Not the synth worker — OCR must not queue behind a
  page of narration, or the reverse — and off the Tokio runtime the pipe server shares.
- **The models are brought up lazily; `probe()` never builds a session.** `/status` is polled,
  and loading reads ~10 MiB and costs about a third of a second. Readiness is answered from the
  file system, and each digest is cached against that file's own length and mtime.
- **A failed load is not remembered.** The fix for `missing` is to put the file back; a host
  that had to be restarted to notice would cost more than the retry does. The worker catches a
  *panic* out of the load for the same reason: ORT's dylib resolution has no `Result` on the
  failure path, and an unwind would kill the worker for the life of the process — turning the
  one failure that is permanent into the retryable `Unavailable` all the others already are.
- **The digests gate the LOAD, and they gate the exact bytes loaded.** Verifying them in
  `probe()` alone made the pin decorative where it mattered: `/status` would answer `corrupt`
  while `/ocr` went on recognizing with whatever was on disk, so the two endpoints disagreed
  about whether the engine was usable and the permissive one was the dangerous one. A recognizer
  that is not the pinned one but happens to emit the same class count passes every other check
  here and returns fluent, plausible, entirely wrong text.
- **Calling `probe()` from the load path was the obvious fix and is not enough**, for two reasons
  that are one reason: *a path is not a file*. `probe`'s digest cache is keyed on length and
  mtime — right for a polled readiness report, wrong for a gate, since same-length bytes with a
  restored mtime read as verified — and even without the cache, nothing stops the file changing
  between the check and the `commit_from_file` that reopens it. So each file is read once, hashed
  as bytes, and the session and dictionary are built from that same buffer (`commit_from_memory`).
  What was verified is what runs, with nothing in between.
- **They are re-verified at run time, not only at install.** These are data reachable from a
  network-facing endpoint, and a digest an installer checked once stops being true the moment
  anything else writes to the directory.
- **Cancellation is a bounded discard contract, not a stop button.** ORT cannot abandon a run in
  progress, so the flag is checked between stages and before each line — which is finer than it
  sounds, since a page is one detection pass plus one inference per line. A job already inside a
  run finishes and its result is thrown away, safe precisely because the cross-page furniture
  memory is still in the extension, so a late result has nothing left to poison.
- **A deadline fails the page rather than returning part of it**, and it is checked *before*
  each line for that reason: a partial column narrated as a whole one is the book silently going
  missing.
- **PNG only.** The extension's `convertToBlob` emits PNG and nothing else, and every extra
  codec compiled in is another parser reachable from a network-facing endpoint by anyone holding
  the pairing token.
- **Limits are checked against the PNG header, before pixels exist.** A 40 KB file declaring
  30000x30000 is a 900 megapixel allocation that has already happened by the time a
  check-afterwards would run.

## The models

Two files and a dictionary, 9.80 MiB together, pinned by SHA-256 in `src/lib.rs` and in the
fetch script. Both sources are Apache-2.0 ONNX conversions of PaddleOCR models.

| local | upstream | size |
|---|---|---|
| `det.onnx` | `SWHL/RapidOCR` · `PP-OCRv4/en_PP-OCRv3_det_infer.onnx` | 2.31 MiB |
| `rec.onnx` | `ppu-paddle-ocr-models` · `.../en/v5/en_PP-OCRv5_mobile_rec_infer.onnx` | 7.49 MiB |
| `en_dict.txt` | `ppu-paddle-ocr-models` · `.../en/v5/ppocrv5_en_dict.txt` | 1,417 bytes |

CPU, by measurement: WebGPU was **2.27x slower** for warm recognition on this pair and returned
identical text and boxes. Recognition is still unbatched, so if that changes the provider has to
be measured again rather than inferred.

English only. Adding a language is a different recognizer, a different dictionary and a
different class count — not the file drop Tesseract made cheap.

## Building and testing

```powershell
# The unit tests need nothing provisioned - that is the point of them.
cargo test --manifest-path kokoro-ocr\Cargo.toml

# The models, once. Digests are verified on download and a mismatch is deleted, not kept.
native-deps\fetch-ocr-models.ps1

# End to end on one page, with no browser and no host.
$env:ORT_DYLIB_PATH = "native-deps\runtime\onnxruntime.dll"
cargo run --manifest-path kokoro-ocr\Cargo.toml --example ocr-check -- page.png native-deps\ocr
```

`kokoro-host` finds the models in `ocr\` beside its own exe (staged by the installer), falling
back to `native-deps\ocr` in a debug build so `cargo run` works without a copy step.
