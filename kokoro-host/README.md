# kokoro-host — the tray daemon + synth core (x64)

The windowless **system-tray daemon** and the only thing in the app that produces audio.
It owns the named pipe `\\.\pipe\KokoroSapiSynth`, synthesizes Kokoro-82M **natively via
ORT** (pure Rust — the `ort` crate + an espeak-ng FFI) on the **Dawn WebGPU EP by
default, or the CPU EP** if `controls.json`'s `gpu_synth` flag is `false`, and reads its
settings live from `controls.json`. It auto-starts hidden at login (`--hidden`); Quit is
via the tray menu.

**`kokoro-host` must be running for Kindle to speak** — the x86 SAPI DLL (`kokoro-sapi`)
is a thin shim that forwards each utterance here and streams PCM back. No host, no audio
(by design: the shim has no local fallback).

## Build

```powershell
# One-time: provision the synth runtime deps (must run first — build.rs panics without it).
..\native-deps\fetch-deps.ps1

cargo run   # windowless tray daemon; right-click the tray → Settings for the panel
```

`build.rs` links the prebuilt espeak-ng import lib and stages the 5 runtime DLLs +
`espeak-ng-data` next to the exe; `onnxruntime.dll` is loaded at runtime by `ort`
(load-dynamic), not linked.

## Layout

| File | What |
|---|---|
| `main.rs` | Shared bootstrap (`boot`: paths, ORT, the one worker, `CoreCtx`, the endpoint) plus a `cfg`-selected entry point — on Linux it runs the loopback endpoint and nothing else, and treats a failure to create it as fatal. On Windows: `tao` event loop + `tray-icon` menu (Settings / Quit) + `auto-launch`. "Settings" spawns `kokoro-panel.exe`; a `WaitUntil` timer ticks `kindle_watch`. Also the one place the contexts are built: one `CoreCtx`, one `KindleState` over its `HostState`, then `KindleCtx` for the pipe and `WebCtx` for the endpoint. `#![windows_subsystem = "windows"]` in release (no console). |
| `pipe.rs` | The SAPI bridge and **owner of all chunking**: the tokio named-pipe server, `split_text` into sentence chunks, a depth-1 prefetch pipeline, and frame-by-frame streaming with pacing/sub-framing (`stream_synth`, shared by `CMD_SYNTH`, `CMD_SYNTH_ALIGNED` and `CMD_PREVIEW` - `aligned` picks the chunk header form and changes nothing else). Also answers `CMD_STATUS`, `CMD_BENCH`, and `CMD_KINDLE` (`apply_kindle`). |
| `ctx.rs` | `CoreCtx` — what a synthesis client needs whatever transport it arrived on: the app-data/model paths, the one `NativeSynth` worker, the one `HostState`, and `available_voices`. Built once in `main` and cloned into both `pipe::KindleCtx` and `webserve::WebCtx`; the clone shares rather than copies, so there is no way to end up with a second worker, audio clock or bench slot. |
| `state.rs` | `HostState`: the general lock-free cell both transports read and write — the any-client audio clock and the bench slot. Every method is non-blocking, which is what lets a heartbeat answer during a synthesis. |
| `kindle_state.rs` | `KindleState`: the same, for what the host believes **Kindle** is doing — the reading belief and the evidence rules behind it, the Kindle-only audio clock, the live pause, the pid, the open-stream count, and the `CMD_KINDLE` state byte. It sits *on* a `HostState` (so a Kindle write stamps both clocks from one reading of the wall clock) and is reachable only from the pipe path and the watcher — never from the HTTP endpoint. |
| `kindle_ctl.rs` | The Kindle-control thread — the only code in the project that touches Kindle's UI. Foregrounds Kindle and sends its Ctrl+A Read Aloud shortcut via raw `SendInput`; UI Automation is used only to find Kindle's window and to dismiss an open Aa/ToC flyout first — **never** to read the reader's state back (`refresh` touches no UIA at all). Also `WM_CLOSE`s Kindle for the panel's narration-voice dialog. Blocking and COM-heavy, so it owns a dedicated OS thread. |
| `native_synth.rs` | The synth core: normalize → phonemize → tokenize → the Kokoro ONNX model on the Dawn WebGPU or CPU EP (`Engine`, from `controls.json`'s `gpu_synth`) → f32 PCM **plus this chunk's word marks** (`Synthesized`). Loads the stock `model.onnx` with `model_patch`'s duration exports appended in memory (`build_session`), and turns the resulting per-token durations into marks via `text::aggregate_spans`; `marks` empty always means "no timing", never "no words". Also the `controls.json` reader (`read_controls`) and `bench()`, which times one EP for the panel's speed test. |
| `kindle_watch.rs` | Kindle-watcher: polls for `Kindle.exe`, and when `kindle_kokoro` is on, spawns the x86 `kokoro-inject.exe` to inject `kokoro_hook.dll` (restores Kokoro on Kindle 18632+). Edge-triggered per PID; never panics. Publishes "is Kindle running?" into `KindleState` on the way past — it already had to look. |
| `text.rs` | Kokoro-js text normalization (11 passes) + punctuation segmentation; golden tests (`#[cfg(test)] mod tests`) lock token-parity with kokoro-js. |
| `espeak.rs` | The espeak-ng FFI + one-segment phoneme trace. |
| `split_text.rs` | The sentence-chunk splitter `pipe.rs` uses. Returns `Chunk { text, start_utf16 }` - the absolute offset comes from here because chunks are trimmed and so do **not** abut. |
| `model_patch.rs` | The 273-byte ONNX graph edit appended to `model.onnx`'s bytes at session build, exporting the per-token durations the length regulator already computes (`durations_frames`, `duration_cumsum`). A pure append, because protobuf merges a repeated `ModelProto.graph`. Its unit test byte-matches the encoder against the ranges `onnx` itself serialized, so a wrong field number cannot pass as a different-but-valid graph. |

## Invariants (do not rediscover)

- **Synthesis is serialized onto one dedicated worker thread.** espeak keeps global state
  and isn't thread-safe, and the `ort` session lives there — never run the session or
  call espeak from multiple threads.
- **This host is the panel's sole authority for Kindle reading and health.** The panel sends
  intent over `CMD_KINDLE` and draws what comes back; it holds no UI Automation and never
  looks at Kindle. Two processes each forming their own belief about what Kindle was doing
  is what that replaced. All of it lands on `kindle_ctl`'s own thread — blocking UIA must
  stay off the tokio pipe runtime *and* off the synth worker, or it ends up behind a page of
  narration. Commands there are serialized against each other on purpose (a Stop waits for
  an in-flight Play — two overlapping keystroke sequences aimed at one blind toggle would
  land in an unknowable order); it's queries and pauses that must never go near that thread,
  and they don't.
- **The reading belief is never read back off Kindle's own toggle.** `refresh` once sampled
  "ToggleButton-Assistive reader toggle" through UIA and took it as definite. That element
  exists only while the Aa menu is open — i.e. while the user is working the toggle by hand —
  so its one readable moment was its one changing moment, and a stale sample stored as
  *definite* makes `set_reading` skip its Ctrl+A and invert Play and Stop. `refresh` now uses
  the process list plus the Kindle audio clock and touches no UIA, which also means a
  heartbeat's coalesced refresh costs a process enumeration instead of a matcher timeout paid
  in full. The toggle's AutomationId still marks that the flyout is **open** so it can be
  dismissed before Ctrl+A; it is never asked what it says.
- **`controls.json` is the single source of truth for SETTINGS, read live**
  (`%APPDATA%\com.phc260.kokoro-kindle-reader\`). The keys `kokoro-panel` writes (`voice`,
  `speed`, `gain`, `chunk`, `kindle_kokoro`, `gpu_synth`) must each be read by a host
  reader: `read_controls` for the synth fields (including `gpu_synth`, which triggers a
  session rebuild — the EP is fixed at session-build time), `kindle_watch::enabled` for
  `kindle_kokoro` — change them together. **Live commands don't belong in that file.** Pause
  used to, and it meant a command travelled as a file the host polled and a restart could
  come back up already stalled; it is `KindleState::paused` now, set over the pipe and read by
  `pipe.rs` per sub-frame.
- **"Speaking" is two clocks, not one.** `HostState` stamps every audio write, and
  `KindleState` stamps a *second* Kindle-only clock (and the general one with it, from a
  single reading of the wall clock) on the `CMD_SYNTH` path alone — so the panel's own preview
  (`CMD_PREVIEW`) and the browser's HTTP synthesis can't be mistaken for Kindle narrating a
  page. That distinction used to be a timing guess in the panel, and it was the reason the
  speed test couldn't safely trust an "idle" reading.
- The pacing lead (500 ms) and sub-frame (250 ms) are **fixed constants** in `pipe.rs`
  (`DEFAULT_LEAD_MS` / `DEFAULT_SUBFRAME_MS`), not user-tunable.
- **Never time synthesis through `CMD_SYNTH`** — that stream is paced to ~real time, so
  any engine faster than realtime measures ~1.0x and GPU and CPU look identical. That's
  what `CMD_BENCH` → `NativeSynth::bench` is for: same worker, same session builder, no
  pacing, on a fixed sentence the host owns (so the timing stays comparable and a client
  can't hand the worker an arbitrarily long text). It queues behind a live utterance
  rather than running beside it, which is why the panel refuses to start one while Kokoro
  is narrating Kindle — and why the host runs **one measurement at a time**
  (`HostState`'s bench slot — general, not Kindle's), answering `BENCH_BUSY` instead of queueing. Anything on the machine can open the pipe,
  and enough queued measurements would starve Kindle past the silent gap its narrator
  tolerates.

The pipe wire format is the shared **`kokoro-protocol`** crate.

See the repo-root [`CLAUDE.md`](../CLAUDE.md) and [`ARCHITECTURE.md`](../ARCHITECTURE.md)
for the full engine chain and gotchas.
