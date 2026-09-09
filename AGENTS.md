# AGENTS.md

Instructions for OpenAI Codex working in this repo. (Claude Code reads `CLAUDE.md`; the
two are kept consistent, and `CLAUDE.md` is the fuller reference — read it.)

## What this is

Local, offline Kokoro-82M text-to-speech that gives **Kindle for PC** a real TTS voice on
Windows. Pure-Rust synth core (`ort` crate on ONNX Runtime's Dawn WebGPU EP + an espeak-ng
FFI). Two x64 exes plus three x86 artifacts Kindle loads in-process. No workspace — each
crate builds standalone.

## Your role follows the user's request

For review requests, read the code, verify claims, and report findings. When the user asks
for fixes or implementation, make the necessary changes and complete the appropriate
verification. Permission already given for that work remains valid within its scope; don't
ask again merely because the default workflow describes Codex as a reviewer. The read-only
review workflow in `DEVELOPMENT.md` applies to review-only sessions, not user-authorized edits.

Read the current diff before working: Claude and the user may have changed the tree since
the last turn. Preserve their pending changes and keep your edits focused on the requested
work. Commit or publish changes only when the user requests it.

Don't rewrite working code, and don't propose stylistic changes — formatting, naming, and
comment density are settled and match the surrounding code deliberately.

**Verify before reporting.** Grep or read the relevant source and confirm a finding actually
holds before writing it up. A confident false positive costs more here than a missed nit,
because every finding gets hand-verified downstream.

**If nothing is wrong, say so plainly.** Do not manufacture findings to appear useful.

## Read these for context

| Need | Read |
|---|---|
| The invariants — start here | `CLAUDE.md` |
| Engine chain, streaming/pacing, repo layout | `ARCHITECTURE.md` |
| Installer, elevation flow, ACL staging | `packaging/README.md` |
| Per-crate detail | `<crate>/README.md` |

## Environment constraints — these override any instruction to "just test it"

- **Never run benchmarks, stress tests, or sustained-load builds.** Real performance numbers
  are measured on designated target hardware; a figure taken from a review checkout is noise
  either way. That means no `cargo bench`, no timing harnesses, and no repeated release builds.
- **Don't try to run the app.** `kokoro-host` must be running for any audio path to work,
  Kindle must be installed and injected, and the SAPI DLL must be registered (elevated).
  Verification here is by reading code, plus at most `cargo check`.
- **Don't register/unregister the COM server** or edit the Kindle MSIX hive. Both need
  elevation and change system state.

## Invariants worth checking in review

Full list and rationale in `CLAUDE.md` — these are the ones code changes actually break:

- **Never execute an elevated artifact from a user-writable path.** `regsvr32` runs a DLL's
  `DllRegisterServer` and the guard runs a `.ps1`, both as admin. They must come from the
  ACL-locked `%ProgramData%\Kokoro Kindle Reader\engine\`, never `%LOCALAPPDATA%`. This is a
  standing local-EoP concern; flag any path that reintroduces it.
- **Bitness is fixed.** `kokoro-sapi`, `kokoro-hook`, `kokoro-inject` must stay x86 (Kindle
  is 32-bit); the host is x64 and spawns the injector rather than injecting itself.
- **Don't call a `windows` crate feature unused because no import names it.** The bindings gate
  individual functions on the features their *parameter* types need, so `Win32_Security` is what
  makes `RegCreateKeyExW` and `CreateRemoteThread` exist and `Win32_System_IO` is what makes
  `WriteFile` exist — none of them via a path anyone writes. Both were removed on that reasoning
  and both broke the x86 build.
- **`kokoro-host` is the sole authority for Kindle reading and health.** `kokoro-panel` has
  no UI Automation, no Win32 dependency, and no Kindle poll: Play/Stop/Pause/Resume/Close go
  over the pipe as `CMD_KINDLE` intent and it renders what the host reports back, health
  included (an unopenable pipe is what "offline" means). Flag any panel code that inspects
  or drives Kindle directly, any second native control transport, and any health check that
  infers liveness from a process name, cached state, or a request that already succeeded.
  All Kindle UI work belongs on `kindle_ctl`'s dedicated thread — off the tokio runtime and
  off the synth worker. Commands there are serialized against each other deliberately (a
  Stop waits for an in-flight Play; two overlapping keystroke sequences aimed at one blind
  toggle would land in an unknowable order). What must never queue behind them is a query or
  a pause, and neither touches that thread — flag anything that puts them there.
- **The host compiles for Windows AND Linux; keep the split at `cfg(windows)`.** Windows is
  the Kindle reader (named pipe, `kindle_ctl`, `kindle_state`, `kindle_watch`, `legal`,
  `split_text`, tray) and its crates are under `[target.'cfg(windows)'.dependencies]`; Linux
  is the shared core plus the loopback endpoint. Flag anything that puts a Windows-only type,
  module or crate on the shared path, and any `build.rs` change that branches on
  `cfg!(windows)` rather than `CARGO_CFG_TARGET_OS` — a build script is compiled for the
  host, so `cfg!` there answers the wrong question. `cargo check --target
  x86_64-unknown-linux-gnu` must stay clean including warnings.
- **CPU is the Linux default in code (`DEFAULT_ENGINE`), never via a written settings file.**
  `read_controls` falls back to `Controls::default()` for a missing file, unparseable JSON
  (a UTF-8 BOM does it silently) and a missing key alike. Flag any fix that writes an initial
  `controls.json` instead, and any silent GPU→CPU substitution: the fallback must log.
- **`build-espeak.sh` and `build-espeak.ps1` are one recipe in two languages.** Same immutable
  commit, same single documented modification, same `ffa5cbde…` digest of the patched
  `phsource/ph_english_us`, same refusal to build a tree carrying anything else. Flag any
  drift between them, and any suggestion to use a distribution's own libespeak-ng — it is
  unmodified and the phonemes would differ, audibly and on one OS only.
- **The browser path must not reach Kindle types.** The host has three contexts:
  `ctx::CoreCtx` (paths, the one `NativeSynth`, the one `HostState`, `available_voices`),
  `pipe::KindleCtx` (core + `KindleCtl` + `KindleState`), and `webserve::WebCtx` (core +
  endpoint + OCR worker). `WebCtx` held the whole pipe context once, so serving a page image
  over HTTP depended on a UI Automation thread for a reader that client never uses. Flag any
  `KindleCtl`/`KindleState` reaching `webserve.rs`, and any construction of a second
  `NativeSynth`, `HostState` or bench flag — all three are built once in `main` and cloned.
- **The reading belief is never sampled from Kindle's assistive-reader toggle.** That UIA
  element is only in the tree while the Aa menu is open, which is when the user is changing
  it — so a read lands mid-change, gets stored as *definite*, and makes `set_reading` skip
  its Ctrl+A, inverting Play and Stop against a blind toggle. `refresh` uses the process list
  and the Kindle audio clock and touches no UIA. Flag any code that reads that toggle's
  state; reading its *presence* to dismiss the flyout before Ctrl+A is the allowed use.
- **`controls.json` is the contract, and it is SETTINGS only.** Every key `kokoro-panel`
  writes must be read by a host reader — `native_synth::read_controls` for synth fields,
  `kindle_watch::enabled` for `kindle_kokoro`. A key written but never read is a bug, and so
  is a live command put in the file: `paused` was one, and it is host-owned state reached
  over the pipe now. Also: **a BOM makes the whole file parse-fail in silence** and every
  setting revert to its default (`gpu_synth`'s default is Gpu). Flag any script that writes it
  with PowerShell's `Set-Content -Encoding utf8`, which adds one.
- **Synthesis is serialized.** espeak has global state and isn't thread-safe, and the `ort`
  session is owned by one worker thread. Any code calling espeak or running the session off
  that thread is a bug.
- **The wire format lives in `kokoro-protocol`**, a path dep of every consumer. Neither
  `kokoro-host`, `kokoro-sapi` nor `kokoro-panel` may hardcode the constants inline.
- **`CMD_SYNTH` and `CMD_SYNTH_ALIGNED` are for real-time sinks only.** That stream is paced to ~real time, which is
  right for Kindle (the SAPI engine plays what it's handed) and wrong for everyone else.
  Timing it measures the pacing — every engine faster than realtime reads ~1.0x — so the
  panel's speed test uses `CMD_BENCH`, its Preview uses `CMD_PREVIEW`, its transport and
  health check use `CMD_KINDLE` (no audio at all), and the browser path bypasses the pipe
  entirely. Flag any new consumer that reaches for the paced path instead — and don't let a
  doc claim the pipe only serves real-time sinks; three of its four commands don't.
  `CMD_PREVIEW` also keeps the panel's own synthesis off the host's *Kindle*-audio clock, so
  "is Kokoro narrating?" is a fact the host states rather than a guess a client makes from
  timing. Any new source of host audio that isn't Kindle needs the same separation.
- **Kindle's word highlight is model-derived; the browser's is not, and the two must not be
  described alike.** `model_patch.rs` appends 273 bytes to the stock `model.onnx`'s bytes **in
  memory** at session build, so the graph returns per-token frame counts, and `CMD_SYNTH_ALIGNED`
  carries them to the engine as marks in a `CHUNK_ALIGNED` header. Things to flag:
  - **A mark list that is empty means "no timing for this chunk", never "no words".** Any consumer
    reading it the other way fires no events, which is the bug that strands Kindle on sentence one.
  - **`marks` must never be approximate.** Every failure in the duration path degrades to empty and
    lets interpolation take over; nothing there may fail the *audio*.
  - **`sum(frames) * 600 == waveform length` is asserted per run.** Don't let it be relaxed to a
    tolerance, and don't rescale frames by `speed` (it is applied upstream of the rounding).
  - **The graph edit is in memory and must stay there.** Anything that writes a patched graph to
    disk is a regression: over `model.onnx` the panel's verify deletes it (SHA-256 mismatch) and
    silently reverts the feature; under its own name it is a 326 MB artifact that has to be
    hosted, downloaded or hand-copied - which is exactly why the previous sidecar shipped to
    nobody. The append works because protobuf merges a repeated `ModelProto.graph`; flag any
    change that turns it back into a re-serialization.
  - **The patch must never cost the audio.** A rejected graph truncates the buffer back to the
    file's own bytes and commits that. There is no capability flag: `run_model` asks the session
    for `durations_frames` by name, so a stock fallback reaches interpolation with no bookkeeping.
  - **`model_patch`'s encoder is byte-matched against `onnx`'s serialization in a unit test.**
    A wrong protobuf field number yields a file that still parses, into a different graph, so
    flag any change to the encoder that doesn't keep that oracle honest.
  - **`CHUNK_ALIGNED`'s absolute chunk start is load-bearing.** Chunks are trimmed and do not abut;
    accumulating `CHUNK_INFO` lengths drifts about a character per chunk. Flag any client that
    reintroduces the running total.
  - **The old-host drop has TWO shapes and both must trigger the fallback**: a failed
    `begin_synth` (the host closed while the request was still being written) and a failed first
    read (it closed after). Which one happens is a race on the pipe buffer. Handling only the
    read shape makes the fallback unreachable and every page returns `E_FAIL` - silence.
  - **A `Speak` that yields no audio must not return quickly** (`RECOVER_WINDOW`, 15 s).
    Kindle turns the page when the utterance ends and an `HRESULT` is all it has, so an
    instant `E_FAIL` reads as "page done" and races the book. Flag any change that returns
    early from a failure path, caches "no host" across attempts, or makes the wait ignore
    `SPVES_ABORT`.
  - **The fallback re-checks `SPVES_ABORT` before re-sending.** The probe read blocks for a whole
    chunk's synthesis, so a Stop pressed inside it is already pending by the time the fallback
    runs - and re-sending puts the host to work on the one serialized synth worker behind a Stop
    the user watched succeed, stamping the Kindle-audio clock as it goes.
  - **A malformed aligned stream costs the MARKS, not the page** - but an unusable *header*
    (count over the cap or over the chunk's own length, or `charStart + charLen` overflowing)
    fails closed and drops the connection, because its announced length cannot be trusted to
    drain. Flag any collapse of those two cases into one.
  - **The `'S'` fallback is scoped to ONE utterance and is never cached** - `Worker` holds no
    capability state at all. Zero frames is necessary evidence of an old host but NOT
    sufficient (a current host quit mid-request looks identical), and the evidence destroys the
    connection it is about, so any cache lands on the *replacement* connection - which may be a
    restarted capable host. Flag any reintroduction of a cached/sticky downgrade, per-process or
    per-connection.
  - **Interpolation is the fallback and must stay.** A chunk without marks still has to fire its
    events somewhere.
- **The browser has exactly ONE transport: loopback HTTP** (`webserve.rs`, port 8787). An
  extension cannot open a named pipe. A native-messaging bridge was prototyped first and
  rejected. **Flag any change that adds one as a fallback**: two
  transports mean every failure is diagnosed twice, and HTTP is the one that needs no
  per-browser registration, could reach Firefox, and can be curl'd.
  Flag any change that weakens the endpoint's four checks (127.0.0.1 bind, origin allowlist,
  constant-time token, `Host` check) or binds anything other than loopback. The extension
  manifest's `key` is load-bearing: it pins the id the origin allowlist matches.
- **A page's columns feed ONE utterance.** A two-column page is OCR'd a column at a time so the
  first can be spoken while the second is recognized; the parts go over the port into one
  `PartQueue`. Flag any change that issues a second `speak` per column (it tears the running
  audio stream down and puts a synthesis-length silence mid-page), that leaves a path where the
  queue is not closed (finish, Stop, superseded, disconnect - a missed close parks the worker
  forever), that cuts a part anywhere but a sentence end, or that infers a part's `base` from a
  running total of part lengths instead of the producer stating it.
- **The page is turned by ONE action, `ArrowRight`, and a turn is claimed only on EVIDENCE - a new
  `blob:` URL at an unchanged layout.** `turnPage` dispatches the key on the page image; a
  next-page control found by accessible name and a tap on the forward half of the page were both
  built and deliberately removed - they could only ever run after the key had already failed, and a
  fallback that runs only in the case you cannot reproduce is the second-transport trap. A resize
  or zoom re-renders the SAME page under a fresh URL, so a render is rejected only on positive
  proof of a re-layout (viewport or rendered size differing). Those two do not catch a font-size
  reflow, which nothing cheap can - so an accepted turn is handed back only once the page holds
  still, which is what keeps the real turn landing behind such a re-render from being read past.
  A turn stays "pending" (for `settleTurn`) only while nobody has watched it to the end of a
  budget: `turnPage` clears it after its own uncancelled wait, and a `settleTurn` cut short by a
  Stop must leave it for the next reader. Flag anything that reports a turn because the key was
  sent, that treats any new URL as a turn, that hands back a turn without waiting for the page to
  settle, that re-adds a fallback action, that distinguishes "end of book" from "the reader stopped
  answering" by appearance, that dispatches the key on `document.activeElement` (pressing Play
  leaves focus inside this extension's own panel), that ties the pending window to `TURN_WAIT_MS`
  rather than to the wait actually performed, or that starts a read without `settleTurn()` (Stop
  cannot unsend a keypress already dispatched, and its render lands under whatever reads next).
- **In `src/ocr/`, a rule that can silently remove or reorder text must act on EVIDENCE, not on
  appearance.** (The directory is five modules: `backend.ts` is the `POST /ocr` transport,
  `layout.ts` the pixel-side column split, `lines.ts` the shared line geometry, `furniture.ts` the
  only rule that drops a line, and `index.ts` assembles the page — it owns the re-split, the
  checkpoint around it and the `trial` flag. It runs in the **offscreen document** — `furniture.ts` keeps
  cross-page state, so a copy loaded in another context has a memory nothing writes to.)
  Four content losses came from thresholds encoding what a page was assumed to look
  like. Appearance may pick candidates; only evidence may act — text no book has in its body, the
  same thing seen on another page, or a decision verified after the fact and redone. **Flag any
  new threshold whose failure is silent.** Specifically flag: dropping a line on a first sighting;
  counting sightings rather than distinct pages (`pageToken` stops a page read twice counting as
  two) or building that token from anything a reflow changes (whitespace, line structure,
  hyphens); an OCR pass whose text is not narrated reaching the rule at all (the re-split wraps in
  `furnitureCheckpoint()`, the reflow re-OCR passes `{trial:true}`); `measureOf` computed
  page-wide rather than per column; removing the even-spacing check (it serves both the measure
  test and `looksInterleaved`); deciding page polarity from a mean rather than the histogram mode;
  or removing the per-page log of what was withheld — furniture OCRs perfectly, so nothing else
  can detect the mistake. A constant that only degrades quality (`PLAYBACK_RAMP`, `PAD`, the
  word-timing weights) is not covered by this.
- **Test fixtures must be public-domain or invented, never the book under test.** **Flag any
  fixture, ground truth, doc example or code comment that reads as if it were pasted from a real
  copyrighted work** — quoted prose, a real running head or section heading, a publisher's name, a
  real ASIN (`B0` + 8 alphanumerics). Public domain is fine (`test/ocr-fixture.html`'s ground truth
  is *Moby-Dick*) and so is invented prose (the furniture fixtures are an invented harbour book;
  `BENCH_TEXT` is an invented lighthouse sentence). This has happened once and reached six files
  including a page image, so it is worth a look whenever fixtures change: a fixture needs the
  shape — word count, ink width, punctuation — never the content.
- **The browser's word highlight runs on ESTIMATED boundaries.** `/synth` returns PCM and the
  browser path does not use the aligned pipe command (it doesn't use the pipe at all) - the
  durations exist, `/synth` just doesn't carry them - so `word-timing.ts` splits each chunk's
  exact duration
  across its words by syllable count. Flag anything that presents these as true offsets, adds
  a second estimator, or times them off `setTimeout` rather than `AudioContext.currentTime` —
  the audio clock is what makes Pause freeze the highlight instead of running it to the end of
  the page. Chunk offsets must stay aligned on non-whitespace characters; summing chunk lengths
  drifts a character per paragraph break. A resize re-renders the reader's page with the text
  reflowed, so the highlight re-OCRs it and relocates the word by its neighbours — flag anything
  that re-anchors the NARRATION to a reflow (it is still speaking the text captured at the start
  of the page) or that draws on an ambiguous or unmatched relocation.
- **A live speed change needs BOTH a per-chunk rate and a flush of the lead.** The panel mutates one
  `SpeakOptions` object and tells the worker (`{t:'options', rate}`), since the port clones the
  options at `speak` time; `speakAll` then re-reads the rate per send. That alone is inaudible for up
  to `MAX_LEAD_S` — `speed` is a synthesis parameter, so already-rendered samples cannot be adjusted
  — so `audio-retune` discards every source that has not started, rewinds the cursor to the end of
  the chunk being heard, and the loop re-sends from there. A flush leaves no lead, so the chunk it
  resumes on is re-sent in `PLAYBACK_RAMP`-sized pieces - the same cold start as the top of a page;
  whole, it is a silence one to two sentences long. Flag: a fresh options object per Play (it freezes
  the speed for the whole book); a re-send under a NEW chunk index (every later word boundary then
  remaps onto the wrong word); a cold resume at the settled chunk size; dropping the per-piece
  `offset` from the wire or from `queueMarks` (marks then address the piece instead of the chunk, and
  a second change mid-ramp repeats a sentence); `playbackRate` used to retune scheduled audio (it
  shifts the pitch); cutting the chunk currently playing; a retune driven by the slider's `input`
  rather than `change` (a drag then costs dozens of re-syntheses on the worker Kindle shares); or a
  flush that bumps the epoch (that is a Stop, and it ends the page). Also flag any wait between the
  slider and the flush that cannot see the change: the throttle's nap is sliced and the pull for more
  text is raced (`raceInterrupt`, holding the pull rather than re-issuing it - an iterator's value is
  consumed by the `next()` that produced it, so a dropped pull is a lost chunk), and the worker's
  `live` options are published before `narratorFor()` is awaited, which on a first Play is seconds.
- **Kindle 18632's narrator is event-driven.** The SAPI engine must emit word/sentence/
  bookmark events at true audio offsets, or Kindle speaks one sentence per page and stops.
- **The tree is not uniformly MIT.** `kokoro-host/src/text.rs` (+ `espeak.rs`'s
  `PhonemizeSegment` mirror, `native_synth.rs`'s style-row rule) is ported from **kokoro-js**
  and `kokoro-ocr/src/detect.rs`, `prep.rs`, `recognize.rs`, `session.rs` from **PaddleOCR** —
  both Apache-2.0, both attributed file-by-file in `THIRD_PARTY_NOTICES.md`, with the text in
  `licenses/Apache-2.0.txt`. Every derived file carries its own in-file Apache-2.0 change
  notice (Apache §4(b)): the PaddleOCR ones retain `Copyright (c) 2020 PaddlePaddle Authors`,
  and `text.rs` / `espeak.rs` / `native_synth.rs` name the kokoro-js-derived portion (the
  mixed files are marked `MIT AND Apache-2.0`, not whole-file Apache). `kokoro-panel/ui/resume.svg`
  carries one too (modified Material Symbol). Flag any new port that doesn't add both the
  notices-file row and the in-file header.
- **The bundle is GPLv3 even though the source is permissive.** The app links espeak-ng
  (GPL-3.0-or-later, and *modified* by `native-deps/build-espeak.ps1`) and Slint under its
  GPL-3.0 option. So `LICENSE` + `THIRD_PARTY_NOTICES.md` + `licenses/` must stay staged
  by `build-installer.ps1` and installed by `installer.nsi`. No shipped artifact may claim
  a bare licence name in its version resource — that's `installer.nsi`'s `VIAddVersionKey`
  plus `LegalCopyright` in `kokoro-host/build.rs` and `kokoro-panel/build.rs`; all three
  read a copyright holder plus a pointer to `THIRD_PARTY_NOTICES.md` instead. If the espeak
  patch changes, the notice of modification in `THIRD_PARTY_NOTICES.md` must change with it.
- **The Cargo dependency closure's own licence notices are generated, not hand-audited.**
  `packaging/generate-dependency-licenses.ps1` runs `cargo about` against each shipped
  crate's `Cargo.lock`, on every installer build, and fails the build if a dependency's
  licence isn't on `packaging/about.toml`'s accepted list. Don't replace that with a
  hand-written prose list of "the unusual crates" — that's what drifted and shipped four
  sole-licensed crates as if MIT/Apache-2.0 already covered them. `cargo about` must be
  installed in CI (pinned in `installer.yml`; also gated on PRs by `license-check.yml`) or
  the build throws. `GPL-3.0-only` is accepted for the Slint crates ONLY (per-crate entries
  in `about.toml`), so a GPL dep through anything else fails `--fail`.
  Preserve the packaged-file/source-header appendices and the hash-pinned upstream
  clarifications. `cargo-about` only warns if a clarification fails; the separate
  `verify-dependency-licenses.ps1` check must reject missing texts in the generated AND
  extracted reports. MIT placeholders are not a substitute for upstream copyrights.
  `source-notices.json` pins complete W3C terms from Tao/Winit/cursor-icon and Intel's ISC
  notice from Ring's native P-384 source; unreviewed versions or changed excerpts fail
  generation, and their hashes are required in generated and extracted reports.
  Ordinary appendix files/headers must also pass decoded-text hashes and the recorded
  block count; checking only special clarifications misses changed or deleted notices.
  The tray and Settings must keep **About & licenses** available independently of narration;
  it opens installed `legal.html`, whose content and local links are checked in packaging.
- **Provisioned notices that must ship, and the checks that prove they do.** Besides the ORT
  wheel notices, `fetch-deps.ps1` provisions espeak-ng's own `COPYING*` (incl. `COPYING.UCD`,
  which is NOT `licenses/Unicode-3.0.txt`) and `build-installer.ps1` stages NSIS's `COPYING`
  (LZMA/CPL exception) from the pinned toolchain — both `throw` when absent.
  `verify-installer-notices.ps1` extracts the built `-setup.exe` in CI and fails on any
  missing/empty notice. The Rust Standard Library is outside Cargo's graph too:
  `build-installer.ps1` stages the exact toolchain's generated `COPYRIGHT-library.html` plus
  its release/commit, CI installs `rust-src`, and the corresponding-source archive carries
  that full `library/` tree after matching the active toolchain to the installer's staged
  `TOOLCHAIN.txt`. `packaging/components.toml` inventories every non-Cargo shipped
  component; `LICENSING.md` is the authoritative per-artifact map + §6 procedure.
- **Checked-in licence texts are content-pinned.** `packaging/license-texts.sha256` covers
  `LICENSE`, `THIRD_PARTY_NOTICES.md`, and every file under `licenses/` after newline
  normalization; `verify-license-texts.ps1` runs in PR CI, before an installer build, and
  against the extracted installer. Update a hash only after comparing the complete replacement
  with the pinned upstream revision. Provisioned notices must be exact named, non-empty files.
- **Native caches carry provenance.** `fetch-deps.ps1` pins ORT's exact cp312 win_amd64 wheel
  by filename and PyPI SHA-256 (the 1.27.0 wheels contain different native DLL bytes), and
  writes ORT/espeak recipe markers only after all expected outputs and notices exist. A missing
  or mismatched marker forces a fresh provision, and installer staging reads that cache directly.
  The espeak marker pins immutable
  commit `4870adfa25b1a32b4361592f1be8a40337c58d6c`, the modification, and the normalized
  `build-espeak.ps1` SHA-256; its build-time source manifest must match the corresponding-source
  tree exactly.
- **`-SkipBuild` still proves source identity.** A full installer build records SHA-256s for
  every tracked source file, both x64 executables, and the Rust toolchain beside the host output.
  Reuse requires all to match; corresponding-source packaging uses the records frozen in
  `staging/provenance/` (including espeak's source manifest) and checks the tracked tree again; never let
  a clean tag bless arbitrary stale binaries from `target/`.
- **NSIS is pinned in code as well as CI.** `build-installer.ps1` rejects `makensis` unless
  `/VERSION` reports 3.12, keeping the stub, its staged `COPYING`, `components.toml`, and the
  corresponding-source instructions on the same toolchain.
- **GPL binaries ship corresponding source.** `build-corresponding-source.ps1` produces
  `corresponding-source-X.Y.Z.zip` (LFS-resolved source, lockfiles/scripts, the modified
  espeak-ng tree and the exact Rust Standard Library source, both with SHA-256 manifests,
  plus the hash-verified official NSIS 3.12 source for its CPL-covered LZMA module)
  and `installer.yml` pairs it with the installer in both Actions artifacts and tagged
  releases. `sapi.yml` build-tests its intermediate DLL but does not upload it bare. Never
  ship an installer or intermediate binary alone.

## Encoding rules (real bugs, not style)

- **`.ps1` files must be ASCII.** PowerShell 5.1 misreads a UTF-8-no-BOM em-dash. Use `-`
  and `...`, never `—` or `…`.
- **`packaging/installer.nsi` must be ASCII.** `makensis` parses it as ACP, so non-ASCII in
  a user-visible `DetailPrint`/`MessageBox` renders as mojibake in the install UI.
- Rust and `.slint` files handle Unicode fine.

## Reporting format

Number each finding. For each: the defect in one sentence, `file:line`, a concrete failure
scenario (inputs/state → wrong outcome), and severity. Group by file. Lead with whether
anything was found at all.

For requested fixes, explain what changed, why, what was checked, and any unresolved issues.
Distinguish checks you ran from results reported by the user or another agent; don't describe
your own implementation review as an independent review.
