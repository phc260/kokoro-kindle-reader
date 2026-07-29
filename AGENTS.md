# AGENTS.md

Instructions for OpenAI Codex working in this repo. (Claude Code reads `CLAUDE.md`; the
two are kept consistent, and `CLAUDE.md` is the fuller reference — read it.)

## What this is

Local, offline Kokoro-82M text-to-speech that gives **Kindle for PC** a real TTS voice on
Windows. Pure-Rust synth core (`ort` crate on ONNX Runtime's Dawn WebGPU EP + an espeak-ng
FFI). Two x64 exes plus three x86 artifacts Kindle loads in-process. No workspace — each
crate builds standalone.

## Your role here is **reviewer**

Default to reviewing, not editing. Read the code, verify claims, report findings. Don't
rewrite working code, and don't propose stylistic changes — formatting, naming, and comment
density are settled and match the surrounding code deliberately.

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
  either way. That means no `kokoro-bench` / `bench_synth`, no `cargo bench`, no repeated
  release builds.
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
- **`controls.json` is the contract.** Every key `kokoro-panel` writes must be read by a
  host reader — `native_synth::read_controls` for synth fields, `kindle_watch::enabled` for
  `kindle_kokoro`. A key written but never read is a bug.
- **Synthesis is serialized.** espeak has global state and isn't thread-safe, and the `ort`
  session is owned by one worker thread. Any code calling espeak or running the session off
  that thread is a bug.
- **The wire format lives in `kokoro-protocol`**, a path dep of every consumer. Neither
  `kokoro-host`, `kokoro-sapi` nor `kokoro-panel` may hardcode the constants inline.
- **`CMD_SYNTH` is for real-time sinks only.** That stream is paced to ~real time, which is
  right for Kindle (the SAPI engine plays what it's handed) and wrong for everyone else.
  Timing it measures the pacing — every engine faster than realtime reads ~1.0x — so the
  panel's speed test uses `CMD_BENCH`, and the browser path bypasses the pipe entirely.
  Flag any new consumer that reaches for the paced path instead.
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
- **In `ocr.ts`, a rule that can silently remove or reorder text must act on EVIDENCE, not on
  appearance.** Four content losses came from thresholds encoding what a page was assumed to look
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
- **The browser's word highlight runs on ESTIMATED boundaries.** `/synth` returns PCM and the
  stock model exposes no alignment, so `word-timing.ts` splits each chunk's exact duration
  across its words by syllable count. Flag anything that presents these as true offsets, adds
  a second estimator, or times them off `setTimeout` rather than `AudioContext.currentTime` —
  the audio clock is what makes Pause freeze the highlight instead of running it to the end of
  the page. Chunk offsets must stay aligned on non-whitespace characters; summing chunk lengths
  drifts a character per paragraph break. A resize re-renders the reader's page with the text
  reflowed, so the highlight re-OCRs it and relocates the word by its neighbours — flag anything
  that re-anchors the NARRATION to a reflow (it is still speaking the text captured at the start
  of the page) or that draws on an ambiguous or unmatched relocation.
- **Kindle 18632's narrator is event-driven.** The SAPI engine must emit word/sentence/
  bookmark events at true audio offsets, or Kindle speaks one sentence per page and stops.
- **The bundle is GPLv3 even though the source is MIT.** The app links espeak-ng
  (GPL-3.0-or-later, and *modified* by `native-deps/build-espeak.ps1`) and Slint under its
  GPL-3.0 option. So `LICENSE` + `THIRD_PARTY_NOTICES.md` + `licenses/` must stay staged
  by `build-installer.ps1` and installed by `installer.nsi`. No shipped artifact may claim
  plain "MIT" in its version resource — that's `installer.nsi`'s `VIAddVersionKey` plus
  `LegalCopyright` in `kokoro-host/build.rs` and `kokoro-panel/build.rs`. If the espeak
  patch changes, the notice of modification in `THIRD_PARTY_NOTICES.md` must change with it.

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
