# kokoro-host — the tray daemon + synth core (x64)

The windowless **system-tray daemon** and the only thing in the app that produces audio.
It owns the named pipe `\\.\pipe\KokoroSapiSynth`, synthesizes Kokoro-82M **natively on
the ORT Dawn WebGPU EP** (pure Rust — the `ort` crate + an espeak-ng FFI), and reads its
settings live from `controls.json`. It auto-starts hidden at login (`--hidden`); Quit is
via the tray menu.

**`kokoro-host` must be running for Kindle to speak** — the x86 SAPI DLL (`kokoro-sapi`)
is a thin shim that forwards each utterance here and streams PCM back. No host, no audio
(by design: the shim has no local fallback).

## Build

```powershell
# One-time: provision the synth runtime deps (must run first — build.rs panics without it).
..\native-deps\tools\fetch-deps.ps1

cargo run   # windowless tray daemon; right-click the tray → Settings for the panel
```

`build.rs` links the prebuilt espeak-ng import lib and stages the 5 runtime DLLs +
`espeak-ng-data` next to the exe; `onnxruntime.dll` is loaded at runtime by `ort`
(load-dynamic), not linked.

## Layout

| File | What |
|---|---|
| `main.rs` | `tao` event loop + `tray-icon` menu (Settings / Quit) + `auto-launch`. "Settings" spawns `kokoro-panel.exe`; a `WaitUntil` timer ticks `kindle_watch`. `#![windows_subsystem = "windows"]` in release (no console). |
| `pipe.rs` | The SAPI bridge and **owner of all chunking**: the tokio named-pipe server, `split_text` into sentence chunks, a depth-1 prefetch pipeline, and frame-by-frame streaming with pacing/sub-framing. |
| `native_synth.rs` | The synth core: normalize → phonemize → tokenize → the Kokoro ONNX model on the Dawn WebGPU EP → f32 PCM. Also the `controls.json` reader (`read_controls`). |
| `kindle_watch.rs` | Kindle-watcher: polls for `Kindle.exe`, and when `kindle_kokoro` is on, spawns the x86 `kokoro-inject.exe` to inject `kokoro_hook.dll` (restores Kokoro on Kindle 18632+). Edge-triggered per PID; never panics. |
| `text.rs` | Kokoro-js text normalization (11 passes) + punctuation segmentation; golden tests (`#[cfg(test)] mod tests`) lock token-parity with kokoro-js. |
| `espeak.rs` | The espeak-ng FFI + one-segment phoneme trace. |
| `split_text.rs` | The sentence-chunk splitter `pipe.rs` uses. |

## Invariants (do not rediscover)

- **Synthesis is serialized onto one dedicated worker thread.** espeak keeps global state
  and isn't thread-safe, and the `ort` session lives there — never run the session or
  call espeak from multiple threads.
- **`controls.json` is the single source of truth, read live** (`%APPDATA%\com.phc260.kokoro-kindle-reader\`).
  The keys `kokoro-panel` writes (`voice`, `speed`, `gain`, `chunk`, `kindle_kokoro`) must each
  be read by a host reader: `read_controls` for the synth fields, `kindle_watch::enabled` for
  `kindle_kokoro` — change them together.
- The pacing lead (500 ms) and sub-frame (250 ms) are **fixed constants** in `pipe.rs`
  (`DEFAULT_LEAD_MS` / `DEFAULT_SUBFRAME_MS`), not user-tunable.

The pipe wire format is the shared **`kokoro-protocol`** crate. See the repo-root
[`CLAUDE.md`](../CLAUDE.md) and [`ARCHITECTURE.md`](../ARCHITECTURE.md) for the full
engine chain and gotchas.
