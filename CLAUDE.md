# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Local, offline Kokoro-82M text-to-speech synthesized **natively on the Dawn WebGPU**
execution provider of ONNX Runtime (the same Dawn as Chrome/WebView2) — **no WebView2,
no browser, no C++**. The synth core is pure Rust: the `ort` crate drives the model on
the WebGPU EP, and espeak-ng is reached over a thin FFI. The app is two cooperating
binaries plus small x86 in-process pieces Kindle needs:

1. **`kokoro-host.exe`** — a **windowless system-tray daemon** (x64). It owns the named
   pipe, synthesizes natively, reads its settings live from `controls.json`, and runs the
   **Kindle-watcher** (`kindle_watch.rs`) that injects the hook below. It is the only
   thing that produces audio. Auto-starts hidden at login.
2. **`kokoro-panel.exe`** — a **native settings panel** (Slint/Fluent), **spawned on
   demand** from the tray "Settings" item. Pick a narrator, tune speed/volume, audition
   with a **Preview** button (synthesizes a fixed per-voice intro line; there is **no**
   free-text reading box), download/verify the model, and toggle whether Kindle narrates
   with Kokoro. Zero resident UI at idle.
3. **`KokoroSapi.dll`** — a thin **x86** COM shim (Rust, `kokoro-sapi`) that Kindle
   loads in-process. It's **connect-only**: it forwards each `Speak` over a named pipe
   to the running host, which synthesizes and returns PCM.
4. **`kokoro_hook.dll` + `kokoro-inject.exe`** — x86 pieces that make **Kindle for PC
   1.0.18632.0+** narrate with Kokoro. That build's narrator forces the WinRT/OneCore
   default voice via `ISpVoice::SetVoice`, ignoring the classic SAPI5 `DefaultTokenId`, so
   the host's watcher spawns `kokoro-inject.exe`, which LoadLibrary-injects `kokoro_hook.dll`
   into Kindle; the hook patches the shared `SetVoice` vtable slot so the Kokoro token wins.
   Only then does Kindle load `KokoroSapi.dll` (#3). See `kokoro-hook` / `kokoro-inject`.

All audio is produced by the native synth in `kokoro-host`; the SAPI engine itself does
no synthesis. **Consequence: `kokoro-host` must be running for Kindle to speak** — and it
is also what injects the hook, so one running host both hooks Kindle and serves its audio.

```
Kindle.exe (x86) ──in-proc COM (LoadLibrary + vtable)──▶ KokoroSapi.dll (x86 shim)
                                                            │ named pipe \\.\pipe\KokoroSapiSynth
                                                            ▼
      kokoro-host.exe (x64, tray): pipe.rs ──▶ native_synth.rs (Rust synth:
                                       ▲          text.rs + espeak.rs + ort/Dawn WebGPU EP)
        reads live ── controls.json ──┘        ▲ spawns "Settings"
                          ▲                     │
      kokoro-panel.exe (Slint) writes ─────────┘
```

## Commands

```powershell
# One-time: provision the synth runtime deps (Dawn ORT runtime DLLs + espeak-ng x64
# import lib/DLL + espeak-ng-data). Must run before building kokoro-host.
native-deps\tools\fetch-deps.ps1

# Build + run (Rust, x64). Right-click the tray → Settings to open the panel.
cargo run --manifest-path kokoro-host\Cargo.toml     # windowless tray daemon
cargo run --manifest-path kokoro-panel\Cargo.toml    # settings panel (or via the tray)

# SAPI engine — x86 Rust cdylib, no deps (thin COM shim + pipe client).
cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-sapi\Cargo.toml

# Kindle 18632 hook + injector — both x86 (Kindle is 32-bit; the host spawns the injector).
cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-hook\Cargo.toml
cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-inject\Cargo.toml
# Kindle-free check that the SetVoice vtable index is still 18 (needs Kokoro registered):
cargo run --release --target i686-pc-windows-msvc --manifest-path kokoro-hook\Cargo.toml --bin selftest

# Register the voice — DEV path (elevated; MUST be the 32-bit regsvr32). Same DLL path =
# registration survives rebuilds. The packaged installer does this automatically.
C:\Windows\SysWOW64\regsvr32.exe "kokoro-sapi\target\i686-pc-windows-msvc\release\KokoroSapi.dll"

# Packaged installer — builds the x86 DLL + release-builds both crates, stages everything
# (both exes + native runtime + the x86 DLL + guard scripts), then runs makensis. NSIS.
packaging\build-installer.ps1
# CI does this on a v* tag (.github/workflows/installer.yml); sapi.yml
# builds the x86 DLL + runs the COM smoke test on kokoro-sapi/** changes.

# SAPI smoke test — no Kindle, no elevation: LoadLibrary the DLL + drive the COM object
# model + Speak path (needs the host running for audio). See kokoro-sapi-smoke/.
cargo run --release --target i686-pc-windows-msvc --manifest-path kokoro-sapi-smoke\Cargo.toml
# Or the SAPI-registered path (32-BIT PowerShell, host running, DLL registered):
C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe -File kokoro-sapi\test-speak.ps1
```

No Rust test suites; "testing" is Preview in the panel and Read Aloud in Kindle (or
`test-speak.ps1`).

## Architecture

### Native synthesis (the one engine) — pure Rust
- `kokoro-host/src/native_synth.rs` — the whole synth core. Per chunk: `text.rs`
  normalize/segment → `espeak.rs` phonemize each segment → tokenize (tokenizer.json
  vocab) → the Kokoro ONNX model on the ORT **Dawn WebGPU** EP via the **`ort` crate**
  (`load-dynamic` against the staged `onnxruntime.dll`; `WebGPU::default()` EP) → f32
  PCM. espeak keeps global state and isn't thread-safe (and the `ort` session lives
  here), so **all synthesis is serialized onto one dedicated worker thread** that owns
  the session for the process lifetime; requests arrive over an mpsc channel and replies
  come back on tokio oneshots so the async pipe tasks never block. It also owns the
  `controls.json` reader (`read_controls`).
- `kokoro-host/src/text.rs` — Kokoro-js text normalization (11 passes) + punctuation
  segmentation + phoneme post-processing, on UTF-8 bytes; verified token-parity vs
  kokoro-js. `kokoro-host/src/espeak.rs` — the espeak-ng FFI + one-segment phoneme
  trace (temp-file trace via CRT `fopen`/`fclose`).
- `native-deps/` is now just **dep provisioning**: `tools/fetch-deps.ps1` populates
  `third_party/` (the `onnxruntime-webgpu` pip wheel → Dawn `onnxruntime.dll` +
  `dxcompiler.dll` + `dxil.dll` + `onnxruntime_providers_shared.dll`; `build-espeak.ps1`
  → `espeak-ng.dll` + import lib + `espeak-ng-data`). No C++ source remains.

### The host (`kokoro-host/src/`) — windowless tray daemon
- `main.rs` — a `tao` event loop with a `tray-icon` menu (Settings / Quit) and
  `auto-launch` (release-only) that registers `kokoro-host.exe --hidden` at login.
  "Settings" spawns `kokoro-panel.exe` (tracked via `Child`/`try_wait` to avoid dup
  windows). `#![windows_subsystem = "windows"]` in release so there's no console.
- `pipe.rs` — the **SAPI bridge** and the **owner of all chunking**. A tokio named-pipe
  server at `\\.\pipe\KokoroSapiSynth` speaking the wire format from the `kokoro-protocol`
  crate (pipe name, `'S'`/`'I'` commands, `STREAM_END`/`SYNTH_ERROR` markers). Each
  `'S'` request carries the **whole utterance**; `split_text` cuts it into sentence
  chunks (first chunk = 1 sentence for a fast start, then `chunk` sentences each), a
  **depth-1 prefetch pipeline** renders each chunk via `native_synth`, and the PCM is
  streamed back **frame by frame** (`[nSamples][gain][f32…]`, then a `STREAM_END` /
  `SYNTH_ERROR` marker — the u32 sentinels `0xFFFF_FFFE` / `0xFFFF_FFFF`). Gain is read
  from `controls.json` **per sub-frame**; the pacing lead (500 ms) and sub-frame size
  (250 ms) are **fixed built-in defaults** (`DEFAULT_LEAD_MS` / `DEFAULT_SUBFRAME_MS`).
- `native_synth.rs` + `text.rs` + `espeak.rs` + `split_text.rs` — live in
  `kokoro-host/src/` (plain modules). `build.rs` links the prebuilt espeak-ng import lib
  (for the `espeak.rs` FFI) and stages the 5 runtime DLLs + `espeak-ng-data` next to the
  host exe; `onnxruntime.dll` is loaded at runtime by `ort` (load-dynamic), not linked.

### The panel (`kokoro-panel/src/` + `ui/panel.slint`) — Slint, on demand
- `main.rs` wires the Slint UI to the framework-agnostic `download.rs` (model
  download/verify) and `preview.rs` (synth via the host pipe + rodio playback = WYSIWYG,
  same engine as Kindle). Background work runs on threads and pushes results back via
  `upgrade_in_event_loop`. The "Narrate Kindle with Kokoro" checkbox just persists
  `kindle_kokoro` to `controls.json` (no elevation) — the host's Kindle-watcher acts on it.
- The narrator list is derived from the embedded `model-manifest.json` (accent from
  `id[0]` a/b, gender from `id[1]` f/m). Controls persist to `controls.json`.

### Settings — `controls.json` (single source of truth)
- Lives at `%APPDATA%\com.phc260.kokoro-kindle-reader\controls.json`. Fields: **`voice`,
  `speed`, `gain`, `chunk`, `kindle_kokoro`**. The panel writes it; the host reads it
  **live** — `voice`/`speed`/`gain`/`chunk` per utterance/sub-frame (so a change lands on
  Kindle's **next page**, no IPC/restart), and `kindle_kokoro` per watcher tick in
  `kindle_watch.rs` (gates auto-injection; default `true`). (The pacing lead/sub-frame are
  *not* in the file; they're fixed constants in `pipe.rs`.)

### Kindle 18632 hook (`kokoro-host/src/kindle_watch.rs` + `kokoro-hook/` + `kokoro-inject/`)
Kindle for PC 1.0.18632.0 (2026-07-07) added a new native narrator (`SpVoiceEngine` in
`xrm120.dll`) that resolves its voice from the **WinRT `SpeechSynthesizer` default** and
applies it via `ISpVoice::SetVoice`, ignoring the classic SAPI5 `DefaultTokenId` — so
Kokoro is never selected even though it's registered. The engine is still classic
`ISpVoice`, so the fix is to change *which token reaches `SetVoice`*:
- `kokoro-host/src/kindle_watch.rs` — the host's watcher. On a 4 s `tao` timer it polls for
  `Kindle.exe`; when a new PID appears and `kindle_kokoro` is on, it spawns `kokoro-inject.exe`
  (edge-triggered per PID). The host (x64) does **not** inject itself.
- `kokoro-inject` — x86 exe that `LoadLibrary`-injects the hook (`OpenProcess` +
  `VirtualAllocEx` + `WriteProcessMemory` + `CreateRemoteThread(LoadLibraryW)`). x86 so its
  own `kernel32!LoadLibraryW` address is valid in x86 Kindle.
- `kokoro-hook` — x86 cdylib. Its `DllMain` patches the process-shared `ISpVoice` vtable slot
  for `SetVoice` (**index 18**) to force the Kokoro token. `selftest` bin proves the override
  Kindle-free (guards the slot-18 ABI). The patch is in-memory only; it dies with Kindle.

### SAPI engine (`kokoro-sapi/`) — Rust x86 cdylib, connect-only, no deps
A `crate-type = ["cdylib"]` built for `i686-pc-windows-msvc` (Kindle is 32-bit). It
uses `windows-rs` for the COM plumbing; the three `sapiddk.h` interfaces (`ISpTTSEngine`,
`ISpTTSEngineSite`, `ISpObjectWithToken`) are **hand-declared** via `#[interface]` since
windows-rs ships only the SAPI *SDK* surface. `panic = "abort"` — a Rust panic must never
unwind into Kindle.
- `lib.rs` — the four exported COM entry points (`DllGetClassObject` / `DllCanUnloadNow` /
  `DllRegisterServer` / `DllUnregisterServer`), `DllMain`, the class factory, and
  registration (writes the CLSID `InprocServer32` + the `KokoroTTS` voice token).
  `#[no_mangle] extern "system"` exports them undecorated, so **no `.def` file is needed**.
- `engine.rs` — `KokoroEngine` (`ISpTTSEngine` + `ISpObjectWithToken`), a pure streaming
  sink: `Speak` forwards the whole utterance over the pipe, loops the response frames,
  applies carried gain × host volume, and writes ~250 ms blocks with `SPVES_ABORT` checks.
- `worker.rs` — the pipe **client** (connect-only, no spawn); `sapi.rs` — the interface
  declarations; the wire format is the shared **`kokoro-protocol`** crate (used by both
  the DLL and `pipe.rs` — one source of truth).
- The `voice-setup.ps1` / `kindle-voice-guard.ps1` / `test-speak.ps1` scripts live here.
  Verified by `kokoro-sapi-smoke` (no-Kindle COM + Speak smoke test).

### Packaging / installer
- **Standalone NSIS** via `makensis` (`packaging/installer.nsi` + `build-installer.ps1`)
  — **not** a Tauri bundler. `build-installer.ps1` release-builds both crates, builds the
  three x86 artifacts (`KokoroSapi.dll` + `kokoro_hook.dll` + `kokoro-inject.exe`), stages
  the two exes + the 5 runtime DLLs + `espeak-ng-data` + `icons/icon.ico` + those x86
  artifacts + guard scripts (the x86 files + scripts go under `resources\`), then runs `makensis`.
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
  `voice`/`speed`/`gain`/`chunk`/`kindle_kokoro`; the host re-reads `voice`/`speed`/`chunk`
  per utterance and `gain` per sub-frame (via `native_synth::read_controls`), so a slider
  move lands on the next chunk/page — not frozen into prefetched samples. `kindle_kokoro` is
  read separately, per watcher tick, by `kindle_watch::enabled` (default `true`).
  **Invariant: every key the panel writes must be read by whichever host reader consumes it —
  `read_controls` for the synth fields, `kindle_watch` for `kindle_kokoro` — keep them in
  sync.** The pacing lead / sub-frame size are **not** user-tunable; fixed constants in `pipe.rs`.
- **Native synth is serialized.** espeak has global state + isn't thread-safe (and the
  `ort` session is owned by the worker), so ONE dedicated thread owns the synth; never
  call espeak / run the session from multiple threads.
- **`fetch-deps.ps1` must run before building `kokoro-host`.** `build.rs` panics if
  `native-deps/third_party/` (ORT + Dawn DLLs + espeak) is missing; that's what
  `fetch-deps.ps1` provisions. It also stages the 5 runtime DLLs next to the exe.
- **Registration → `WOW6432Node`.** The 32-bit `regsvr32` writes `HKLM\SOFTWARE\Classes\…`
  into the WOW64 view — exactly what 32-bit Kindle reads.
- **Register from a stable path, never a git worktree.** The token's `InprocServer32`
  stores the absolute DLL path it was registered from; if that path goes away (e.g. an
  auto-cleaned worktree), Kindle's `LoadLibrary` fails silently and Read Aloud plays
  **nothing**. For a **dev** build, register the main checkout's
  `kokoro-sapi\target\i686-pc-windows-msvc\release\KokoroSapi.dll`. The **installer** does
  *not* register the bundled `resources\KokoroSapi.dll`: `voice-setup.ps1` first copies it
  (and `kindle-voice-guard.ps1`) into an admin-owned, ACL-locked
  `%ProgramData%\Kokoro Kindle Reader\engine\` and registers *that* — so the DLL that runs
  elevated (regsvr32 → `DllRegisterServer`) and the one Kindle loads is never the
  user-writable `%LOCALAPPDATA%` copy. **Never point the installer's registration back at a
  user-writable path** — that reopens the local-EoP window below.
- **Installer is `currentUser`, registration self-elevates.** `installMode: currentUser`
  keeps the app out of `C:\Program Files` and runs the installer unelevated, but
  `DllRegisterServer` writes HKLM and the Kindle guard does `reg load`, both needing admin
  — so the hooks call `voice-setup.ps1`, which relaunches through UAC. Caveat: if UAC is
  satisfied with a **different** admin account, the guard's `$env:LOCALAPPDATA` points at
  that admin's profile and it won't find the installing user's Kindle hive (logs "hive not
  found", skips).
- **Never run an elevated artifact from a user-writable path (local EoP).** `regsvr32`
  runs a DLL's `DllRegisterServer` and the guard runs a `.ps1` — both **as admin**. If those
  files sat in `%LOCALAPPDATA%` (writable by the possibly-lower-integrity user), a same-user
  process could swap them and get code run as admin on the next install/uninstall. So
  `voice-setup.ps1 -Action register` stages both into `%ProgramData%\Kokoro Kindle Reader\`
  with an `icacls`-locked ACL (SYSTEM + Administrators Full, Users read/execute, owner =
  Administrators **group** so an admin user's medium-integrity process can't reopen the ACL
  via owner-`WRITE_DAC`), registers/runs the locked copies, and **fails closed** if the lock
  can't be set. Unregister reverts the voice, unregisters, then removes that dir. Do the
  privileged file placement from the **elevated** context, not from NSIS (which runs
  unelevated). Residual, only closable by signing the installer: the resources\ *source* and
  the entry-point `.ps1` are still user-writable, so first-install/entry-point tampering
  needs a signed installer to fully close.
- **Kindle (MSIX) shadows HKCU.** Its SAPI default voice (`DefaultTokenId`) comes from the
  package hive (`…\Packages\AMZNKindle…\SystemAppData\Helium\User.dat`), not real HKCU.
  Patch it via `reg load`/`unload` with Kindle stopped — `kindle-voice-guard.ps1 -Set
  kokoro|david`. It runs in two places: the installer (`-Set kokoro` after the token
  registers) and the uninstaller (`-Set david` before the token is deleted); both self-skip
  if the hive is absent. **On Kindle 1.0.18632.0+ this `DefaultTokenId` is ignored by the
  narrator** (it uses the WinRT default — see the Kindle-18632 hook), so the guard is a
  harmless no-op there, kept only for older builds. The panel's checkbox no longer runs the
  guard; it just persists `kindle_kokoro`, which the host's watcher acts on (no UAC).
- **The Kindle-18632 hook is selection-only, in-memory, x86, and slot-18.** Kindle 18632's
  narrator `SetVoice`s the WinRT default; the fix injects `kokoro_hook.dll` to patch the
  shared `ISpVoice` vtable **slot 18** (`SetVoice`) → Kokoro token. Invariants: the hook +
  `kokoro-inject.exe` **must stay x86** (Kindle is 32-bit; the injector reuses its own
  `LoadLibraryW` address in the target); the host (x64) **spawns** the injector, never injects
  itself; injection needs host and Kindle at the **same integrity** (both normal user — the
  watcher logs and skips if `OpenProcess` fails); the patch is **in-memory** (gone when Kindle
  exits — no persistence/unhook, so disabling applies on Kindle's next launch). `kokoro-hook`'s
  `selftest` guards the slot-18 ABI.
- **Don't move `kokoro-sapi/`** — the registered token points at the DLL by path;
  relocating means re-`regsvr32`.
- **Shared files live outside the engine crate.** the synth core (`native_synth.rs` +
  `text.rs` + `espeak.rs` + `split_text.rs`) is in
  `kokoro-host/src/`; `model-manifest.json` + `icons/` are at the repo root (the panel
  embeds the manifest; the exes + installer use the icons). `icons/*` are in Git LFS. The
  pipe wire constants are in the `kokoro-protocol` crate (a `path` dep of `kokoro-host`).
  The Kindle-18632 hook + injector are standalone root crates (`kokoro-hook/`,
  `kokoro-inject/`), built x86 and staged into the installer's `resources\`.

## Environment quirks

- **PowerShell 5.1:** don't redirect native stderr (`2>&1` + `$ErrorActionPreference=Stop`
  turns a harmless cmd-autorun line into a terminating error). `Select-Object -First`
  truncates upstream pipelines. Writing `.ps1` files: keep them **ASCII** — PS 5.1 misreads
  a UTF-8-no-BOM em-dash "—", so use "-" in scripts (Rust/`.slint` handle "—" fine).
- **Keep `installer.nsi` ASCII too.** `makensis` parses the script as **ACP** (see its
  `(ACP)` log line) since the file has no BOM — `Unicode true` only makes the *output*
  installer's strings Unicode. So a UTF-8 `…`/`—` in a user-visible `DetailPrint`/
  `MessageBox` renders as mojibake (`â€¦`) in the install UI. Use plain ASCII (`...`, `-`).
- **File locks:** rebuilds hit LNK1104 / "Access is denied" while Kindle holds
  `KokoroSapi.dll` or a running `kokoro-panel.exe`/`kokoro-host.exe` holds its exe — stop
  them first. Port lingers after a crashed session.
- **Slint `step`** on a `Slider` only affects keyboard/scroll, **not** mouse drag — snap
  the dragged value manually (see `SliderRow` in `panel.slint`).
- Registering/unregistering the voice and editing the MSIX hive need elevation
  (`Start-Process -Verb RunAs`).
