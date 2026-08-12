# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Local, offline Kokoro-82M text-to-speech synthesized **natively on the Dawn WebGPU**
execution provider of ONNX Runtime (the same Dawn as Chrome/WebView2) — **no WebView2,
no browser, no C++**. The synth core is pure Rust: the `ort` crate drives the model on
the WebGPU EP, and espeak-ng is reached over a thin FFI. Two x64 exes, plus three x86
artifacts Kindle loads in-process:

1. **`kokoro-host.exe`** (x64) — windowless system-tray daemon. Owns the named pipe,
   synthesizes, reads `controls.json` live, runs the Kindle-watcher, and is the **only**
   process that touches Kindle at all. The only thing that produces audio. Auto-starts
   hidden at login.
2. **`kokoro-panel.exe`** — native Slint settings panel, spawned on demand from the tray.
   Narrator/speed/volume, Preview, model download/verify, Kindle-narration toggle, and the
   Read Aloud transport — which it drives by *asking the host*, never Kindle.
3. **`KokoroSapi.dll`** (x86) — thin connect-only COM shim Kindle loads in-process; forwards
   each `Speak` over the pipe to the host.
4. **`kokoro_hook.dll` + `kokoro-inject.exe`** (x86) — make Kindle for PC 1.0.18632.0+ narrate
   with Kokoro by patching the `ISpVoice::SetVoice` vtable slot.

```
Kindle.exe (x86) ──in-proc COM (LoadLibrary + vtable)──▶ KokoroSapi.dll (x86 shim)
   ▲                                                        │ named pipe \\.\pipe\KokoroSapiSynth
   │ Ctrl+A / UIA / WM_CLOSE                                 │  'S' synth · 'B' bench
   │ (kindle_ctl.rs — the ONLY code that touches Kindle)     │  'P' preview · 'K' Kindle control
   │                                                         │
Chrome/Edge (Firefox: see below)                             │
  └─ kokoro-browser-extension (read.amazon.com)              │
       └── loopback HTTP 127.0.0.1:8787 ──▶ webserve.rs ─────┤
                                                             ▼
      kokoro-host.exe (x64, tray): pipe.rs ──▶ native_synth.rs (Rust synth:
                                       ▲          text.rs + espeak.rs + ort/Dawn WebGPU EP)
        reads live ── controls.json ──┘        ▲ spawns "Settings"
                          ▲                     │
      kokoro-panel.exe (Slint) writes ─────────┘
        └── 'K' over the same pipe: Play/Stop/Pause/Resume as intent, + a 1 Hz heartbeat
```

All audio comes from the native synth in `kokoro-host`; the SAPI engine synthesizes
nothing. **Consequence: `kokoro-host` must be running for Kindle to speak** — and it also
injects the hook and owns the reading controls, so one running host hooks Kindle, drives its
Read Aloud, and serves its audio. The panel is a view onto the host, not a second driver.

## Where the details live

This file carries only the **invariants** — the things that are expensive to rediscover.
Load the detail on demand:

| Need | Read |
|---|---|
| The engine chain end to end, streaming/pacing model, repo layout table, build-from-source | [`ARCHITECTURE.md`](ARCHITECTURE.md) |
| Contributor workflow, the two-model review split + `Reviewed-by:` convention, CI table, release/tagging steps | [`DEVELOPMENT.md`](DEVELOPMENT.md) |
| Every test and harness, what each pins, and the gaps | [`TESTS.md`](TESTS.md) |
| Installer internals: NSIS build, elevation flow, ACL staging, uninstall | [`packaging/README.md`](packaging/README.md) |
| Tray host + synth core internals (per-file layout) | [`kokoro-host/README.md`](kokoro-host/README.md) |
| Settings panel internals | [`kokoro-panel/README.md`](kokoro-panel/README.md) |
| SAPI engine: COM exports, interfaces, dev registration, smoke tests | [`kokoro-sapi/README.md`](kokoro-sapi/README.md) · [`kokoro-sapi-smoke/README.md`](kokoro-sapi-smoke/README.md) |
| Pipe wire format (the single source of truth) | [`kokoro-protocol/README.md`](kokoro-protocol/README.md) + the crate itself |
| Kindle 18632 hook + injector | [`kokoro-hook/README.md`](kokoro-hook/README.md) · [`kokoro-inject/README.md`](kokoro-inject/README.md) |
| Browser path: the extension itself — setup, pairing, layout, browser support | [`kokoro-browser-extension/README.md`](kokoro-browser-extension/README.md) |
| Browser path: the loopback HTTP endpoint and its four security checks | [`kokoro-host/src/webserve.rs`](kokoro-host/src/webserve.rs) |
| Browser path: Cloud Reader OCR — the two models, the boundary, the post-processing | [`kokoro-ocr/README.md`](kokoro-ocr/README.md) |
| Dep provisioning (ORT/Dawn DLLs, espeak-ng, OCR models) | [`native-deps/README.md`](native-deps/README.md) |
| User-facing install/usage | [`README.md`](README.md) |
| Codex's copy of these instructions (reviewer role + constraints) | [`AGENTS.md`](AGENTS.md) — keep its invariant list in sync with this file |

## Commands

```powershell
# One-time: provision the synth runtime deps (Dawn ORT runtime DLLs + espeak-ng x64
# import lib/DLL + espeak-ng-data). Must run before building kokoro-host.
native-deps\fetch-deps.ps1

# Build + run (Rust, x64). Right-click the tray → Settings to open the panel.
cargo run --manifest-path kokoro-host\Cargo.toml     # windowless tray daemon
cargo run --manifest-path kokoro-panel\Cargo.toml    # settings panel (or via the tray)

# SAPI engine — x86 Rust cdylib, no deps (thin COM shim + pipe client).
cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-sapi\Cargo.toml

# Browser path. The extension lives in kokoro-browser-extension/ (bun) and reaches the host
# over loopback HTTP only - nothing to register, but it must be PAIRED once per browser
# (tray -> "Web pairing code"). The endpoint is webserve.rs, inside the host; there is no
# separate exe to build.
bun run build.ts --stage         # from kokoro-browser-extension/; --stage copies off U:\ for Chrome
bun test test/                   # chunking + offsets, word timing, manifest/permission drift
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8787/status   # the whole transport, reproducible

# Cloud Reader OCR (kokoro-ocr, PP-OCR on the host). The models are loaded at RUN time, so the
# host builds without them and reports `missing`; fetch them once. Digests verified on download
# AND on every /status probe.
native-deps\fetch-ocr-models.ps1
cargo test --manifest-path kokoro-ocr\Cargo.toml   # needs no models: bounds, DB post, CTC decode
# The real graphs over one PNG, no browser and no host — the only thing that catches a tensor
# layout or class-count mistake, which otherwise reads out as fluent, confident, wrong text.
$env:ORT_DYLIB_PATH = "native-deps\runtime\onnxruntime.dll"
cargo run --manifest-path kokoro-ocr\Cargo.toml --example ocr-check -- page.png native-deps\ocr

# Kindle 18632 hook + injector — both x86 (Kindle is 32-bit; the host spawns the injector).
cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-hook\Cargo.toml
cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-inject\Cargo.toml
# Kindle-free check that the SetVoice vtable index is still 18 (needs Kokoro registered):
cargo run --release --target i686-pc-windows-msvc --manifest-path kokoro-hook\Cargo.toml --bin selftest

# Register the voice — DEV path (elevated; MUST be the 32-bit regsvr32). Same DLL path =
# registration survives rebuilds. The packaged installer does this automatically.
C:\Windows\SysWOW64\regsvr32.exe "kokoro-sapi\target\i686-pc-windows-msvc\release\KokoroSapi.dll"

# Packaged installer — builds the x86 DLL + release-builds both crates, stages everything,
# then runs makensis. NSIS. See packaging/README.md.
packaging\build-installer.ps1
# CI does this on a v* tag (.github/workflows/installer.yml); sapi.yml
# builds the x86 DLL + runs the COM smoke test on kokoro-sapi/** / kokoro-sapi-smoke/**
# / kokoro-protocol/** changes; hook.yml compile-checks the x86 hook + injector on
# kokoro-hook/** / kokoro-inject/** changes. (Both also re-run on edits to their own
# workflow file.)

# SAPI smoke test — no Kindle, no elevation: LoadLibrary the DLL + drive the COM object
# model + Speak path (needs the host running for audio). See kokoro-sapi-smoke/.
cargo run --release --target i686-pc-windows-msvc --manifest-path kokoro-sapi-smoke\Cargo.toml
# Or the SAPI-registered path (32-BIT PowerShell, host running, DLL registered):
C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe -File kokoro-sapi\test-speak.ps1
```

**245 automated tests** — 145 in the extension (`bun test test/`), 54 in `kokoro-ocr` and 46 in
`kokoro-host`, all of which run in seconds and need no host, no Kindle, no models and no network.
Every other crate has none, and the x86 ones have smoke binaries instead. What is *not* covered
that way is still the audible half: Preview in the panel and Read Aloud in Kindle (or
`test-speak.ps1`). Full inventory, per-suite verdicts and the known gaps: [`TESTS.md`](TESTS.md).

## Gotchas / invariants (do not rediscover these)

### Runtime / synth
- **`kokoro-host` must be running** or Kindle gets no audio (the engine's `Speak` returns
  `E_FAIL` when the pipe is absent — no fallback). It's a windowless tray daemon that
  **auto-starts hidden at login** (`auto-launch`, `--hidden`); Quit is only via the tray
  menu. Closing the settings panel does **not** stop the host. This also fixes Kindle
  **fast-scrolling** when the host is gone mid-Read-Aloud: a mid-session pipe disconnect
  makes each per-page `Speak` fail instantly, which Kindle reads as "page done" and races
  through the book — so keep the host alive.
- **Two synth performance questions are SETTLED. Don't re-open either without different
  hardware.** The standalone timing crate that measured them is gone (it produced figures, which
  this repo does not keep, and it could not be run here anyway); what it concluded is load-bearing
  and stays:
  - **Raising ORT's intra-op thread count above the physical core count made synthesis *slower***
    on the reference laptop — not flat, slower — because hyperthreads add contention rather than
    throughput on a power/FMA-limited part. That is why `native_synth.rs` ships ORT's default and
    has no thread-count override. Don't add one back for a chip in the same class.
  - **The hybrid GPU+CPU dispatcher gate FAILED.** Alternating chunks between both engines was
    measured *worse than CPU alone* on an integrated-GPU laptop: the active iGPU starves the CPU
    cores of the shared package power budget, so the combined rate lands below the CPU's solo
    rate. Machines that would need a hybrid can't benefit from one and machines with a real GPU
    don't need one, so it was never built. Sequential measurements overstate it — anything
    re-proposing this has to measure the two running *concurrently*, at thermal steady state.
- **Native synth is serialized.** espeak has global state + isn't thread-safe (and the
  `ort` session is owned by the worker), so ONE dedicated thread owns the synth; never
  call espeak / run the session from multiple threads.
- **The model fails the BERT `Expand` node past ~510 tokens.** A chunk's tokens are
  sub-split into `MAX_CONTENT_TOKENS`(=500) windows, each wrapped in its own BOS/EOS, and
  the PCM concatenated (`native_synth.rs`). A run also retries a couple of times —
  rebuilding the session on the last try — to ride out a transient Dawn device error.
- **Kindle 18632's narrator is event-driven.** `engine.rs` must report
  `SPEI_WORD_BOUNDARY` / `SPEI_SENTENCE_BOUNDARY` / `SPEI_TTS_BOOKMARK` at true
  audio-stream offsets, which is what the `CHUNK_INFO` frame (`0xFFFF_FFFD` + the chunk's
  UTF-16 span + sample count) exists to make possible. Without those events Kindle speaks
  the first sentence of a page and never advances.
- **Never time synthesis through `CMD_SYNTH`.** That stream is paced to ~real time
  (`pipe.rs`), so any engine faster than realtime clocks in at ~1.0x and GPU and CPU
  measure identical. The panel's "Test speed" dialog uses `CMD_BENCH` →
  `native_synth::bench` instead: same worker, same session builder, unpaced, on a fixed
  sentence the **host** owns (comparable numbers; a client can't hand the worker an
  arbitrary text). It shares the serialized worker with real synthesis, so the panel
  refuses to start one while Kokoro is narrating Kindle.
- **The panel's Preview is `CMD_PREVIEW` (`'P'`), not `CMD_SYNTH`.** Same request and
  response bytes; the host just doesn't pace it (the panel buffers the whole clip before
  playing any of it, so pacing only makes the intro take as long to arrive as to speak) and
  doesn't stamp the Kindle-audio clock with it. The second half is the load-bearing one:
  without a command of its own, the host cannot tell its own panel's silent narrator
  prefetch from Kindle narrating a page, and every consumer of "is Kokoro speaking?" has to
  guess from timing. One did, and the speed test had to distrust its own reading because of
  it. Anything that plays host audio without being Kindle needs the same treatment.

### `kokoro-host` is the sole authority for Kindle reading and health
- **The panel does not touch Kindle** — no UI Automation, no window enumeration, no
  keystrokes, no process lookups; `kokoro-panel` has no `windows`/`uiautomation` dependency
  and adding one back is the first step to two processes disagreeing about what Kindle is
  doing (which is what this replaced). Play/Stop/Pause/Resume/Close travel as intent over
  `CMD_KINDLE` (`'K'`) on the existing pipe, and the panel renders the state the host
  reports back — including a failure, which reverts the switch rather than leaving it
  claiming a reader that never started. **Do not add a second native transport** for this: no
  loopback control port, window message, registry signal, or file heartbeat.
- **All Kindle UI work runs on `kindle_ctl`'s own OS thread** — blocking, COM-heavy, and
  `dismiss_open_flyout` waits out a matcher timeout on every command, since the flyout it
  looks for is absent in the normal case. Off the tokio pipe runtime *and* off the synth
  worker, so it can't be stuck behind
  a page of narration and can't stall the pipe while it works. Commands on that thread are
  **serialized against each other on purpose**: a Stop *does* wait for an in-flight Play,
  because two overlapping foreground-plus-keystroke sequences aimed at one blind toggle
  would land in an unknowable order. What must never queue behind Kindle work is a *query*
  or a *pause* — and neither touches that thread.
- **`CMD_KINDLE`'s query, pause and resume are answered inline off atomics** (`state.rs`),
  never on that thread. A query also *kicks* a refresh (coalesced, rate-limited) and answers
  from cache, so no heartbeat ever waits on UIA. That refresh can learn a Read Aloud **started**
  inside Kindle (a synth stream appears); it cannot learn one was **stopped** there, because the
  only evidence is one-way and the UIA read that could have seen it is gone by design. A manual
  stop therefore leaves the belief reading `true` until the next command corrects it.
- **Health is reachability, not a reply value.** A pipe that won't open is an offline host;
  no field can express that, which is why the old `CMD_STATUS` + `unwrap_or(false)` made a
  stopped host indistinguishable from an idle one. The panel proves it with real I/O at
  ~1 Hz and never infers it from a process name, the tray icon, a cached voice list, a
  `controls.json` timestamp, a Kindle window, or a request that worked a second ago.
- **Read Aloud is a blind toggle (Ctrl+A), so a command is only as good as the belief behind
  it.** `set_reading` refreshes that belief from evidence first and toggles only if it still
  disagrees — otherwise a second Play turns reading *off*. Evidence, in order: Kindle gone or
  restarted (definite, via the pid), then Kindle audio flowing ⇒ reading. That last one is
  **one-way**: quiet is not proof of stopped (page gaps, a pause, and slow synthesis are all
  quiet).
- **The evidence for "reading" is a synth STREAM that opened after the last command**
  (`stream_proves_reading`), not audio, and not a latch. A stream open for Kindle means its
  narrator asked for a page — stronger than audio and available seconds earlier, before the
  first sample exists. The start-time comparison against `commanded_ms` is what stops it
  arguing with a command: the stream a Stop interrupted opened *before* that Stop, so it says
  nothing about the present however long Kindle takes to abandon it. Two earlier shapes were
  wrong and both are worth remembering:
  - **The audio clock alone** undid the Stop the user had just watched succeed — audio doesn't
    cease when Read Aloud does (stream lead, the 1.5 s debounce, and Kindle possibly playing
    out the page), so a refresh in that tail concluded "reading" and **bounced the switch back
    ON**. A time-boxed grace is the same mistake wearing a number: it must guess the tail, and
    the case that most needs covering is the one that outlasts any guess.
  - **A latch cleared by observed silence** fixed the bounce and broke detection, because it
    needed something to *watch* the silence and refreshes only happen while a client is
    querying. Close the panel after a Stop, start Read Aloud inside Kindle, reopen the panel:
    the silence between was never observed, the latch was still set, and the belief stayed
    `false` while Kindle read aloud — so Stop took its early return and sent no Ctrl+A at all.
    **Compare timestamps, don't accumulate observations**: the answer must not depend on who
    was watching.
- **A new Kindle pid voids the audio clock, not just the belief.** `set_kindle_pid` zeroes
  `last_kindle_audio_ms` alongside `reading`/`paused`, because that clock measures audio
  written for a Kindle that no longer exists — and the inference would otherwise read the
  dead reader's tail as proof the *replacement* is already narrating, handing the fresh Kindle
  the identical wrong belief the reset exists to clear.
- **Never read the belief back off Kindle's own toggle.** `refresh` used to sample
  "ToggleButton-Assistive reader toggle" through UIA and adopt it as definite. That element
  is only in the tree while the **Aa menu is open** — which is exactly when the user is
  working that toggle by hand, so the only moment it could be read was the only moment it was
  changing. A sample taken between the tap and Kindle's repaint stored the stale value as
  *definite*, `set_reading` skips its Ctrl+A on a definite belief, and against a blind toggle
  that inverts Play and Stop for as long as the belief survives. Reading it is the race.
  `refresh` now touches no UIA at all: process list plus the Kindle audio clock, neither of
  which anything else is mutating. The toggle's AutomationId is still used to notice the
  flyout is **open** (it traps Ctrl+A and must be dismissed) — never to ask what it says.
- **`fetch-deps.ps1` must run before building `kokoro-host`.** `build.rs` panics if the
  provisioned dep folders under `native-deps/` (ORT + Dawn DLLs + espeak) are missing.
  It also stages the 5 runtime DLLs next to the exe.

### `controls.json` — single source of truth for SETTINGS, read live
- Lives at `%APPDATA%\com.phc260.kokoro-kindle-reader\controls.json`. The panel writes
  `voice`/`speed`/`gain`/`chunk`/`kindle_kokoro`/`gpu_synth`; the host re-reads
  `voice`/`speed`/`chunk`/`gpu_synth` per utterance and `gain` per sub-frame (via
  `native_synth::read_controls`), so a slider move lands on the next chunk/page — not
  frozen into prefetched samples (`gpu_synth` triggers a session rebuild on the next chunk,
  since the EP is fixed at session-build time). `kindle_kokoro` is read separately, per
  watcher tick, by `kindle_watch::enabled` (default `true`).
- **Invariant: every key the panel writes must be read by whichever host reader consumes it**
  — `read_controls` for the synth fields, `kindle_watch` for `kindle_kokoro`. Keep them in
  sync.
- **Settings only — a live command does not belong in this file.** `paused` used to live
  here, which made a command travel as a file the host polled, gave two processes write
  access to one document, and let a restart come back up already stalled. It is
  `HostState::paused` now: set over `CMD_KINDLE`, held in memory, read by `pipe.rs` per
  sub-frame (same mid-page stall, same held pipe). A setting is something the user chose and
  the host reads when it next needs it; a command has to *arrive*.
- The pacing lead (500 ms) / sub-frame size (250 ms) are **not** user-tunable; fixed
  constants in `pipe.rs` (`DEFAULT_LEAD_MS` / `DEFAULT_SUBFRAME_MS`).

### The browser path (Kindle Cloud Reader)
- **A browser extension cannot open a named pipe.** MV3 offers `fetch`/`WebSocket`/`chrome.*`,
  none of which address `\\.\pipe\...`; `chrome.sockets.*` was Chrome Apps only and is gone. So
  the browser needs a transport of its own.
- **There is exactly ONE browser transport: loopback HTTP** (`webserve.rs`, port 8787). A
  native-messaging bridge was prototyped first and rejected before any of it shipped. Don't add
  one as a fallback: **two transports mean every failure has to be diagnosed twice**, and the
  half that broke is never the half you're looking at.
  - HTTP needs **no registration**. Native messaging *is* per-browser configuration: registry
    keys per browser, two manifest dialects, a gecko id and a hashed Chrome id, a BOM trap, a
    mandatory browser restart — and every one of those failures surfaces as the same one string
    ("Specified native messaging host not found"), which is exactly what happened here.
  - HTTP is the only transport that *could* reach **Firefox** — a content script can `fetch`
    and, outside a Chrome service worker, own its own `AudioContext`, whereas native messaging
    needs a background script Firefox's build doesn't have. **But Firefox is NOT supported
    today**: `getNarrator()` reaches the backend through that same absent worker, so the Firefox
    build falls back to `speechSynthesis`. Chrome and Edge are what works. Don't write "the only
    route to Firefox" as though it were shipped — it's an available shape, not a feature.
  - It is reproducible outside the browser: `curl` with the bearer token *is* the transport.
  - Cost of the choice: a **pairing step**, once per browser (tray → "Web pairing code"). That's
    the price of not having the browser vouch for the client, and it's a paste.
  - **The tray is the only place that pairing code is visible** — a release host is a
    windows-subsystem exe with no console — so that menu item is load-bearing for the whole
    browser path, not a convenience. It was hidden for 0.3.3 (`SHOW_WEB_PAIRING` in `main.rs`),
    which is what made the endpoint unreachable in practice for a release that shipped no
    browser path; v0.4.x turns it back on. Delete the constant once the browser path ships.
- **The HTTP endpoint's four checks are not optional**: 127.0.0.1 bind, origin allowlist,
  constant-time bearer token, `Host` check (DNS-rebinding guard). No TLS — 127.0.0.1 is already
  a trustworthy origin and a self-signed cert defends against nobody. The security delta versus
  the pipe is narrower than "socket vs no socket" suggests: `PIPE_NAME` is already openable by
  any process running as this user (that's why `bench_busy` and `MAX_FRAME_SAMPLES` exist), so
  the local-process threat was already accepted. What a port uniquely adds is reachability from
  **web pages** — which is what the origin allowlist and bearer token address.
- **Nothing a peer SIZES is allocated or copied before the token check.** `read_head` stops at the
  blank line; `serve_conn` runs Host, Origin and bearer-token checks, and only then calls
  `read_body` or `discard_body`. State it as "peer-sized", not as "no byte of the body is read" —
  the stricter phrasing was written here first and is **false**: `BufReader`'s 8 KiB buffer will
  have prefetched the leading body bytes whenever head and body share a TCP segment, and `take` in
  `read_line_capped` bounds what the *adapter yields*, not what the reader pulls from the socket.
  That costs a fixed 8 KiB per connection, which a peer cannot influence, so the property that
  matters survives — but the sentence has to say which property that is.
  The order is the security property, not tidiness: `vec![0u8; len]` is the one allocation in the
  request path whose size a peer chooses, `serve_loop` spawns a task per accepted socket with no
  cap on how many, and the runtime being filled is the one feeding Kindle its audio — so a stranger
  declaring `Content-Length: 33554432` and sending nothing could hold 32 MiB × N for the whole
  deadline. Past the token check the peer holds the pairing token and was already inside the
  accepted threat model (same as anything that can open `PIPE_NAME`), which is what makes
  `discard_body`'s 64 MiB affordable. `reading_the_head_consumes_none_of_the_body` pins the seam
  (that the head read leaves the body retrievable), not the ordering — see below for why the
  ordering has no test.
- **The accepted cost of checking first: a bad token plus a large body loses its 401.** The reply
  is written under an upload still in flight, and the close can destroy it — the same shape as the
  413 bug above, kept deliberately this time, because draining for an unauthenticated peer is the
  thing the ordering exists to prevent. **Do not justify this with "`/status` runs first"** — it
  was justified that way here and the justification was wrong: `kwr.readPage()` posts a page image
  with no handshake having run at all. What covers it is `diagnosePaired` in `kokoro-http.ts`,
  which re-asks with a **bodiless** `/status` — that request cannot lose its reply, so it separates
  a stale token from a dead host, which is the distinction the lost 401 costs.
- **`refuse_oversized` is the only writer of the TRANSPORT's 413, and that is load-bearing.**
  Driving `serve_conn` from a test means standing up a `Ctx` (a live `NativeSynth` and
  `KindleCtl`), so nothing proves the endpoint *calls* the drain — one door that always drains is
  what stands in for the test. An earlier version of
  `an_over_cap_post_is_refused_with_a_status_the_client_can_read` inlined the drain in its own
  fixture, so deleting the drain from the endpoint left it green: it tested the test. Keep it
  calling the real function. Say "the transport's": `ocr_status_line` maps `Error::TooLarge` to
  413 as well, for a decoded image over `max_pixels`/`max_dimension`, and that one needs no drain
  because its body was read in full before the engine saw it.
- **`refuse_oversized`'s drain gets its own budget, NOT the connection deadline.** Sharing it put
  the original bug straight back for a slow upload: the drain was cancelled at 15 s and the 413
  written underneath a client still sending, which is the undeliverable refusal again. A second
  window is affordable here alone, because this runs only after the token check and
  `MAX_DISCARD_BYTES` still bounds the bytes when the clock does not.
- **An over-cap body must be READ before it is refused**, or the 413 is unreachable from a browser.
  `read_head` never *allocates* one — that is the point of the cap — but not reading it meant
  answering and closing the socket under a client still uploading: `fetch` sends no
  `Expect: 100-continue`, so Chrome's own write failed and it never read the reply. Every legible
  distinction the endpoint makes (`too_large`, the byte limit, the route) arrived as
  `TypeError: Failed to fetch` — the one error shape that names neither cause nor next action, and
  indistinguishable from the host being down. `discard_body` reads and sinks up to
  `MAX_DISCARD_BYTES` (4x the largest cap; bounded in time by `REQUEST_TIMEOUT`), and beyond that
  a reset is the honest answer to a client no browser is. **curl cannot see this bug** — it sends
  `Expect: 100-continue` and reads the 413 cleanly, which is why the transport tested healthy while
  the browser could not use it. `an_over_cap_post_is_refused_with_a_status_the_client_can_read`
  speaks the browser's dialect deliberately; suppress `Expect:` in any curl repro.
- **The `/ocr` body cap is sized for a PICTURE BOOK, not for prose** (32 MiB, and `MAX_OCR_BODY`
  must stay equal to `Limits::max_body_bytes`). Reasoning it from "a column of book text is well
  under 1 MiB" gave 8 MiB and refused a real Cloud Reader page. The overrun is structural, not
  marginal: the extension captures at the reader's own device-pixel resolution and re-encodes to
  **PNG, losslessly**, so a painterly page that arrived as a few hundred KiB of JPEG leaves the
  canvas an order of magnitude bigger. **PNG-only is not the thing to revisit** — one decoder is
  one parser reachable from a network-facing endpoint — and neither is downscaling before the
  post: the recognizer crops each line from the SOURCE pixels, which is what upsamples small type
  for free, so a page-wide shrink trades away the one thing resolution still buys. The cap is what
  fits the format.
- **A fetch that fails below HTTP is the only failure with no status, so it must be told where it
  was going.** `/ocr` and `/status` both name the endpoint (and `/ocr` the posted size, since the
  cap is what it is most likely to have hit), and `connectKokoro` turns it into
  `describeProbe(await probeDaemon())` — the three sentences that already existed for the unpaired
  case and never ran for a paired one. A bare `TypeError` reaching the panel is the bug, not the
  diagnosis.
- **The panel follows the SITE; its transport follows the BOOK.** It mounts on the library
  (`/kindle-library`) as well as on an open book, because everything it settles — voice, speed,
  and whether Kokoro is reachable or the browser is about to fall back to a platform voice —
  happens before reading, and mounting only on a rendered page made all of it invisible until it
  was too late to act on. **Play is two actions, chosen at press time by whether a book is open**:
  a read on the reader, and on the library a *sample of the selected voice* — there is no page
  image to capture there, so a read could only end in an error message, and a fifth control that
  is dead on one of the two surfaces is worse than four that always mean something. The glyph is
  shared, so `title`/`aria-label` are the only things that say which it is. The sample must
  never start on top of a narration, since a second utterance tears the running stream down (the
  worker bumps its generation and calls `closeAll()`) — it would end the book rather than play
  over it. That needs **two** checks: the enable-gate covers what the panel started, and the press
  asks `narrate.narrating()` about `kwr.readBook()`/`speakPage()` from the console or the debug
  bridge, which the panel never sees. It is also gated on the voice list having arrived, or the
  sample announces itself as "Loading" in whatever voice the engine defaults to.
  **`narrating()` means "not stopped", not "not finished", and `stop()` CLEARS it** instead of
  waiting for the promises to settle — an utterance is not guaranteed to settle. `speakParts` has
  no reply deadline and the worker answers `end` only after its own `speakStream` returns, which
  can park on an offscreen request that never comes back; a dead offscreen document leaves the
  *port* healthy, so nothing rejects and nothing times out. Counting instead of clearing left
  Preview refusing every press for the life of the page, over a book stopped long ago. Clearing is
  only honest if Stop really silences everything, so **`stop()` stops the current narrator AND the
  one a mid-page fallback displaced**: the fallback swap exists so Stop can reach the *fallback*,
  and it left Stop unable to reach the narrator it replaced — which, when the fallback was caused
  by one failed chunk rather than a dead worker, is still holding up to `MAX_LEAD_S` of rendered
  book audio in a live offscreen document.
  **A stopped action's continuation must not clear a newer one's state**: Stop signals the engine
  and returns without settling the promise the press is awaiting, so Preview→Stop→Preview left the
  first press to wake up, call the panel idle and disable Stop while the second was still audible.
  The handlers compare an `epoch` token rather than trusting a flag the newer press has already
  re-set — Play had the same shape before any of this and now shares the token.
  Two facts, one poll (`onSurfaceChange`),
  and the highlight stays tied to the book: without a page its measurements describe something
  that is gone.
- **The voice list is ordered by Kokoro's published grades, not the alphabet.** The grade itself
  is only in the row's tooltip — the order is what it was there to say, and a "(C+)" on every row
  is a second thing to read in a 260 px panel.
  The grades are in the model card's `VOICES.md` and the spread inside one download is A to F+;
  nothing in a voice's *name* carries any of it, so alphabetical put `af_alloy` (C) above
  `af_heart` (A) and made the default selection — the first row of a group — an accident of
  spelling. It is a lookup table (`KOKORO_GRADE` in `voices.ts`), never derived from the id.
  A voice whose **id** is not in the table sorts after the graded ones and keeps its alphabetical
  order among its own kind — unknown is not the same as worst. That is every platform voice in
  practice, but the lookup reads the id and nothing else, so it is the id that decides and not
  where the voice came from.
- **The reader's own Layout setting must NOT drive the column split.** It is readable —
  `KWR_Display_Settings.maxNumberColumns` in `read.amazon.com`'s localStorage, with the Aa menu
  closed, no English labels — and it was built, plumbed through to `preprocess` and reverted, so
  don't rediscover it. Two facts kill it, and the second is the one that costs a book:
  the field is a **ceiling**, not an outcome (a narrow window or a large font renders one column
  with it set to `2`), so only `1` could ever have acted; and the value is **global, persistent
  and applies to reflowable books only**. Picture books are fixed-layout, the Layout control is
  not even shown for them, and the key keeps whatever the last reflowable book left in it —
  measured: a picture book reporting `2`. So a reader who sets Single Column and then opens a
  picture book arrives with a stale `1` that would veto the gutter split on a spread with text in
  two places, merge the columns, and — because a veto has to survive the missed-gutter retry to be
  worth anything — disable `looksInterleaved`, the one thing that could have caught it. That is
  strictly worse than measuring, on exactly the pages `v0.4.x` exists for. A stale global
  preference for a different book is weaker evidence than the pixels, which is the whole rule.
- **The extension's manifest `key` is load-bearing** even with no native messaging: an unpacked
  extension's id derives from its path, and the endpoint allowlists that id as an **origin**.
  Unpinned, the id changes whenever the folder moves and every request 403s.
- **The browser never gets the paced path**: `webserve.rs` calls `native_synth::synth` directly.
  The paced stream is correct for Kindle (the SAPI engine plays what it is handed) and wrong for
  a client that schedules onto its own AudioContext cursor and *depends* on synthesis outrunning
  playback to build a lead — pacing it clamps it to ~1.0x and puts mid-page silence back. Same
  reason `CMD_BENCH` exists: the paced path is the wrong tool whenever the caller isn't a
  real-time sink.
- **The browser shares the one serialized synth worker with Kindle**, so the two queue behind
  each other rather than contending — the only correct arrangement, since espeak has global
  state and the ORT session is owned by that worker.
- **A page's columns feed ONE utterance — never one `speak` per column.** A two-column page is
  OCR'd a column at a time so the first can be spoken while the second is still being recognized
  (~1s off a ~4s wait for the first word). A second `speak` would start a second audio stream, and
  `startStream` tears the running one down, so the page would go silent for as long as the new
  stream's first chunk takes to synthesize — about what the streaming saved. Hence `{t:'part'}` on
  the port and `PartQueue` in the worker; **every** way an utterance can end must close that queue
  (finish, Stop, superseded, port disconnect) or the worker parks on it forever. Parts are cut at
  a **sentence** end, not at the column boundary — a column runs into the next mid-sentence and
  each part is chunked separately — and each carries its own `base`, since the sentence cut means
  the bases are *not* a running total of part lengths.
- **The browser's word highlight runs on ESTIMATED boundaries, and must keep saying so.** Kindle
  gets model-derived ones over `CMD_SYNTH_ALIGNED` (see the word-timing section below); the browser
  does not, because `/synth` returns PCM and nothing else — not because the durations are
  unavailable. They are — every session is patched to expose them now, browser included — and
  carrying them across is a change to the response shape *and* the extension. So `word-timing.ts`
  splits each chunk's
  *exact* duration (the sample count) across its words by syllable count plus a punctuation beat.
  What makes that good enough is the error resetting at every chunk (one to four sentences), so
  nothing accumulates down a page. Don't add a second estimator elsewhere, and don't describe
  these as real boundaries.
- **Those marks fire on the AudioContext clock, in the offscreen document — never `setTimeout`.**
  `ctx.currentTime` stops while the context is suspended, which is what makes Pause freeze the
  highlight on the word being spoken instead of running it to the end of the page. It is also the
  clock the samples are scheduled on, so a mark cannot drift from its sound. They reach the
  narrator as broadcasts filtered by epoch, because the lead means a chunk's audio is heard long
  after the request that scheduled it was answered.
- **A live speed change is TWO things, and one of them alone does nothing audible.** The rate must
  be read per chunk from a live `SpeakOptions` object — the panel keeps one and mutates it, and the
  worker is told separately (`{t:'options', rate}`) because the port *clones* the options at `speak`
  time. A fresh object per Play froze the speed for the whole book, which is what made the slider
  look dead. But applying it only to chunks not yet sent still isn't audible for up to `MAX_LEAD_S`
  (30 s): the lead is already-rendered audio, and `speed` is a *synthesis* parameter (the model
  predicts shorter phoneme durations — which is why it doesn't shift the pitch), so those samples
  cannot be retuned. Hence `audio-retune`: stop every source that hasn't started, rewind the cursor
  to the end of the chunk being HEARD, drop that audio's marks, and re-send from there — **at the
  same chunk indices**, or every later boundary remaps onto the wrong word. The playing chunk
  finishes at the old speed; cutting it is a click for the sake of a second.
- **A change is only as prompt as the longest wait that can't see it**, so every wait between the
  slider and the flush is interruptible: the throttle's nap is sliced, and the pull for more text is
  *raced* (`raceInterrupt`) — that one has no upper bound, since a two-column page waits there while
  the second column is recognized. The pull is **held, never re-issued**: an iterator's value is
  consumed by the `next()` that produced it, so dropping one loses a chunk of the book. The worker's
  `live` options are likewise published *before* `narratorFor()` is awaited — that await is seconds
  wide on a first Play, and a slider moved inside it would otherwise find nothing to write to.
- **A flush leaves NO lead, so the chunk it resumes on must re-enter `PLAYBACK_RAMP`.** This is the
  same cold start as the top of a page and it has the same fix. Re-sending that chunk whole put a
  silence *one to two sentences long* right after the change — ~5.8 s to render a settled chunk with
  nothing buffered to hide it — which is the entire reason the ramp exists. The pieces are cut by
  `chunk()` (so they still end at a sentence or clause), each carries the character `offset` where it
  starts in its chunk, and `queueMarks` adds that to every mark — so a boundary still addresses the
  whole chunk and `remap` never learns pieces exist. `retune` reports **index and offset**, not index
  alone: a second change mid-ramp would otherwise resume at the top of a part-heard chunk and say a
  sentence twice. Chunks after the resumed one go out whole; the ramp has rebuilt the lead by then.
  Don't reach for `playbackRate` — it is instant and it sounds like a chipmunk. The retune fires on
  the slider's `change`, never `input`: each committed value costs a real re-synthesis on the one
  worker Kindle also queues behind.
- **Chunk offsets are aligned on non-whitespace characters, not summed lengths.** `chunk()` only
  drops or normalizes whitespace, so counting ink is exact; summing lengths assumes one space
  between chunks and loses a character at every paragraph break — invisible for a sentence, about
  a word wide by the foot of a page, which is precisely where a highlight is most obviously
  wrong.
- **ONE action turns the page: `ArrowRight`** — the reader's own shortcut, confirmed on a live book
  and working with the window minimized. `turnPage` (`capture.ts`) dispatches it on the page image
  (`composed`, so it bubbles out of every shadow root; `keyCode` set by hand since the constructor
  drops it), never on `document.activeElement` — pressing Play leaves the focus inside this
  extension's own panel. A next-page control found by accessible name and a tap on the forward half
  of the page were both built and both removed: they could only run once the key had failed, which
  is the worst moment to fire untested actions at the reader, and a fallback that runs only in the
  case you cannot reproduce is the same trap as a second transport. Don't re-add one; the manual
  turn below is the fallback, and it is a path that gets used.
- **A page turn is only ever claimed on EVIDENCE — a new `blob:` URL at the SAME layout.** The
  keypress having gone out is not a turn, and neither is a new URL by itself: the reader renders to
  the viewport, so a resize or zoom gives the same page a fresh URL (this is why `followReflow`
  exists), and one taken for a turn has the loop narrate the page it just read. A render is
  rejected only on positive proof of a re-layout — the viewport or the rendered size actually
  differing. **Those two are not a complete test** and the code says so: a font-size change leaves
  both identical, and only the text could tell that page apart from the next one, which is an OCR
  pass. What bounds the damage is that an accepted turn is handed back only once the page holds
  still (`waitForSettled` inside `waitForTurn`) — so when the real turn lands behind a re-render
  that was mistaken for it, the caller still captures the page that stayed rather than reading a
  page that is already gone.
- `turnPage` returning false is the last page of the book and a reader that has stopped answering
  the key, indistinguishably and deliberately; `readBook` treats both as "nothing moved", says so,
  and parks on a manual turn — which is also what absorbs a turn that was merely slow.
- **A turn is "pending" only while nobody has finished watching for it.** `turnPage` clears it when
  its own wait ran out uncancelled (whatever `waitMs` it was given — tying that to the constant
  parks the next reader for the remainder), and `settleTurn` hands it back untouched when a second
  Stop cuts *it* short, or a Stop-Play-Stop-Play consumes the turn without ever seeing it and the
  reader after that starts on a page about to be swapped.
- **A dispatched turn outlives the loop that asked for it.** `dispatchEvent` is synchronous, the
  reader's render is not, so Stop cannot unsend it. `readBook` therefore opens with
  `capture.settleTurn()`: without it a Stop-then-Play inside that beat starts reading the page
  about to be swapped, then advances off the one it was swapped to — leaving that page unread.
- **The highlight is a box over the page image, keyed to the capture it was measured on.** The
  reader is pixels, so there is no range to style; `highlight.ts` maps an OCR bbox through the
  image's *live* rect (never a cached one) and refuses to draw unless the displayed `blob:` URL is
  still the one that was OCR'd. It is the second and last shadow host this extension adds.
- **A rule that can silently remove or reorder text must act on EVIDENCE, not on appearance.**
  This is the governing rule for `src/ocr/` and it was earned: four content losses, every one from a
  threshold that encoded what a page was assumed to look like. Appearance is still allowed to
  decide what is a *candidate*; only evidence may act. In practice that means one of three shapes —
  text no book has in its body (`FURNITURE_PATTERN`, now two branches), the same thing seen on
  another page (`repeatsAcrossPages`, for running heads and for folio *slots*, whose text changes
  every page), or a decision checked after the fact and redone (`looksInterleaved` →
  `preprocess({columns:'split'})`). A constant that only degrades quality — `PLAYBACK_RAMP`,
  `PAD`, the word-timing weights — is not this and needs no ceremony.
- **Furniture is only ever removed on PATTERN or REPETITION — never on how a line looks.** A
  section heading and a running head are the same object geometrically: short, near the page edge,
  no closing punctuation. Every attempt to separate them by appearance lost real content (three
  rounds of it: a mid-sentence continuation line, a full-measure opening line at a larger font,
  then four section headings on a chapter-per-page layout), and every loss was silent, because
  furniture OCRs *perfectly* — no confidence or accuracy check can see the mistake. So
  `src/ocr/furniture.ts` drops a line only when `FURNITURE_PATTERN` matches it (folio, `[293]`, copyright) or it has
  already appeared in a band on **another page**. A running head is therefore read once per
  session and never again; that is the intended trade, and it is the cheap mistake — audible,
  over, and it removes none of the book.
- **An OCR pass whose text is not narrated must not touch the furniture memory.** Two of them
  exist and they take different routes. The re-split throws a read away *after* making it, so it
  wraps in `furnitureCheckpoint()` — and the rollback must happen only once the replacement is
  certain, or a read that IS narrated loses what it learned. The reflow re-OCR never needed the
  rule at all, so it passes `{trial:true}` and never reaches it: a resize repaginates, so its page
  fingerprint differs from the read being narrated and the same heading would count as seen on a
  second page. That is the poisoning bug arriving through two more doors; it has already cost a
  line of the book once.
- **Dark pages are detected from the MODE of the luminance histogram, not the mean.** The mode is
  the paper; a mean is dragged around by anything large that is not paper (a full-width plate, a
  dark figure), so a page could average its way across a threshold while plainly being black on
  white. With the paper level in hand the comparison is against 128 — the midpoint of the range,
  not a tuned number.
- **`bandSeen` counts PAGES, not sightings, which is why `pageToken` exists.** The same page gets
  recognized more than once as a matter of course (a re-render while narrating, `kwr.readPage()`
  before pressing Play), and counting those would condemn a heading on the second look at the page
  it belongs to. The token is a fingerprint of the column's letters and digits only — a resize
  reflows lines and rehyphenates words, so anything counting whitespace or line structure would
  call the same page new. It is taken over **every** line before any is dropped, so a page's
  identity never depends on the decisions being made about it.
- **The measure and sentence-end guards decide what may ENTER that memory.** Body text is set to
  the column's measure and a running head is not; a paragraph's last line is short but ends in a
  full stop. Passing either means the line is never a candidate, so it can never be dropped as a
  repeat however often the same sentence lands in a band. The spacing check beside the measure is
  not decoration: a title-left/folio-right header spans the measure too, and is told apart by
  having one huge gap where justification stretches every gap together. `measureOf` is computed
  **per column** — a page-wide figure is the wider column's and disqualifies every line of the
  other.
- **Every page logs what it withheld** (`[kwr] not narrated: …`). Keep it: it is the only evidence
  that exists when a line goes missing.
- **Fixtures are PUBLIC-DOMAIN or INVENTED — never the book being tested against.** The furniture
  rules and the OCR ground truth both need realistic book prose, so the nearest real book is the
  path of least resistance and it is the wrong one. One commentary read on the Cloud Reader reached
  six files before anyone looked: the running head, four section headings, ~20 lines of quoted
  prose, the publisher's name — rendered into `ocr-fixture.html`'s page *image*, not just its
  source — and a real ASIN in `route.test.ts`. **A real ASIN identifies a book as precisely as its
  title.** Nothing downstream can catch any of it, because a fixture built from a real book passes
  every test there is; it is the furniture problem one level up, and the same rule applies — the
  check has to happen where the text is *written*, since afterwards it is indistinguishable from
  work. What is allowed: public domain (`ocr-fixture.html`'s ground truth is *Moby-Dick*) or
  invented (the furniture fixtures are an invented harbour book; `BENCH_TEXT` is an invented
  lighthouse sentence). Inventing costs nothing, because a fixture needs the **shape** — word count,
  ink width, punctuation, whether the line ends a sentence — and never the content. That is exactly
  why the 41 replacements could be verified by preserving word counts alone. **The tell that you are
  doing it wrong is that you are pasting rather than writing.**
- **A resize re-renders the page, and the highlight has to be re-OCR'd to survive it.** The reader
  renders to the viewport, so any resize or zoom produces a new `blob:` URL with the text reflowed
  onto different lines — at which point the boxes are stale and the refusal above fires *for the
  rest of the page*. `followReflow` (in `content/index.ts`) re-OCRs each new render and hands it
  over; `relocate` finds the current word in it by matching its neighbours, since reading order
  survives a reflow even though line breaks do not. **The narration is never re-anchored** — it is
  still speaking the text captured when the page started, and only the boxes moved. No match
  (the reflow pushed the word onto the next page) and an ambiguous match both draw nothing: a
  mark in the wrong place costs more than a missing one.
- **Only `CMD_SYNTH` (`'S'`) and `CMD_SYNTH_ALIGNED` (`'A'`) are for a real-time sink**, and only
  Kindle's SAPI engine sends either. They are the same paced stream with different chunk headers
  (see the word-timing section below).
  The pipe's other commands exist because the callers are *not* sinks: `CMD_BENCH` (`'B'`) times
  the model unpaced, `CMD_PREVIEW` (`'P'`) buffers a whole clip before playing a note of it, and
  `CMD_KINDLE` (`'K'`) carries no audio at all — it's the panel's transport and its ~1 Hz health
  check. The browser doesn't use the pipe (loopback HTTP, above). **The pacing is the thing to
  ask about, not the socket:** a caller that isn't consuming in real time must not be handed the
  paced stream, which is the whole reason `'B'` and `'P'` are separate commands rather than flags
  on `'S'`. `CMD_SYNTH_RAW` (`'R'`, unpaced) and `CMD_INFO` (`'I'`) were built for the deleted
  native-messaging bridge and went with it; if a client ever needs *that* shape (unpaced, no
  gain, voice on the wire), it's in that commit.

### Cloud Reader OCR runs on the HOST (`kokoro-ocr`), and it is a detector plus a recognizer
- **The extension ships no OCR engine.** Recognition is `POST /ocr`, behind the same four checks
  as `/synth`. There is **no in-page fallback and must not be one**: a missing host is a state to
  report, not a reason to run a second engine nobody has measured against these fixtures. The
  package therefore also carries no wasm, no language data and no `wasm-unsafe-eval` — a CSP
  relaxation kept for an engine that left is a standing invitation with nothing behind it.
- **Detection and recognition are SEPARATE, and the separation is load-bearing.** A DBNet detector
  emits a text-probability map and only the regions it finds reach the CTC recognizer — which is
  what makes a Cloud Reader picture-book page (four sparse lines of serif type in the corner of a
  full-page illustration) a solvable problem at all, since that is a detection problem and not a
  segmentation one. It also decides a **product** behaviour: a running head and its folio come back
  as **two lines**, which is the only reason `repeatsAcrossPages` can match the head. An engine
  that merges them yields one line whose text changes every page, and the head is then narrated
  forever. Any replacement must return line boxes, per-word boxes, and those two separately.
- **The extension posts the page in ORIGINAL COLOUR** — not flattened, not inverted. Engine-specific
  preprocessing belongs next to the engine, and a detector looking for four words inside an
  illustration needs the contrast a flatten throws away. `preprocess` still computes the luminance
  mode, but only to know which way ink runs for the gutter search. **Do not put an inversion back in
  the extension**: a rendered dark-theme fixture reads perfectly through the backend with none, and
  if a real dark capture ever fails, the fix goes in the backend.
- **There is no page-wide upscale, and its absence is deliberate.** The recognizer resizes every
  detected line to a fixed 48 px height *from the source pixels*, so small type is upsampled per
  line for free; a 2x in front of that resamples twice and quadruples the detector's input for
  nothing. Scale was the dominant accuracy lever for the engine this replaced, which read whatever
  resolution it was handed — **do not port that reasoning across.**
- **Word boxes come from CTC timesteps, and the space class is what splits words.** Detection
  returns *line* boxes; `hasOutlierGap` measures the gap between consecutive *words*, so an engine
  without word boxes cannot drive the furniture policy at all. The timestep a character fires at is
  its x-position. The dictionary's leading empty sentinel must be dropped and a trailing space class
  appended — keeping the sentinel shifts the whole alphabet by one, and losing the space class
  leaves one run-together string with nothing to split.
- **The engine argument lives in ONE place and is not summarized again here.**
  [`kokoro-ocr/README.md`](kokoro-ocr/README.md) carries it; the bullets above are the *rules that
  follow*. Every one of them was measured against alternatives before it was written down, but the
  **figures are deliberately not in this repo** — they date, they are machine-specific, and nothing
  here can reproduce them. Don't quote benchmark numbers into the tree, and don't re-derive a rule
  from what a general-purpose OCR engine would want: several of them invert it.
- **The models are pinned by SHA-256, and the digests gate the LOAD as well as `/status`.**
  Checking them in `probe()` alone left the pin decorative where it mattered: `/status` would
  answer `corrupt` while `/ocr` recognized with whatever was on disk, and a recognizer that
  emits the same class count passes every other check and returns fluent, confident, wrong
  text. **Calling `probe()` from the load path is not the fix** — its digest cache is keyed on
  length and mtime (right for a polled endpoint, wrong for a gate), and a path checked is not a
  path reopened. Each file is read once, hashed as bytes, and committed from that same buffer
  via `commit_from_memory`; what was verified is what runs. They are re-verified on every probe, not just at install, because they are data
  reachable from a network-facing endpoint. `probe()` never builds a session (loading is
  ~10 MiB and a third of a second, and `/status` is polled), and a failed load is never cached
  — the fix for `missing` is to put the file back. The worker catches a *panic* out of the load
  too: ORT's dylib resolution has no `Result` on its failure path, and an unwind would kill the
  worker for the life of the process.
- **`kokoro-ocr` must not initialize ONNX Runtime**, and its `ort` dependency must stay identical
  to `kokoro-host`'s (`=2.0.0-rc.12`, `load-dynamic`, `default-features = false`). `ort`'s own
  rule is that the application creates the environment; two `ort` versions in one process would
  be two `OrtApi` tables against one library, and ort's defaults include *downloading* a runtime.
  **`main` calls `native_synth::init_ort` before spawning anything**, because two workers now use
  ORT and whichever builds a session first decides which library the process loads — and they do
  not decide it alike: `init_ort` names the staged DLL by absolute path, while `ort`'s lazy
  fallback honours `ORT_DYLIB_PATH` first.
- **Cancellation is a discard contract.** ORT cannot abandon a run, so the flag is checked
  between stages and before each line — fine, because a page is one detection pass plus one
  inference per line. A deadline fails the whole page rather than returning part of it, and is
  checked *before* each line for that reason: a partial column narrated as a whole one is the
  book silently going missing.
- **The installer verifies all three digests via `fetch-ocr-models.ps1 -VerifyOnly`**, never an
  existence check and never a second copy of the pins. An interrupted download leaves one file
  present and another absent, which an existence check waves through: the build succeeds, the
  package looks complete, and the host reports `missing` on the first page. It then stages the
  three files **by name** — a recursive copy would ship whatever else is in `native-deps\ocr`,
  verified by nothing.
- **`fetch-ocr-models.ps1` pins a revision per URL and pulls the recognizer from
  `media.githubusercontent.com`.** That file is Git LFS, and `raw.` answers 200 with a 132-byte
  *pointer* — the shape of download a size check waves through. The dictionary is not LFS and
  `media` 404s for anything that isn't, so the two deliberately come from different hosts.

### Word timing on the Kindle path (`model_patch.rs` + `CMD_SYNTH_ALIGNED`)
- **The stock graph already computes per-token durations and throws them away.** Kokoro is
  StyleTTS2-derived, so a length regulator drives the decoder:
  `duration_proj → Sigmoid → ReduceSum → Div(speed) → Round → Clip → CumSum → MatMul`.
  `model_patch.rs` exports `/encoder/Clip_output_0` (as `durations_frames`, float32) and
  `/encoder/CumSum_output_0` (as `duration_cumsum`, **int64** — the dtypes differ, and declaring
  both float passes `onnx.checker` then fails at ORT session creation, which is exactly what the
  forced-rejection test reproduces). **273 bytes, weights untouched, no computation added.** The
  host fetches only `durations_frames`; `duration_cumsum` is its `cumsum` and carries nothing new
  — it is an offline audit witness, and an output ORT is never asked for costs nothing.
- **The edit is applied IN MEMORY, to the bytes of the manifest-verified `model.onnx`, at session
  build time.** There is no patched file on disk, nothing extra to download and nothing to host.
  This works because `graph` is field 7 of `ModelProto` and **protobuf merges a repeated
  appearance of a singular message field**: a second `graph` carrying only `node` and `output`
  entries appends to the lists already there. So it is a pure `extend_from_slice` — no length
  prefix is rewritten, not one of the 325 MB of weights before it moves, and no protobuf writer is
  needed. That last point is why this replaced the previous design.
- **It replaced a 326 MB sidecar (`kokoro-claude-variant.onnx`) and must not go back.** That file
  could only be produced by a Python script and copied in by hand, so **every released install
  fell back to estimated timing** — the feature shipped to nobody. It also had to dodge the
  panel's startup verify, which deletes any file under the model dir whose SHA-256 doesn't match
  the manifest, so writing it over `model.onnx` silently reverted the feature on the next panel
  open. Hosting it, downloading it, or deriving it to disk all cost either 326 MB of bandwidth,
  326 MB of disk, or a binary nobody can reproduce. In memory costs none of them, and **the
  shipped code is the derivation**. `LEGACY_VARIANT` exists only so a host that finds a leftover
  copy can say once that it is dead weight; it is never loaded.
- **The encoder is byte-matched against `onnx`'s own serialization, not trusted.**
  `model_patch::tests` asserts the emitted bytes equal the `graph.node` + `graph.output` ranges
  lifted verbatim out of the old `kokoro-claude-variant.onnx` (186 + 84 = 270, exactly how much
  larger that file was). Hand-rolling protobuf is fine; hand-rolling it *unverified* is not — a
  wrong field number produces a file that still parses, into a different graph.
- **A rejected patch must cost the marks, not the audio.** If the patched bytes don't load,
  `build_session` truncates the same buffer back to what it read from disk and commits again —
  stock by construction, so the host still speaks and the chunk falls back to interpolated timing.
  The log line is deliberately *not* phrased as "the patch is bad": an unavailable execution
  provider fails the first commit too, and only the retry tells the two apart. There is **no
  capability flag** anywhere: `run_model` asks the session for `durations_frames` by name per run,
  so a stock fallback reaches the interpolated path with no bookkeeping.
- **`SAMPLES_PER_FRAME` is 600 and is asserted every run**, not trusted from the offline
  measurement: `sum(frames) * 600` must equal the waveform length. That assertion is the only
  thing that would notice the graph, the EP or the frame size moving, and the cost of missing it
  is a highlight that drifts further from the voice with every word. **Do not rescale by `speed`**
  — it is applied at `/encoder/Div`, upstream of the rounding, so the frames already describe the
  audio at the requested speed. **Do not copy the public demo's hard-coded divisor or `-3`**; they
  exist to paper over pre-rounding floats and are wrong here.
- **Verified on this machine, through the real host, both EPs: the patch does not change the
  audio.** Stock vs patched is bit-identical **per EP** — WebGPU `b04d6e8b…` both ways, CPU
  `66ddbf57…` both ways. The stock side of each pair was produced by *forcing* the fallback (a
  deliberately mistyped `durations_frames`), so it is the same binary answering, not a different
  build. **The two EPs do NOT agree with each other**, and an earlier note here claiming they did
  was wrong — see the BOM trap below for why. That is unremarkable for different providers; what
  matters is that adding the outputs disturbed neither. (`onnx-community/Kokoro-82M-v1.0-ONNX-timestamped`
  is equivalent — `round()` on its floats recovers these frames exactly — but its output is the
  PRE-rounding tensor, and using those floats raw costs median 19 ms / p95 71 ms / max 106 ms as
  the residuals random-walk along the cumsum. This edit's edge is only that the rounding cannot be
  forgotten.)
- **A `controls.json` with a UTF-8 BOM is silently ignored in full.** `read_controls` does
  `serde_json::from_str`, which rejects the BOM, and the whole `if let Ok(v)` is skipped — so
  every setting reverts to its default, including `gpu_synth`, whose default is **Gpu**. Nothing
  is logged. This is how the earlier "WebGPU and CPU are bit-identical" result happened: the
  harness wrote the file with PowerShell 5.1's `Set-Content -Encoding utf8`, which adds a BOM, so
  the run that was supposed to be CPU was a second GPU run. **Any script that writes
  `controls.json` must write UTF-8 without a BOM** (`[System.IO.File]::WriteAllText` with
  `UTF8Encoding($false)`), and any experiment that selects an EP must confirm it from the host's
  own `session: Gpu|Cpu` log line rather than from what it wrote.
- **Timing and audio fail independently, and marks are never approximate.** `Synthesized.marks`
  empty means "no timing for this chunk" — stock model, durations that didn't validate,
  aggregation that produced nothing — and every consumer must fall back rather than read it as "no
  words". A chunk that can't be marked is still a chunk that must be spoken, so nothing in the
  duration path may fail the audio.
- **`CMD_SYNTH_ALIGNED` is chosen by the CLIENT, never by what the host can supply.** Same audio,
  same pacing, same sub-frames as `'S'`; only the chunk header differs. An aligned response with
  zero marks is a good answer, and a client that asked for `'S'` must not get headers it has no
  parser for just because marks happened to exist.
- **`CHUNK_ALIGNED` carries the chunk's ABSOLUTE start; `CHUNK_INFO` cannot.** Chunks do not abut —
  `split_text` trims whitespace at every boundary — so accumulating lengths (what `engine.rs` did,
  `chunk_start += chunk_u16`) drifts about a character per chunk: invisible at the top of a page,
  about a word wide at the foot of it, which is exactly where a highlight is most obviously wrong.
  `split_text` returns `Chunk { text, start_utf16 }` so the offset comes from where the kept text
  actually began.
- **Capability negotiation has exactly one signal: the connection dropping with zero frames.** An
  old host treats `'A'` as an unknown command byte and drops the client; there is no reply that
  means "unsupported", and an offline host looks identical until one of them answers. So the engine
  probes with an aligned request and falls back on the first sign of a drop — which comes in **two
  shapes, and only handling one leaves the fallback unreachable**: an old host reads one command
  byte and closes, so the rest of the request either lands in the pipe buffer (the first *read*
  fails) or does not (the *write* fails mid-request). It is a race, so both count as the same
  evidence. So does every other way of failing to get a request out — no host, a dead host, an
  over-long text — because none of them can be told apart without trying, and none produces a
  wrong audible answer; they all end at the same `E_FAIL` one `CreateFile` later. **Re-check the
  abort before re-sending**: the probe's read blocks for a whole chunk's synthesis, so a Stop
  pressed inside it is already pending, and the retry would set the host synthesizing behind it. The fallback is **scoped to that one utterance — nothing is cached anywhere**, which is why `Worker` holds no capability state and `begin_synth` takes
  `aligned` as an argument. Two reasons, and both had to be learned: zero frames is *necessary* for
  an old host but not *sufficient* (a current host quit between accepting the request and writing
  its first frame looks identical, over a window as wide as a chunk's synthesis); and the evidence
  **destroys the connection it is evidence about**, so the only place left to cache it is the
  *replacement* connection, which may be a restarted capable host. Per-connection caching is
  therefore the same bug with a shorter fuse. Re-probing is affordable because a probe is a
  connect, a write and a failed read, all before any audio exists.
- **A malformed mark stream costs the marks, not the page.** `read_aligned_chunk` drains the bytes
  it announced and returns the chunk with `marks` empty; only a short read is fatal. Dropping the
  connection over a highlight would silence a page — see the fast-scrolling note above.
- **What this actually bought, measured through the host on a page of book prose:** character-linear
  placement is out by a **median 261 ms, p90 734 ms, max 1.2 s** against the model's own schedule,
  40 of 76 words over 250 ms. The remaining error is phoneme-to-word attribution, not timing — the
  frames *are* the schedule the audio was generated from.
- **Interpolation stays as the fallback and must not be deleted.** It is right about the shape of a
  chunk even when it is wrong about a word, and a chunk with no marks still has to fire its events
  somewhere. `ChunkMap::offset_of` is the one place the two meet.
- **The browser path still uses its own estimator** (`word-timing.ts`) — `/synth` returns PCM and
  nothing else. The marks exist and are better (the browser shares the same patched session), but
  carrying them across is a change to the response shape *and* the extension, and it is the
  browser path's own increment. Until then, keep saying the browser's marks are estimates.

### Bitness, registration, file placement
- **The engine must stay x86** — Kindle is a 32-bit process and loads the COM DLL
  in-process by registry path. It **cannot** be merged into the x64 host.
- **Registration → `WOW6432Node`.** The 32-bit `regsvr32` writes `HKLM\SOFTWARE\Classes\…`
  into the WOW64 view — exactly what 32-bit Kindle reads.
- **Register from a stable path, never a git worktree.** The token's `InprocServer32`
  stores the absolute DLL path it was registered from; if that path goes away (e.g. an
  auto-cleaned worktree), Kindle's `LoadLibrary` fails silently and Read Aloud plays
  **nothing**. For a **dev** build, register the main checkout's
  `kokoro-sapi\target\i686-pc-windows-msvc\release\KokoroSapi.dll`.
- **Don't move `kokoro-sapi/`** — the registered token points at the DLL by path;
  relocating means re-`regsvr32`.
- **Never run an elevated artifact from a user-writable path (local EoP).** `regsvr32` runs
  a DLL's `DllRegisterServer` and the guard runs a `.ps1` — both **as admin**. So
  `voice-setup.ps1 -Action register` stages both into an `icacls`-locked
  `%ProgramData%\Kokoro Kindle Reader\engine\` and registers *those* copies, never the
  user-writable `%LOCALAPPDATA%` ones, and **fails closed** if the lock can't be set.
  `-Action unregister` executes only those locked copies too — with the keys deleted
  directly when they're absent, never a fallback to `resources\`.
  **Never point the installer's registration back at a user-writable path.** Full rationale
  and the residual (unsigned-installer) gap: [`packaging/README.md`](packaging/README.md).
- **Kindle (MSIX) shadows HKCU.** Its SAPI default voice (`DefaultTokenId`) comes from the
  package hive (`…\Packages\AMZNKindle…\SystemAppData\Helium\User.dat`), not real HKCU.
  Patch it via `reg load`/`unload` with Kindle stopped — `kindle-voice-guard.ps1 -Set
  kokoro|david`. **On Kindle 1.0.18632.0+ this `DefaultTokenId` is ignored by the narrator**
  (it uses the WinRT default — see the hook), so the guard is a harmless no-op there, kept
  only for older builds. The panel's checkbox no longer runs it; it just persists
  `kindle_kokoro`, which the host's watcher acts on (no UAC).

### The Kindle-18632 hook
- **Selection-only, in-memory, x86, slot-18.** Kindle 18632's narrator (`SpVoiceEngine` in
  `xrm120.dll`) resolves its voice from the WinRT `SpeechSynthesizer` default and applies it
  via `ISpVoice::SetVoice`, ignoring `DefaultTokenId`. The fix injects `kokoro_hook.dll` to
  patch the shared `ISpVoice` vtable **slot 18** (`SetVoice`) → Kokoro token.
- Invariants: the hook + `kokoro-inject.exe` **must stay x86** (Kindle is 32-bit; the
  injector reuses its own `LoadLibraryW` address in the target); the host (x64) **spawns**
  the injector, never injects itself; injection needs host and Kindle at the **same
  integrity** (both normal user — the watcher retries the injector a few times per Kindle
  PID, then logs and gives up when `OpenProcess` keeps failing); the
  patch is **in-memory** (gone when Kindle exits — no persistence/unhook, so disabling
  applies on Kindle's next launch). `kokoro-hook`'s `selftest` guards the slot-18 ABI.

### Where shared files live
- The synth core (`native_synth.rs` + `text.rs` + `espeak.rs` + `split_text.rs` +
  `model_patch.rs`) is in `kokoro-host/src/` — **not** in the engine crate. `text.rs`/`espeak.rs`
  are still written to be pure and self-contained (no `kokoro-host`-specific state); the standalone
  bench crate that consumed them through `#[path]` includes is gone, so nothing outside the host
  depends on that now — but the golden normalization tests do, and purity is what keeps them
  cheap.
- `model-manifest.json` + `icons/` are at the repo root (the panel embeds the manifest; the
  exes, the installer **and the browser extension** use the icons — `build.ts` copies
  `32x32.png`/`128x128.png` into each `dist/<target>/icons/` rather than keeping a second copy,
  so the toolbar and the tray can't show different art). `icons/*` are in Git LFS.
- The pipe wire constants live in the `kokoro-protocol` crate — a `path` dep of **both**
  `kokoro-host` and `kokoro-sapi`, so the two ends can't drift. Neither may hardcode them.
- The Kindle-18632 hook + injector are standalone root crates (`kokoro-hook/`,
  `kokoro-inject/`), built x86 and staged into the installer's `resources\`.
- Cloud Reader OCR is `kokoro-ocr/` — a path dep of `kokoro-host` with a **target-neutral**
  public API (no HTTP, browser, Windows-UI, pipe or synthesis types), so a non-Windows port
  reuses it unchanged and `webserve.rs` stays the only file that knows both halves. Its models
  are staged into the installer's `ocr\`, beside the exe.
- There is **no root workspace**; each crate builds standalone with its own target dir.

### Licensing of the bundle (MIT source, GPLv3 binaries)
- The project's own code is **MIT**, but the shipped app links **espeak-ng**
  (GPL-3.0-or-later) and **Slint** under its GPL-3.0-only option, so **every binary
  release is conveyed under GPLv3**. That's fine and doesn't restrict the repo — MIT is
  GPL-compatible — but it comes with obligations that live in code, not just docs.
- **`build-espeak.ps1` *modifies* espeak-ng** (the horse-hoarse `o@` revert), so GPLv3
  §5(a) requires a stated notice of modification + date. It's in
  `THIRD_PARTY_NOTICES.md`; if the patch changes, update that notice.
- **Invariant: `LICENSE` + `THIRD_PARTY_NOTICES.md` + `licenses/` must ship inside the
  installer** (staged by `build-installer.ps1`, installed by `installer.nsi`) — GPLv3
  requires the text to accompany the binaries.
- **No shipped artifact may claim plain "MIT" in its version resource.** Three places set
  it and all three must stay accurate: `installer.nsi`'s `VIAddVersionKey`, and the
  `LegalCopyright` in `kokoro-host/build.rs` + `kokoro-panel/build.rs` (Windows shows that
  string in the exe's Properties). The x86 artifacts set no copyright field and are
  genuinely MIT-only — the SAPI shim is connect-only with no GPL deps.

## Environment quirks

- **PowerShell 5.1:** don't redirect native stderr (`2>&1` + `$ErrorActionPreference=Stop`
  turns a harmless cmd-autorun line into a terminating error). `Select-Object -First`
  truncates upstream pipelines. Writing `.ps1` files: keep them **ASCII** — PS 5.1 misreads
  a UTF-8-no-BOM em-dash "—", so use "-" in scripts (Rust/`.slint` handle "—" fine).
- **Keep `installer.nsi` ASCII too.** `makensis` parses the script as **ACP** (see its
  `(ACP)` log line) since the file has no BOM — `Unicode true` only makes the *output*
  installer's strings Unicode. So a UTF-8 `…`/`—` in a user-visible `DetailPrint`/
  `MessageBox` renders as mojibake (`â€¦`) in the install UI. Use plain ASCII (`...`, `-`).
- **Reloading the extension orphans the content script in every open tab.** It keeps running with
  its panel and captured state intact and `chrome.runtime` removed, so every route back throws
  `Cannot read properties of undefined (reading '<whatever>')` — which names a line, not a cause,
  and there is nothing to retry. **Reload the reader tab too**, always, after `--stage` +
  reload-unpacked. `assertAttached` (`content/alive.ts`) is what turns it into a sentence; it is
  checked at each boundary, never once at startup, because the invalidation is mid-session by
  definition.
- **File locks:** rebuilds hit LNK1104 / "Access is denied" while Kindle holds
  `KokoroSapi.dll` or a running `kokoro-panel.exe`/`kokoro-host.exe` holds its exe — stop
  them first. Port lingers after a crashed session.
- **Slint `step`** on a `Slider` only affects keyboard/scroll, **not** mouse drag — snap
  the dragged value manually (see `SliderRow` in `panel.slint`).
- **A `windows` crate feature cannot be audited by grepping imports.** The bindings gate
  individual *functions* on the features their PARAMETER types need, so a feature is load-bearing
  with its module path appearing nowhere: `Win32_Security` is what makes `RegCreateKeyExW`
  (`kokoro-sapi`) and `CreateRemoteThread` (`kokoro-inject`) exist, via `SECURITY_ATTRIBUTES` in
  a signature nobody names, and `Win32_System_IO` is what makes `WriteFile` exist, via
  `OVERLAPPED`. Both read as unused and both were removed and put straight back. **Compile every
  removal** — and note the x86 crates are the ones this bites, where a `cargo check` on the
  default target proves nothing.
- Registering/unregistering the voice and editing the MSIX hive need elevation
  (`Start-Process -Verb RunAs`).
