# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Local, offline Kokoro-82M text-to-speech synthesized **natively on the Dawn WebGPU**
execution provider of ONNX Runtime (the same Dawn as Chrome/WebView2) via a small C++
core — **no WebView2, no browser**. The app is two cooperating binaries plus a thin
x86 SAPI shim:

1. **`kokoro-host.exe`** — a **windowless system-tray daemon** (x64). It owns the named
   pipe, synthesizes natively, and reads its settings live from `controls.json`. It is
   the only thing that produces audio. Auto-starts hidden at login.
2. **`kokoro-panel.exe`** — a **native settings panel** (Slint/Fluent), **spawned on
   demand** from the tray "Settings" item. Pick a narrator, tune speed/volume, audition
   with a **Preview** button (synthesizes a fixed per-voice intro line; there is **no**
   free-text reading box), download/verify the model, and toggle Kindle's default voice.
   Zero resident UI at idle.
3. **`KokoroSapi.dll`** — a thin **x86** COM shim that Kindle loads in-process. It's
   **connect-only**: it forwards each `Speak` over a named pipe to the running host,
   which synthesizes and returns PCM. (Unchanged from the earlier WebView2 edition.)

All audio is produced by the native synth in `kokoro-host`; the SAPI engine itself does
no synthesis. **Consequence: `kokoro-host` must be running for Kindle to speak.**

```
Kindle.exe (x86) ──in-proc COM (LoadLibrary + vtable)──▶ KokoroSapi.dll (x86 shim)
                                                            │ named pipe \\.\pipe\KokoroSapiSynth
                                                            ▼
      kokoro-host.exe (x64, tray): pipe.rs ──▶ native_synth.rs ──▶ kokoro-worker C++
                                       ▲                              (KokoroSynth, Dawn WebGPU EP)
        reads live ── controls.json ──┘        ▲ spawns "Settings"
                          ▲                     │
      kokoro-panel.exe (Slint) writes ─────────┘
```

## Commands

```powershell
# One-time: provision the C++ synth deps (ORT headers/lib + Dawn runtime DLLs +
# espeak-ng x64 + espeak-ng-data). Must run before building kokoro-host.
kokoro-worker\tools\fetch-deps.ps1

# Build + run (Rust, x64). Right-click the tray → Settings to open the panel.
cargo run --manifest-path kokoro-host\Cargo.toml     # windowless tray daemon
cargo run --manifest-path kokoro-panel\Cargo.toml    # settings panel (or via the tray)

# SAPI engine — x86 only, no deps (thin COM shim + pipe client). NMake via vcvarsall.
kokoro-sapi\build.ps1

# Register the voice — DEV path (elevated; MUST be the 32-bit regsvr32). Same DLL path =
# registration survives rebuilds. The packaged installer does this automatically.
C:\Windows\SysWOW64\regsvr32.exe "kokoro-sapi\build\KokoroSapi.dll"

# Packaged installer — release-builds both crates, stages everything (both exes + native
# runtime + the x86 DLL + guard scripts), then runs makensis. Needs NSIS installed.
packaging\build-installer.ps1
# CI does this on a v* tag (.github/workflows/headless-installer.yml); native.yml still
# builds + uploads just the x86 DLL for kokoro-sapi/** changes.

# SAPI smoke test — run under 32-BIT PowerShell, with the host running, to drive the
# engine -> pipe -> native synth path without Kindle.
C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe -File kokoro-sapi\test-speak.ps1
```

No Rust test suites; "testing" is Preview in the panel and Read Aloud in Kindle (or
`test-speak.ps1`).

## Architecture

### Native synthesis (the one engine)
- `kokoro-worker/src/` — the C++ synth core: `KokoroText.cpp` (espeak
  phonemization/normalization), `KokoroSynth.cpp` (the sentence chunker — a 1:1 mirror
  of `split_text.rs` — plus the Kokoro ONNX model on the ORT **Dawn WebGPU** EP), and
  `kokoro_ffi.cpp` (`kokoro_ffi.h`, the C ABI). `tools/fetch-deps.ps1` provisions `third_party/` (ORT release zip → include/lib;
  the `onnxruntime-webgpu` pip wheel → the Dawn `onnxruntime.dll` + `dxcompiler.dll` +
  `dxil.dll`; `build-espeak.ps1` → `espeak-ng.dll`).
- `kokoro-host/src/native_synth.rs` — the Rust FFI wrapper. espeak keeps global state
  and isn't thread-safe, so **all synthesis is serialized onto one dedicated worker
  thread** that owns the `KokoroWorker` for the process lifetime; requests arrive over
  an mpsc channel and replies come back on tokio oneshots so the async pipe tasks never
  block. It also owns the `controls.json` reader (`read_controls`).

### The host (`kokoro-host/src/`) — windowless tray daemon
- `main.rs` — a `tao` event loop with a `tray-icon` menu (Settings / Quit) and
  `auto-launch` (release-only) that registers `kokoro-host.exe --hidden` at login.
  "Settings" spawns `kokoro-panel.exe` (tracked via `Child`/`try_wait` to avoid dup
  windows). `#![windows_subsystem = "windows"]` in release so there's no console.
- `pipe.rs` — the **SAPI bridge** and the **owner of all chunking**. A tokio named-pipe
  server at `\\.\pipe\KokoroSapiSynth` speaking the `WorkerProtocol.h` wire format. Each
  `'S'` request carries the **whole utterance**; `split_text` cuts it into sentence
  chunks (first chunk = 1 sentence for a fast start, then `chunk` sentences each), a
  **depth-1 prefetch pipeline** renders each chunk via `native_synth`, and the PCM is
  streamed back **frame by frame** (`[nSamples][gain][f32…]`, then a `STREAM_END` /
  `SYNTH_ERROR` marker — the u32 sentinels `0xFFFF_FFFE` / `0xFFFF_FFFF`). Gain is read
  from `controls.json` **per sub-frame**; the pacing lead (500 ms) and sub-frame size
  (250 ms) are **fixed built-in defaults** (`DEFAULT_LEAD_MS` / `DEFAULT_SUBFRAME_MS`).
- `native_synth.rs` + `split_text.rs` — live in `kokoro-host/src/` (plain modules).
  `build.rs` compiles the `kokoro-worker` C++, links the prebuilt ORT + espeak import
  libs, and stages the 5 runtime DLLs + `espeak-ng-data` next to the host exe.

### The panel (`kokoro-panel/src/` + `ui/panel.slint`) — Slint, on demand
- `main.rs` wires the Slint UI to the framework-agnostic `download.rs` (model
  download/verify), `kindle.rs` (elevated Kindle-voice guard), and `preview.rs` (synth
  via the host pipe + rodio playback = WYSIWYG, same engine as Kindle). Background work
  runs on threads and pushes results back via `upgrade_in_event_loop`.
- The narrator list is derived from the embedded `model-manifest.json` (accent from
  `id[0]` a/b, gender from `id[1]` f/m). Controls persist to `controls.json`.

### Settings — `controls.json` (single source of truth)
- Lives at `%APPDATA%\com.phc260.kokoro-kindle-reader\controls.json`. Fields: **`voice`,
  `speed`, `gain`, `chunk`, `kindle_kokoro`**. The panel writes it; the host reads it
  **live** per utterance/sub-frame, so a narrator/speed/gain/chunk change lands on
  Kindle's **next page** — no IPC, no restart. (The pacing lead/sub-frame are *not* in
  the file; they're fixed constants in `pipe.rs`.)

### SAPI engine (`kokoro-sapi/src/`) — connect-only, no deps, UNCHANGED
- `Dll.cpp` — COM class factory + `DllRegisterServer`/`Unregister` (writes the CLSID
  `InprocServer32` and the `KokoroTTS` voice token). Registered by manual `regsvr32`
  (dev) or the installer's `voice-setup.ps1` (packaged).
- `KokoroTTSEngine.cpp` — `ISpTTSEngine`, a pure streaming sink: `Speak` sends the whole
  text (`BeginSynth`), then loops `ReadFrame` over the response stream, applying carried
  gain × host volume and writing ~250 ms blocks with `SPVES_ABORT` checks. Stop
  interrupts by closing the pipe (`WorkerClient::Close`).
- `WorkerClient.cpp` — pipe **client**: `EnsureConnected` is connect-only (no spawn).
- `WorkerProtocol.h` — the wire format, shared in spirit with `pipe.rs`. **Change it in
  both places.**

### Packaging / installer
- **Standalone NSIS** via `makensis` (`packaging/installer.nsi` + `build-installer.ps1`)
  — **not** a Tauri bundler. `build-installer.ps1` release-builds both crates, stages
  the two exes + the 5 runtime DLLs + `espeak-ng-data` + `icons/icon.ico` + the x86
  `KokoroSapi.dll` + guard scripts, then runs `makensis`.
- **Per-user (`currentUser`, unelevated)**, installs to `$LOCALAPPDATA\kokoro-kindle-reader`
  (the same path the original app used). Registration self-elevates: the install/uninstall
  sections call **`voice-setup.ps1`**, which relaunches itself through UAC
  (`Start-Process -Verb RunAs`) to `regsvr32` the DLL (HKLM/WOW6432Node) and run
  `kindle-voice-guard.ps1` (`reg load` the Kindle hive). One UAC prompt per install.
- Sets the HKCU Run value to `kokoro-host.exe --hidden` (login autostart). The uninstaller
  reverts Kindle to Microsoft David **before** unregistering, drops the Run value, and
  **offers** (default: keep, `/SD IDNO`) to delete the downloaded model — so a silent
  upgrade run doesn't force a multi-hundred-MB re-download.

## Gotchas / invariants (do not rediscover these)

- **`kokoro-host` must be running** or Kindle gets no audio (the engine's `Speak` returns
  `E_FAIL` when the pipe is absent — no fallback). It's a windowless tray daemon that
  **auto-starts hidden at login** (`auto-launch`, `--hidden`); Quit is only via the tray
  menu. Closing the settings panel does **not** stop the host. This also fixes Kindle
  **fast-scrolling** when the host is gone mid-Read-Aloud: a mid-session pipe disconnect
  makes each per-page `Speak` fail instantly, which Kindle reads as "page done" and races
  through the book — so keep the host alive.
- **The engine must stay x86** — Kindle is a 32-bit process and loads the COM DLL
  in-process by registry path. It **cannot** be merged into the x64 host; it's a separate
  file, bundled + registered by the installer.
- **`controls.json` is the single source of truth, read live.** The panel writes
  `voice`/`speed`/`gain`/`chunk`/`kindle_kokoro`; the host re-reads per utterance (voice,
  speed, chunk) and per sub-frame (gain), so a slider move lands on the next chunk/page —
  not frozen into prefetched samples. **Invariant: the keys the panel writes are the ones
  `native_synth::read_controls` reads — change them in both places.** The pacing lead /
  sub-frame size are **not** user-tunable; they're fixed constants in `pipe.rs`.
- **Native synth is serialized.** espeak has global state + isn't thread-safe, so ONE
  worker thread owns the `KokoroWorker`; never call the FFI from multiple threads.
- **`fetch-deps.ps1` must run before building `kokoro-host`.** `build.rs` panics if
  `kokoro-worker/third_party/` (ORT + Dawn DLLs + espeak) is missing; that's what
  `fetch-deps.ps1` provisions. It also stages the 5 runtime DLLs next to the exe.
- **Registration → `WOW6432Node`.** The 32-bit `regsvr32` writes `HKLM\SOFTWARE\Classes\…`
  into the WOW64 view — exactly what 32-bit Kindle reads.
- **Register from a stable path, never a git worktree.** The token's `InprocServer32`
  stores the absolute DLL path it was registered from; if that path goes away (e.g. an
  auto-cleaned worktree), Kindle's `LoadLibrary` fails silently and Read Aloud plays
  **nothing**. Always register the main checkout's `kokoro-sapi\build\KokoroSapi.dll`.
- **Installer is `currentUser`, registration self-elevates.** `installMode: currentUser`
  keeps the app out of `C:\Program Files` and runs the installer unelevated, but
  `DllRegisterServer` writes HKLM and the Kindle guard does `reg load`, both needing admin
  — so the hooks call `voice-setup.ps1`, which relaunches through UAC. Caveat: if UAC is
  satisfied with a **different** admin account, the guard's `$env:LOCALAPPDATA` points at
  that admin's profile and it won't find the installing user's Kindle hive (logs "hive not
  found", skips).
- **Kindle (MSIX) shadows HKCU.** Its SAPI default voice (`DefaultTokenId`) comes from the
  package hive (`…\Packages\AMZNKindle…\SystemAppData\Helium\User.dat`), not real HKCU.
  Patch it via `reg load`/`unload` with Kindle stopped — `kindle-voice-guard.ps1 -Set
  kokoro|david`. It runs in three places: the installer (`-Set kokoro` after the token
  registers), the uninstaller (`-Set david` before the token is deleted), and the panel's
  Kindle-voice checkbox (relaunches it elevated via UAC). All paths self-skip if the hive
  is absent. Only once the elevated guard exits 0 does the panel record the choice in
  `controls.json` (`kindle_kokoro`). The OneCore registry is a dead end — Kindle uses
  classic `SpVoice`.
- **Don't move `kokoro-sapi/`** — the registered token points at the DLL by path;
  relocating means re-`regsvr32`.
- **Shared files live outside `kokoro-sapi`.** `native_synth.rs` + `split_text.rs` are in
  `kokoro-host/src/`; `model-manifest.json` + `icons/` are at the repo root (the panel
  embeds the manifest; the exes + installer use the icons). `icons/*` are in Git LFS.

## Environment quirks

- **PowerShell 5.1:** don't redirect native stderr (`2>&1` + `$ErrorActionPreference=Stop`
  turns a harmless cmd-autorun line into a terminating error). `Select-Object -First`
  truncates upstream pipelines. Writing `.ps1` files: keep them **ASCII** — PS 5.1 misreads
  a UTF-8-no-BOM em-dash "—", so use "-" in scripts (Rust/`.slint` handle "—" fine).
- **File locks:** rebuilds hit LNK1104 / "Access is denied" while Kindle holds
  `KokoroSapi.dll` or a running `kokoro-panel.exe`/`kokoro-host.exe` holds its exe — stop
  them first. Port lingers after a crashed session.
- **Slint `step`** on a `Slider` only affects keyboard/scroll, **not** mouse drag — snap
  the dragged value manually (see `SliderRow` in `panel.slint`).
- Registering/unregistering the voice and editing the MSIX hive need elevation
  (`Start-Process -Verb RunAs`).
