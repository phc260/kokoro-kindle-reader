# Tests

Every automated test file in the repo, what it pins, and — for each suite — whether it still earns
its place. Written as an audit, not a brochure: where a suite is thin, unrunnable or absent, that
is recorded here rather than left to be discovered. Coverage is inventoried per file and per
module; individual test names are spelled out only where the distinctions matter (the furniture
rules, which can silently drop a line of the book).

**246 automated tests**, in three suites, plus seven harnesses that are run by hand.

| Suite | Tests | Runs where | Command |
|---|---|---|---|
| Browser extension | 145 in 14 files | `bun test`, no browser | `bun test test/` |
| `kokoro-ocr` | 54 | `cargo test`, **no models needed** | `cargo test --manifest-path kokoro-ocr\Cargo.toml` |
| `kokoro-host` | 47 | `cargo test` | `cargo test --manifest-path kokoro-host\Cargo.toml` |
| every other crate | **0** | — | — |

The three suites run in about four seconds combined and need no host, no Kindle, no models and no
network. That is the property worth protecting: this repo's real testing has always been "Preview
in the panel and Read Aloud in Kindle", and anything that cannot be checked without those has a
habit of not being checked at all.

---

## A standing rule for every fixture here

**Public domain or invented — never the book you are testing against.** The furniture rules and the
OCR ground truth both want realistic book prose, which makes the book currently open in the reader
the path of least resistance and the wrong answer. It happened once: a commentary reached six files
— running head, section headings, ~20 lines of prose, the publisher's name rendered into
`ocr-fixture.html`'s page image, and a real ASIN in `route.test.ts`. A real ASIN identifies a book
as precisely as its title does.

Nothing in this suite can detect it, because a fixture built from a real book passes every test that
exists. So the check belongs at the moment the text is written. What is already here and correct:
`ocr-fixture.html`'s ground truth is *Moby-Dick* (public domain), the furniture fixtures are an
invented harbour book, and `BENCH_TEXT` is an invented lighthouse sentence. Inventing is cheap
because a fixture needs the **shape** — word count, ink width, punctuation, whether the line ends a
sentence — and never the content.

## Browser extension — 145 tests

`bun test test/`. Pure `bun:test` files only; the harnesses that drive a browser are named
`.check.ts` so `bun test` never picks them up (they call `process.exit()`, which under `bun test`
would kill the run and silently skip every file after them).

### `furniture.test.ts` — 24 tests · `src/ocr/furniture.ts`, `src/ocr/lines.ts`

**The most important file here.** It covers the only rule in the pipeline that decides a line of
the book will not be read, and that rule fails silently in both directions: keep a running head and
it is spoken between every page; drop a heading and the book quietly loses part of itself. Furniture
OCRs *perfectly*, so no confidence or accuracy check can ever point at the mistake.

Four separate content losses in this rule's history are each pinned by a test here.

<details><summary>All 24</summary>

- a copyright line goes whatever height it sits at
- a folio is caught by its slot, because its text never repeats
- a lone number that is NOT in a folio slot is read
- a folio slot is a position, so the other corner is a different slot
- a sentence opening with a year is read
- a running head is read once, then never again
- a repeat has to be on ANOTHER page, not the same one read again
- section headings at the top of a column are read
- body text in the middle of the page is never furniture
- a long line in the band is body text, however near the edge it sits
- a short body line at the top of a column survives being read again
- a full-measure line at the top of a column is body text, however it ends
- the bottom band is treated exactly like the top one
- a repeated full-measure line is body text, not a running head
- a full-width title-and-folio header is not mistaken for justified text
- a justified line whose words the recognizer split still reads as body text
- the measure is the column, not its widest accident
- a reflowed page is the same page
- a different page is a different page
- a page read across a gutter is detected
- an ordinary justified page is not
- too little text to judge is left alone
- a discarded pass leaves the running-head memory untouched
- a re-render read for boxes only must not count as another page

</details>

**Verdict: keep, and treat as load-bearing.** Nothing here is redundant. Since the `src/ocr/` split
it imports from `backend`, `lines` and `furniture` by their own paths rather than through the
barrel, so a rule migrating between modules cannot go unnoticed.

### `voices.test.ts` — 20 tests · `src/voices.ts`

Voice ids into something choosable — accent, gender, name — and the quality ordering that decides
which voice is selected by default. Alphabetical order put `af_alloy` (grade C) above `af_heart`
(grade A), which made the default an accident of spelling.

**Verdict: keep.** Two of these are ratchets that would otherwise be rediscovered by ear: *every
shipped voice has a grade* and *the tree loses no voice*.

### `highlight.test.ts` — 15 tests · `src/content/highlight.ts`

`charIndex` → OCR word → bbox → screen rect, plus `relocate` (finding the word again after a resize
reflows the page). Covers the two refusals that matter: a word pushed off the page and an ambiguous
match both draw **nothing**, because a mark in the wrong place costs more than a missing one.

**Verdict: keep.** *the image rect is already viewport-space, so its offset is added once and only
once* is the kind of off-by-one no manual check would catch reliably.

### `narrate.test.ts` — 14 tests · `src/content/narrate.ts`

Port lifecycle — the one part of the narration path with real concurrency in it, and unreachable
through the module functions without a browser. Three of these were added this session, and two of
those exist because an earlier version of the test **passed without the fix**.

**Verdict: keep — and read the header before adding to it.** This file has produced vacuous tests
twice. Any new test here should be checked by deleting the fix and watching it fail.

### `stream.test.ts` — 14 tests · `src/speak.ts`

`PartQueue` and `sentenceEnd`: how a page's columns become one utterance. Covers every way the
queue can be closed — finish, Stop, superseded, disconnect — because missing one parks the worker
forever.

**Verdict: keep.** *a push after close is ignored, not queued forever* and *closing releases a
parked reader instead of hanging it* are hang bugs, which are the worst kind to debug live.

### `chunk.test.ts` — 11 tests · `src/speak.ts`

The ramp schedule and boundary→word mapping. *chunking is lossless once whitespace is normalized*
and *every boundary maps back to the exact word it names* are the two that matter; the rest fence
the ramp constants.

**Verdict: keep.**

### `retune.test.ts` — 9 tests · `src/offscreen-client.ts`, `src/kokoro-http.ts`

Live speed change: flushing up to 30 s of already-rendered lead and re-sending from the right place.
Every interruptible wait has a test, because *a change is only as prompt as the longest wait that
cannot see it*.

**Verdict: keep.** *a speed change during the wait for the next column is acted on, and costs no
text* guards the pull-is-held-never-reissued rule, where the failure is a lost chunk of book.

### `manifest.test.ts` — 7 tests · `manifest.chrome.json`, `manifest.firefox.json`

Manifest ↔ code drift. Three of these are **ratchets against re-adding what has been removed**:
*neither manifest still asks for nativeMessaging*, *the package grants nothing the departed OCR
engine needed* (asserts no `wasm-unsafe-eval`), and *the offscreen document no longer claims a
WORKERS reason*.

**Verdict: keep, specifically because they are negative.** A permission or CSP relaxation kept for
a thing that left is a standing invitation with nothing behind it, and nothing else in the repo
would notice one coming back.

### `route.test.ts` — 6 tests · `src/content/capture.ts`

Where the panel mounts. Two shapes: the reader is a query param (`?asin=`), the library is a path
(`/kindle-library`). Getting either wrong fails silently and identically — the panel simply never
appears, on a page that looks exactly like the one it should have appeared on.

**Verdict: keep.** Writing it found a real hole: the old host check took
`read.amazon.com.evil.test`.

### `word-timing.test.ts` — 6 tests · `src/word-timing.ts`

The browser's estimated word boundaries. *marks start at zero, never go backwards, and fit inside
the audio* is the invariant; the rest are the syllable heuristic.

**Verdict: keep, and note what it is not.** These are estimates. The Kindle path gets model-derived
marks and this file must not be described as testing those.

### `host-contract.test.ts` — 5 tests · Rust source, read directly

The four constants the extension and `kokoro-host` must agree on — extension id, port, sample rate,
pairing string — **parsed out of the Rust with regexes**, not restated in a fixture. Every one of
them fails as something other than itself: an id mismatch arrives as a 403 that reads like a
pairing problem; a sample-rate mismatch is not an error anywhere, the book is just read at the
wrong pitch.

**Verdict: keep — this is the highest value-per-line file in the repo.** No test that drives only
one side can catch any of it. It does not overlap `manifest.test.ts`: that one checks
manifest ↔ extension code, this one checks manifest ↔ Rust.

### `host-http.test.ts` — 5 tests · `src/offscreen.ts`, `src/kokoro-http.ts`, `src/speak.ts`

The whole browser transport against a stub host: handshake, wrong token, Origin refusal, a page
going out as authenticated chunks and coming back as scheduled audio.

**Verdict: keep.** *a wrong token is reported as a pairing problem, not as an outage* is the
distinction `diagnosePaired` exists for.

### `tar.test.ts` — 5 tests · `src/tar.ts`

Parses the reader's `/renderer/render` responses (uncompressed tar of `glyphs.json` +
`page_data_*.json`). `tar.ts` is imported by exactly one thing: `src/content/net-probe.ts`.

**Verdict: keep the tests, but see the open question below.** The tests are 66 lines and correct;
the question is whether the *code* should ship.

### `worker-voices.test.ts` — 4 tests · `src/background.ts`, `src/kokoro-http.ts`

Engine selection in the service worker: Kokoro when paired and reachable, platform voices
otherwise, and — the load-bearing one — *an unreachable PAIRED host is not diagnosed as an unpaired
one*.

**Verdict: keep.** *a stale token on a large POST still gets told to re-pair* covers the
deliberately-accepted lost-401 case, where the bodiless `/status` re-ask is the only thing that
separates a stale token from a dead host.

---

## `kokoro-ocr` — 54 tests

`cargo test --manifest-path kokoro-ocr\Cargo.toml`. **Needs no models.** That is deliberate and is
why there is no `native` feature or stub engine: ONNX Runtime is loaded at run time, so the pure
logic is testable on a machine with nothing provisioned.

| Module | Tests | What it pins |
|---|---|---|
| `detect` | 13 | DB post-processing: box extraction, unclip, reading order, the stride-multiple input, no box escaping the image, and *a small page is never upscaled* |
| `recognize` | 11 | CTC decode and word boxes: repeat collapsing, the blank separating a real double letter, *the space class is what splits words*, boxes non-overlapping left to right, confidence on the extension's 0–100 scale |
| `prep` | 9 | PNG decode and limits: non-PNG, truncated bytes, declared geometry over the pixel cap, grayscale arriving as RGB |
| `session` | 9 | Dictionary handling and the digest gate: *the leading sentinel is dropped and a space appended*, CRLF not becoming part of a class, and bytes that miss their pin being refused **and never returned** |
| `engine` | 6 | Queue and cancellation: a full queue says busy rather than growing, a job cancelled before it runs is not run, models that fail their pin are refused **at load, not only at probe** |
| root | 6 | `Status`, error codes, rect clamping, the one-way cancel flag |

**Verdict: keep all of it, and note what it cannot do.** These are the only tests that would catch a
tensor-layout or class-count mistake in pure logic — the failure mode that otherwise reads out as
fluent, confident, wrong text. But they do **not** run the real graphs. That is
`cargo run --example ocr-check` (below), and it is the only thing that catches the same mistake in
the models themselves.

---

## `kokoro-host` — 47 tests

`cargo test --manifest-path kokoro-host\Cargo.toml`.

| Module | Tests | What it pins |
|---|---|---|
| `text` | 29 | The Kokoro-js normalization port: golden characterization tests whose expected values were proven token-identical to the reference pipeline, plus the whole span-mapping layer (every normalized byte has a span, spans monotonic and in bounds, UTF-16 offsets counting code units not bytes) |
| `webserve` | 10 | The loopback endpoint: per-route body caps, the constant-time token comparison visiting every byte, the frozen response shape, and *an over-cap post is refused with a status the client can read* — which speaks the browser's dialect on purpose, because `curl` sends `Expect: 100-continue` and cannot see that bug. Plus *ocr\_manifest\_matches\_kokoro\_ocr\_pins*: the OCR download manifest's digests/filenames must equal `kokoro-ocr`'s own pins, so the panel (which downloads at first run) and the host (which re-verifies on load) can never split on what a valid model is |
| `native_synth::mark_tests` | 5 | Word marks against the wire rules — the only coverage `kokoro_protocol::mark_is_valid` has anywhere |
| `model_patch` | 3 | The in-memory ONNX graph patch, byte-matched against `onnx`'s own serialization (270 bytes lifted verbatim out of the old sidecar) |

**Verdict: keep.** `text`'s 29 are the oldest tests in the repo and still the ones standing between
a refactor and a subtly different pronunciation. `model_patch::matches_the_bytes_onnx_wrote` is
what makes hand-rolled protobuf acceptable at all — a wrong field number produces a file that still
parses, into a different graph.

---

## Harnesses — run by hand, not by CI

Each exists because the thing it checks cannot be reached from a unit test. All but one are outside
any automated run — **`kokoro-sapi-smoke` is the exception**, and it is the only automated test the
x86 crates have at all (none of them carries a `#[test]` suite).

| Harness | Needs | Status |
|---|---|---|
| `kokoro-ocr --example ocr-check` | ORT DLL + the three models | **Works.** The real graphs over one PNG, no browser and no host. The only thing that catches a tensor-layout or class-count mistake |
| `test/capture.check.ts` | Chrome/Chromium/Edge | **Works.** Drives `capture.ts` against a fixture shaped like the measured reader: shadow piercing, candidate scoring, blob-identity page turns, capture while the tab is hidden |
| `test/ocr-bench.ts` | paired host + `KOKORO_ALLOWED_ORIGINS` | **Works, with setup.** Word error rate over rendered pages with known ground truth, across the matrix that produces silent failures (two-column, dark, non-default font) |
| `test/ext-ocr.check.ts` | a Chrome that honours `--load-extension` | **Effectively unrunnable** — see below |
| `kokoro-sapi-smoke` | x86 build; a host only for the `Speak` half | **Works — 12 checks, and the only one of these that CI runs** (`sapi.yml`). `LoadLibrary` + the COM object model + `Speak`, without Kindle or elevation. The 7 COM checks need no host, which is why the hostless runner still catches a vtable/QI regression; the `Speak` half self-SKIPs there |
| `kokoro-hook --bin selftest` | Kokoro registered; x86 build | Works. Guards the `SetVoice` vtable index (slot 18) without Kindle |
| `kokoro-sapi/test-speak.ps1` | 32-bit PowerShell, host running, DLL registered | Works. The SAPI-registered path end to end |

---

## What is missing, and what is stale

Recorded here rather than left implicit.

### Nothing in CI runs any of these

The three workflows are `installer.yml` (builds the package on a `v*` tag), `sapi.yml` (builds the
x86 DLL and runs the COM smoke test) and `hook.yml` (compile-checks the x86 hook and injector).
**None of them runs `bun test` or `cargo test`.** Every one of the 246 tests above passes or fails
only when somebody runs it locally.

That is the single largest gap in this document, and it is cheap to close: the extension suite
needs `bun` and nothing else, and the two Rust suites need no models, no ORT DLL and no network.
`kokoro-ocr`'s in particular was designed to run with nothing provisioned, which is most of the
argument for adding it to CI first.

### `ext-ocr.check.ts` can no longer run on a normal Chrome

It answers a question nothing else does — *does the `POST /ocr` fetch carry
`chrome-extension://<id>` when the extension is loaded as an extension?* — and if it does not, no
amount of OCR quality matters, because every request 403s. But Chrome 137+ removed
`--load-extension`, and neither `--enable-unsafe-extension-debugging` nor
`--disable-features=DisableLoadExtensionCommandLineSwitch` restores it. It prints `SKIPPED` and
exits 0.

It needs a Chrome for Testing build (`bunx @puppeteer/browsers install chrome@stable`,
`CHROME_PATH=...`). Until then the question is answered faster by `await kwr.readPage()` on a live
book. **Keep it** — the harness is correct and the day a pinned Chrome is available it is the only
automated answer — but do not count it as coverage.

### Five crates have no tests at all

`kokoro-protocol`, `kokoro-sapi`, `kokoro-panel`, `kokoro-hook`, `kokoro-inject`.

Most of that is defensible: the x86 crates are COM/Win32 glue that a unit test cannot reach, and
they have smoke binaries instead. Two gaps are real:

1. **`kokoro-protocol::mark_is_valid` is tested only from `kokoro-host`.** It is a pure function,
   `no_std`, shared by both wire ends, and it is the thing that stops the host emitting a shape the
   engine rejects. Its five tests live in the wrong crate.
2. **`kokoro-sapi`'s parsing of the aligned frame stream is untested.** That is the x86 code running
   *inside Kindle*, allocating off lengths it read from a pipe — `MAX_MARKS_PER_CHUNK` and
   `MAX_FRAME_SAMPLES` exist precisely because a squatted pipe could feed it a bad header. The
   bounds logic in `worker.rs` is pure and could be tested; today only the smoke binary touches it.

### A stale doc reference

`test/capture.check.ts:3` cites `docs/kindle-web-reader-internals.md`, which does not exist. The
reader-internals material it points at is in
[`kokoro-browser-extension/README.md`](kokoro-browser-extension/README.md).

### One open question: should `net-probe.ts` and `tar.ts` ship?

`net-probe.js` is 30.4 kb of every installed package. It hooks `fetch`/`XHR` at `document_start` in
the MAIN world, records what the reader downloaded, and parses the tar — which is how it was
established that the reader ships **no usable text** (glyph ids reshuffled every few pages), and
therefore why this project OCRs at all. It also carries the debug bridge that mirrors `kwr` onto
the page world.

So it is not scaffolding — it is the evidence for the whole architecture, and `kwr.net()` is how
that evidence is re-checked when Amazon changes something. But it runs in every session for every
user, in the MAIN world, on Amazon's page. A dev-build flag is worth considering. **Not a
correctness issue; a packaging one.** Its tests stay either way.

---

## Conventions

- **`.test.ts` is for `bun test`; `.check.ts` is for harnesses run by name.** A `.check.ts` drives a
  browser and calls `process.exit()`, which under `bun test` terminates the whole run and silently
  skips every file after it.
- **Test names are sentences that state the property**, not `describe`/`it` fragments. A failing
  name should read as the claim that broke.
- **A test for a bug must be checked by deleting the fix.** Two tests in `narrate.test.ts` passed
  without their fix before this was made a rule.
- **Contract tests parse the other side's source** rather than restating its values in a fixture.
  A second copy of a constant is the drift the test exists to catch.
