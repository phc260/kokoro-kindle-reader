# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Local, offline Kokoro-82M text-to-speech with a **single synthesis engine** —
`kokoro-js` on **WebGPU**, running in the Tauri app's webview. The app is both:

1. **A voice control panel** (Tauri 2 + React) — pick a narrator, tune speed/volume,
   and audition it with a **Preview** button (synthesizes a fixed per-voice intro
   line via `voiceIntro()`; there is **no** free-text reading box). All controls are
   gated on the Voice Mode toggle being set to Kokoro.
2. **The synthesis host for a Windows SAPI5 voice** — "Kokoro (SAPI5)" appears in
   the system voice list so 32-bit hosts like **Kindle for PC Read Aloud** narrate
   with Kokoro. A thin **x86** COM DLL (`KokoroSapi.dll`) that Kindle loads
   in-process is **connect-only**: it forwards each `Speak` over a named pipe to
   the running app, which synthesizes on WebGPU and returns PCM.

All audio is produced in the app's webview; the SAPI engine itself does no
synthesis — it's a thin COM shim + pipe client with no third-party dependencies.
**Consequence: the app must be running for Kindle to speak.**

```
Kindle.exe (x86) ──in-proc COM (LoadLibrary + vtable)──▶ KokoroSapi.dll (x86 shim)
                                                            │ named pipe \\.\pipe\KokoroSapiSynth
                                                            ▼
                          Tauri app (x64): pipe_server.rs ──emit──▶ webview
                                                            ▲          │ kokoro-js (WebGPU)
                                                            └──PCM─────┘  synth_result
```

## Commands

```powershell
# Reader app (bun, never npm)
bun install
bun run tauri dev          # Vite (port 1420) + Tauri shell; also serves the SAPI pipe
bun run build              # tsc + vite build (frontend typecheck + bundle)

# SAPI engine — x86 only, no deps (thin COM shim + pipe client). NMake via vcvarsall.
kokoro-sapi\build.ps1

# Register the voice — DEV path (elevated; MUST be the 32-bit regsvr32). Same DLL
# path = registration survives rebuilds; no re-register needed after a rebuild.
# The packaged installer does this automatically (see "Packaging / installer").
C:\Windows\SysWOW64\regsvr32.exe "kokoro-sapi\build\KokoroSapi.dll"

# Packaged installer — runs build.ps1 (so the DLL exists) then `tauri build`,
# which bundles the DLL + registers the voice via installer-hooks.nsh on install.
# CI does this on a v* tag (.github/workflows/installer.yml); locally:
kokoro-sapi\build.ps1; bun run tauri build

# SAPI smoke test — run under 32-BIT PowerShell, with the app running, to drive
# the engine -> pipe -> WebGPU path without Kindle.
C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe -File kokoro-sapi\test-speak.ps1
```

No Rust/JS test suites; "testing" is the play/stop loop in the app and Read Aloud
in Kindle (or `test-speak.ps1`).

## Architecture

### Webview synthesis (the one engine)
- `src/tts.worker.ts` loads `kokoro-js` in a Web Worker, prefers WebGPU
  (`dtype: fp32`, `model.onnx`) and falls back to wasm (`dtype: q8`,
  `model_quantized.onnx`) if WebGPU is missing or warm-up times out. `src/tts.ts`
  is the main-thread client (request/response by id). `synthesize()` returns a WAV
  URL for the UI; `synthesizeRaw()` returns raw f32 PCM for the SAPI bridge.

### Rust backend (`src-tauri/src/`)
- `lib.rs` — model **download/verify** + the `kokoro://` **asset server**
  (`serve_model_file`, honors Range + CORS preflight). Commands: `model_exists`,
  `model_location`, `download_model`, `verify_model`, `set_kindle_voice`
  (UAC-elevated guard run that flips Kindle's default voice Kokoro↔Microsoft
  David). Files download from HuggingFace into the Tauri **app-data dir** (per
  `model-manifest.json`, embedded via `include_str!`; its voice entries must stay
  in sync with `VOICES` in `src/voices.ts`). Narrator/speed/gain are **not** here
  — they live in the webview's `localStorage` (see `App.tsx`/`bridge.ts`).
- `pipe_server.rs` — the **SAPI bridge** and the **owner of all chunking**. A tokio
  named-pipe **server** at `\\.\pipe\KokoroSapiSynth` speaking the
  `WorkerProtocol.h` wire format. Each `'S'` request carries the **whole utterance**;
  `split_text` cuts it into sentence chunks (first chunk = 1 sentence for fast
  start, then `tts-chunk` sentences each, with clause/word soft-caps for run-ons),
  and a **depth-1 prefetch pipeline** (detached tokio tasks, bounded by pipe
  backpressure) renders each chunk via `emit("synth-request", {id,text,rate})` →
  the frontend (`src/bridge.ts`) folds in the localStorage narrator/speed and
  returns PCM through `synth_result` (correlated by id, a `oneshot` map). The PCM
  is streamed back to the engine **frame by frame** (`[nSamples][gain][f32…]`, then
  a `kStreamEnd`/`kSynthError` marker); gain (`tts-gain`) and the chunk count
  (`tts-chunk`) are read per request from the webview via the `gain-request` /
  `chunk-request` events. While the app runs it owns the pipe; the engine just
  connects.

### SAPI engine (`kokoro-sapi/src/`) — connect-only, ~700 lines, no deps
- `Dll.cpp` — COM class factory + `DllRegisterServer`/`Unregister` (writes the
  CLSID `InprocServer32` and the `KokoroTTS` voice token). Runs two ways: manual
  `regsvr32` (dev) and the NSIS POSTINSTALL hook (installed app). The engine holds
  no on-disk settings/asset state — narrator/speed/gain live in the app's webview
  localStorage — so no AssetDir is registered (the token's `VoiceFile` attribute
  is informational only).
- `KokoroTTSEngine.cpp` — `ISpTTSEngine`, now a **pure streaming sink** (no
  chunking — that moved to `pipe_server.rs`). `Speak` gathers the host's text,
  sends it whole (`BeginSynth`), then loops `ReadFrame` over the response stream:
  for each frame it applies the carried gain × host volume and writes ~250 ms
  blocks to the SAPI site with `SPVES_ABORT` checks. On stop it interrupts the
  in-flight stream by closing the pipe (`WorkerClient::Close` is atomic for that
  cross-thread cancel; the app's next frame-write then fails and it drops the
  rest). Note: rate is fixed per utterance — a mid-page host rate change lands on
  the next `Speak`, not the next chunk.
- `WorkerClient.cpp` — pipe **client**: `EnsureConnected` is connect-only (no
  spawn); `BeginSynth` writes the `'S'` request and `ReadFrame` pulls each PCM
  frame (or the end/error marker) off the response stream.
- `WorkerProtocol.h` — the wire format, shared in spirit with `pipe_server.rs`.
  **Change it in both places.**

### Packaging / installer
- **NSIS only — never the MSI.** `bundle.targets` is `["nsis"]`, not `"all"`.
  The SAPI registration lives **entirely** in `installer-hooks.nsh`
  (`NSIS_HOOK_POSTINSTALL` → `voice-setup.ps1 register`); WiX/MSI runs **no hooks**,
  so an `.msi` install copies files but **never registers `KokoroTTS`** (Kindle then
  can't narrate) and also ignores `installMode` (installs to `C:\Program Files`).
  The two builds are indistinguishable until runtime, so shipping the MSI just hands
  users a silently-broken installer — don't re-add `"msi"`/`"all"`.
- `tauri.conf.json` `bundle.resources` is a **map**: it pulls the x86 DLL straight
  from `../kokoro-sapi/build/KokoroSapi.dll` into the bundle's `resources/` (along
  with `kindle-voice-guard.ps1`), so `kokoro-sapi\build.ps1` **must run before
  `tauri build`** or bundling fails.
- `.github/workflows/installer.yml` — one `windows-latest` job that enforces that
  ordering (build DLL → `bun install` → `tauri build`) and uploads the NSIS/MSI on
  a `v*` tag. (`native.yml` still builds + uploads just the DLL for `kokoro-sapi/**`.)
- `src-tauri/installer-hooks.nsh` (wired via `bundle.windows.nsis.installerHooks`) —
  the installer is **`currentUser`** (per-user, out of `C:\Program Files`, runs
  unelevated), so both hooks call **`voice-setup.ps1`**, which **self-elevates via
  UAC** (the register/unregister write HKLM + `reg load` and need admin). POSTINSTALL
  runs `voice-setup.ps1 -Action register`: it `regsvr32 /s`'s the DLL (32-bit
  `$WINDIR\SysWOW64\regsvr32.exe`), then runs `kindle-voice-guard.ps1 -Set kokoro`
  to make Kokoro Kindle's default voice (self-skips if Kindle's hive is absent);
  PREUNINSTALL runs `voice-setup.ps1 -Action unregister`: `kindle-voice-guard.ps1
  -Set david` first (revert Kindle's default to Microsoft David **before** the token
  is deleted, so its hive isn't left pointing `DefaultTokenId` at a now-gone
  `KokoroTTS` token — runs while the DLL + guard still exist in `resources\`), then
  `regsvr32 /u`. Back in the (unelevated) uninstaller it then drops the autostart Run value
  (`kokoro-kindle-reader`) and **offers** to delete the per-user app data — the
  downloaded model (`$APPDATA\com.phc260.kokoro-kindle-reader`) and the WebView2
  cache (`$LOCALAPPDATA\…\EBWebView`). That prompt **defaults to "keep"** (`/SD
  IDNO`) precisely because Tauri's NSIS **reuses this uninstaller during an
  upgrade**: a silent run must not wipe the hundreds-of-MB model and force a
  re-download on every version bump; only an interactive uninstall deletes it.
  (No `models` dir / `controls.ini` seed — the engine reads no on-disk settings.)

## Gotchas / invariants (do not rediscover these)

- **The app must be running** or Kindle gets no audio (the engine's `Speak`
  returns `E_FAIL` when the pipe is absent — there's no fallback). To keep it
  alive, **closing the window only hides it to the tray** (`lib.rs`
  `on_window_event` → `prevent_close` + `hide`); Quit is only via the tray menu,
  and the app **auto-starts hidden at login** (`tauri-plugin-autostart`, launched
  with `--hidden`). This also fixes Kindle **fast-scrolling** pages when the app
  is closed mid-Read-Aloud: a mid-session pipe disconnect makes each per-page
  `Speak` fail instantly, which Kindle (already narrating) reads as "page done"
  and races through the book. Hidden-window synthesis needs the Chromium
  anti-throttle flags — set via `additionalBrowserArgs` in `tauri.conf.json`
  (`--disable-background-timer-throttling --disable-renderer-backgrounding
  --disable-backgrounding-occluded-windows`), which **replaces** Tauri's WebView2
  defaults, so `--autoplay-policy=no-user-gesture-required` is included there to
  keep Preview audio working.
- **The engine must stay x86** — Kindle is a 32-bit process and loads the COM DLL
  *in-process by registry path*. It therefore **cannot** be merged into the x64
  app; it's a separate file, bundled + registered (via `installer-hooks.nsh` in the
  packaged app). (Rewriting it in Rust is possible via `windows-rs` COM, but it'd
  still be a separate x86 cdylib.)
- **Tauri v2 needs a capability for `listen`.** `src-tauri/capabilities/default.json`
  grants `core:default` (+ `opener:default`). Without it the frontend `listen`
  silently throws "event.listen not allowed" and the bridge never receives
  requests. (Custom `invoke` commands work without a capability; core ones don't.)
- **Registration → `WOW6432Node`.** The 32-bit `regsvr32` writes
  `HKLM\SOFTWARE\Classes\…` into the WOW64 view — exactly what 32-bit Kindle reads.
- **Installer is `currentUser`, registration self-elevates.** `installMode:
  currentUser` (`tauri.conf.json`) keeps the app **out of `C:\Program Files`** (it
  installs per-user into `$LOCALAPPDATA\…\Programs`) and runs the installer
  **unelevated**. But `DllRegisterServer` writes **HKLM** (WOW6432Node) and the
  Kindle guard does `reg load`, both of which need admin — so the hooks don't call
  `regsvr32`/the guard directly; they invoke `voice-setup.ps1`, which **relaunches
  itself through UAC** (`Start-Process -Verb RunAs`) and does the privileged
  register/unregister there. So an interactive install raises **one UAC prompt**.
  (History: this was `perMachine` ≤0.1.7 — the whole installer elevated and called
  `regsvr32`/the guard inline. Don't "simplify" by calling them straight from the
  hooks again: under `currentUser` they'd run unelevated and the HKLM write +
  `reg load` would silently fail.) Caveat: if UAC is satisfied with a **different**
  admin account, the guard's `$env:LOCALAPPDATA` points at that admin's profile and
  it won't find the installing user's Kindle hive (logs "hive not found", skips) —
  same limitation the old elevated installer had.
- **Kindle (MSIX) shadows HKCU.** Its SAPI default voice (`DefaultTokenId`) comes
  from the package hive
  (`…\Packages\AMZNKindle…\SystemAppData\Helium\User.dat`), not real HKCU. Patch
  it via `reg load`/`unload` with Kindle stopped — `kindle-voice-guard.ps1 -Set
  kokoro|david` does this one-shot. It runs in four places: the installer
  POSTINSTALL hook (`-Set kokoro` after the token registers), the PREUNINSTALL
  hook (`-Set david` before the token is deleted, so Kindle isn't left pointing at
  a gone token), the in-app **Microsoft/Kokoro toggle** (`set_kindle_voice`
  relaunches it elevated via `Start-Process -Verb RunAs` → UAC), and manually if a
  Kindle update resets it. All paths self-skip if the hive is absent. The reg-load
  needs admin, so the toggle path raises a UAC prompt; only once the elevated
  guard exits 0 does the webview record the choice in `localStorage`
  (`kindle-agency`) so the UI toggle initializes correctly next launch. The
  OneCore registry is a dead end — Kindle uses classic `SpVoice`.
- **The kokoro:// scheme URL is per-platform.** macOS/Linux `kokoro://localhost/`;
  Windows/Android `http(s)://kokoro.localhost/` (WebView2 has no bare schemes).
  `tts.worker.ts` derives it from `self.location.protocol` — don't hardcode it.
- **App ↔ engine settings live in webview `localStorage`, not a file.** The
  engine sends only the host's rate-derived `rate` over the pipe; the webview owns
  the narrator (`tts-voice`), speed (`tts-speed`), gain (`tts-gain`), per-chunk
  sentence count (`tts-chunk`) and the `kindle-agency` record. The webview reads
  these per request via three event handlers in `bridge.ts`: `synth-request` folds
  `rate × tts-speed` into the synthesis speed and returns **raw** PCM; `gain-request`
  reports `tts-gain`; `chunk-request` reports `tts-chunk`. Gain rides back to the
  engine **in each PCM frame** and the engine applies it × the host's volume slider
  when it converts to int16 (so a slider move lands within the playing chunk, not
  frozen into prefetched samples); `tts-chunk` drives `split_text` in
  `pipe_server.rs`. `App.tsx` writes the keys directly — there's no `set_controls`
  round-trip and no `controls.ini` (so no path-divergence / icacls / seed-on-
  install concerns). **Invariant: the same localStorage keys the reader UI writes
  are the ones `bridge.ts` reads — change them in both places.**
- **Background WebGPU.** When the app is hidden/tray (the daemon use case),
  Chromium can throttle the renderer; pass
  `--disable-background-timer-throttling --disable-renderer-backgrounding
  --disable-backgrounding-occluded-windows` to keep WebGPU synthesizing.
- **Don't move `kokoro-sapi/`** — the registered token points at the DLL by path;
  relocating means re-`regsvr32`.
- **Register from a stable path, never a git worktree.** The token's
  `InprocServer32` stores the absolute DLL path it was registered from. If you
  `regsvr32` a DLL under `.claude/worktrees/<…>/kokoro-sapi/build/`, the path goes
  dead when the worktree is auto-cleaned — Kindle's `LoadLibrary` then fails
  silently and Read Aloud plays **nothing** (no Kindle-side `KokoroSapi.log`,
  because the DLL never loads). Always register the main checkout's
  `kokoro-sapi\build\KokoroSapi.dll`. Diagnose by reading
  `HKLM\SOFTWARE\WOW6432Node\Microsoft\Speech\Voices\Tokens\KokoroTTS` → `CLSID` →
  `…\Classes\CLSID\{clsid}\InprocServer32` and `Test-Path` it.

## Environment quirks

- **PowerShell 5.1:** don't redirect native stderr (`2>&1` + `$ErrorActionPreference=Stop`
  turns a harmless cmd-autorun "vswhere.exe is not recognized" line into a
  terminating error — run `build.ps1` without `2>&1`). `Select-Object -First`
  truncates upstream pipelines.
- **File locks:** rebuilds hit LNK1104 while Kindle holds `KokoroSapi.dll` — stop
  Kindle (and the app) first. Port 1420 lingers after a crashed dev session.
- Registering/unregistering the voice and editing the MSIX hive need elevation
  (`Start-Process -Verb RunAs`).
- Use **bun** for all JS package/script work.
