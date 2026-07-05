# Headless edition (WebView2-free)

An alternative build of Kokoro Kindle Reader that drops WebView2 entirely. Instead
of hosting `kokoro-js` in a Tauri webview, it synthesizes with the **native Dawn
WebGPU** execution provider (the same Dawn as Chrome/WebView2) via a small C++ core,
driven from a headless Rust tray binary. Same Kokoro model, same audio, no browser.

Status: on branch `headless-tray`. The WebView2 edition (Tauri) still lives in
`src-tauri/` and ships from `main`; this edition is additive and does not change it.

## Why

The WebView2 edition's resident cost is dominated by the Edge/Chromium stack, which
sits idle as a synthesis daemon for Kindle. The headless edition keeps the exact
Kindle streaming path (named pipe + chunking/prefetch/pacing) but replaces the
webview synthesizer with an in-process native WebGPU synth — clean, fast, and light.

## Topology

```
Kindle.exe (x86) --in-proc COM--> KokoroSapi.dll (x86, UNCHANGED, connect-only)
                                     | named pipe \\.\pipe\KokoroSapiSynth
                                     v
   kokoro-host.exe (x64, tray, no window):
     - pipe server: split_text + depth-1 prefetch + sub-frame pacing (shared with
       src-tauri/src/pipe_server.rs's split_text.rs)
     - native_synth: serialized C++ KokoroSynth worker on the Dawn WebGPU EP
     - reads controls.json per utterance/sub-frame
     - tray (Settings / Quit) + login autostart --hidden
                                     ^ spawns "Settings"
                                     |
   kokoro-panel.exe (Slint): narrator/speed/gain/chunk -> controls.json,
     model download/verify, Kindle-voice toggle, Preview (via the pipe = WYSIWYG)
```

Settings are a shared `controls.json` under `%APPDATA%\com.phc260.kokoro-kindle-reader`;
the host reads it live, so panel changes land on Kindle's next page (no IPC/restart).

## Crates

- **`kokoro-host/`** — the headless tray binary (`tao` message loop + `tray-icon` +
  `auto-launch`). Reuses `native_synth.rs` + `split_text.rs` from `src-tauri/src/`
  via `#[path]` include; `pipe.rs` is the Tauri-free pipe server. `build.rs` compiles
  the `kokoro-worker` C++ core and stages the runtime DLLs + `espeak-ng-data`.
- **`kokoro-panel/`** — the native settings panel (Slint, Fluent theme). Reuses the
  framework-agnostic `download.rs` / `kindle.rs` / `preview.rs`.
- **`kokoro-worker/`** — the C++ synth core (KokoroSynth WebGPU + espeak) + its FFI.
- **`kokoro-sapi/`** — the x86 SAPI engine + DLL. **Unchanged** from the WebView2
  edition (connect-only; it just forwards `Speak` over the pipe).

## Build & run (dev)

```powershell
# One-time: provision the C++ deps (ORT headers/lib + Dawn runtime DLLs + espeak x64)
kokoro-worker\tools\fetch-deps.ps1

# x86 SAPI DLL (once; only needed for a real Kindle test / packaging)
kokoro-sapi\build.ps1

# Run the headless host (tray). Right-click the tray -> Settings for the panel.
cargo run --manifest-path kokoro-host/Cargo.toml
cargo run --manifest-path kokoro-panel/Cargo.toml   # or launched from the tray
```

The host owns `\\.\pipe\KokoroSapiSynth` while running; the SAPI engine (and the
panel's Preview) connect to it as clients. The app must stay running for Kindle to
narrate.

## Package

```powershell
packaging\build-installer.ps1        # release-build both crates + stage + makensis
```

Produces `packaging\kokoro-kindle-reader-<version>-setup.exe`: a per-user NSIS
installer bundling both exes + the native runtime + the x86 `KokoroSapi.dll` + guard
scripts. On install it registers the SAPI voice via `voice-setup.ps1` (self-elevating,
one UAC), sets autostart to `kokoro-host.exe --hidden`, and adds Start Menu shortcuts.
The uninstaller reverts Kindle to Microsoft David, unregisters, and offers (default:
keep) to delete the downloaded model. CI: `.github/workflows/headless-installer.yml`.

## Differences from the WebView2 edition

- No WebView2, no Tauri runtime, no `kokoro://` asset scheme, no anti-throttle
  browser flags, no `listen` capability, no poisoned voice CacheStorage.
- Settings live in `controls.json` (not the webview's `localStorage`).
- Two binaries (host + panel) instead of one Tauri app; the panel is native Slint,
  not a React webview, and is spawned on demand (zero resident UI at idle).
- The x86 SAPI engine, its wire protocol, and the Kindle registration/guard layer
  are identical.
