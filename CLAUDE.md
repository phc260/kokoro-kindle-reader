# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Local, offline Kokoro-82M text-to-speech with **two independent consumers**:

1. **Reader app** — Tauri 2 + React desktop app. Inference runs **in the
   webview** via `kokoro-js` / `@huggingface/transformers` (WebGPU, with a wasm
   fallback). The Rust backend does **not** do inference: it only downloads the
   model on first run and serves it back to the webview over a custom URI scheme.
2. **SAPI5 voice** — "Kokoro (SAPI5)" in the Windows voice list, for 32-bit
   hosts like Kindle for PC. A thin **x86** engine DLL delegates to an **x64**
   worker (native ONNX Runtime + DirectML) over a named pipe.

The two halves use **different inference engines and different asset copies** —
see the architecture notes below. README.md's diagram describes an older
in-process-Rust design that no longer matches the reader app.

## Commands

```powershell
# Reader app (bun, never npm)
bun install
bun run tauri dev          # Vite (port 1420) + Tauri shell
bun run build              # tsc + vite build (frontend typecheck + bundle)

# SAPI / native side (x64 worker first, then x86 engine DLL; NMake via vcvarsall)
kokoro-sapi\build.ps1

# Quick native rebuild of one arch (build.ps1 wipes nothing if arch matches)
#   x64: cmake --build kokoro-sapi\build-x64
#   x86: cmake --build kokoro-sapi\build     (run inside matching vcvarsall env)

# Register the voice (elevated; MUST be the 32-bit regsvr32)
C:\Windows\SysWOW64\regsvr32.exe "kokoro-sapi\build\KokoroSapi.dll"

# Native headless check: C++ synth -> WAV, prints provider + realtime factor
kokoro-sapi\build-x64\kokoro_test.exe -o out.wav "text"

# SAPI smoke test — run under 32-BIT PowerShell to see Kindle's SAPI view
C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe -File kokoro-sapi\test-speak.ps1
```

There is no Rust test suite or `voicetest` example: the reader app's Rust crate
(`src-tauri/`) is download/serve only and carries no inference deps. Frontend
"tests" are the manual play/stop loop in the running app.

## Reader app architecture

- **Inference is in the webview, not Rust.** `src/tts.worker.ts` loads
  `kokoro-js` in a Web Worker, prefers WebGPU (`dtype: fp32`) and falls back to
  wasm (`dtype: q8`) if WebGPU is missing or its warm-up times out
  (`WARMUP_TIMEOUT_MS`; ~Pascal GPUs hang on first shader compile). `src/tts.ts`
  is the main-thread client (request/response by id → WAV object URL). **Do not
  reintroduce `src-tauri/src/synth.rs` or `tts.rs`** — the in-process Rust
  inference path was removed on purpose; the app is webview-only now.
- **The Rust backend (`src-tauri/src/lib.rs`) only moves bytes.** Tauri commands:
  `model_exists`, `model_location`, `download_model`, `verify_model`. Files are
  downloaded from HuggingFace into the Tauri **app-data dir** (not
  `kokoro-sapi/models/`), verified by SHA-256, then served to the worker via the
  `kokoro://` URI scheme (`serve_model_file`, which also honors Range requests
  and answers CORS preflight — transformers.js issues cross-origin ranged GETs
  for the large onnx).
- **The custom scheme's URL differs per platform.** macOS/Linux: `kokoro://localhost/`;
  Windows/Android: `http(s)://kokoro.localhost/` (WebView2 has no bare schemes).
  `tts.worker.ts` derives the base from `self.location.protocol` — don't hardcode it.
- **The manifest is the source of truth for what gets downloaded.**
  `src-tauri/model-manifest.json` (embedded via `include_str!`) lists every file
  + size + SHA-256. Its voice entries **must stay in sync with `VOICES` in
  `src/voices.ts`** (the UI list). Voice ids: 1st letter = language (a/b), 2nd =
  gender (f/m).
- **First-run gate:** `Setup.tsx` (the `/setup` route) blocks the reader until
  `model_exists` is true; `main.tsx`'s AppGate redirects there. The chosen
  narrator persists in `localStorage("tts-voice")` — the reader app has **no**
  connection to the SAPI side's `default_voice.txt`.
- **Vite quirks (`vite.config.ts`):** ORT's `.wasm`/`.mjs` (incl. `.jsep.*`
  WebGPU variants) are served from node_modules by a dev middleware and copied
  to `dist/` on build (the worker sets `wasmPaths = "/"`). The watcher must
  ignore `src-tauri/**` and `kokoro-sapi/**` (native rebuilds crash it), and
  `host` is pinned to `127.0.0.1` (Vite binds `::1` for localhost but the Tauri
  CLI polls IPv4).

## SAPI / native architecture invariants

**The Kindle → SAPI → worker chain** (how a 32-bit app gets GPU TTS): SAPI5
voices are registry-discovered COM objects. `DllRegisterServer` (`Dll.cpp`)
writes `CLSID\{guid}\InprocServer32` → the DLL path *and* a voice token
`…\Speech\Voices\Tokens\KokoroTTS` (CLSID + `Attributes`: AssetDir, VoiceFile,
Language…). Kindle resolves its `DefaultTokenId` → CLSID → loads the DLL
**in-process** and calls `ISpTTSEngine::Speak`. The x86 engine can't use x64
DirectML, so on first `Speak` it spawns/connects `kokoro_worker.exe` and pushes
synthesis over the pipe; the worker is the only thing that touches the model.
The 32-bit `regsvr32` lands these registry writes in `WOW6432Node` — the view
32-bit Kindle reads. Kindle's `DefaultTokenId` lives in its MSIX **private hive**
(`User.dat`), not HKCU (see the Kindle gotcha below). `Speak` streams PCM to the
host site in ~250 ms blocks with `SPVES_ABORT` checks, maps SAPI rate −10..10 →
speed 1/3×..3× (log), and re-reads `default_voice.txt` per utterance.

- **The engine DLL must stay x86** (Kindle is a 32-bit process); the worker is
  x64. Never link the model into the DLL — it delegates to the worker, with
  in-proc CPU as fallback only.
- The pipe protocol lives in `kokoro-sapi/src/WorkerProtocol.h`. The worker
  (`tools/kokoro_worker.cpp`) is the pipe **server** (single-instance via
  `FILE_FLAG_FIRST_PIPE_INSTANCE`; idle-exits after 5 min); `WorkerClient.cpp`
  spawns it on demand and connects. The reader app does **not** use this pipe
  today. If you change the wire format, update `WorkerClient.cpp` and bump both
  worker + engine.
- `KokoroSynth.cpp` (C++) is the native synthesis core: phonemize (espeak-ng IPA)
  → token ids (`kokoro_vocab.h`) → ONNX. This pipeline is **mirrored by the
  upstream `kokoro-js` package** the reader app uses — not by any Rust code in
  this repo. Phonemization / vocab / chunking changes only need touching here.
- The SAPI narrator is per-request; the cross-app default is
  `kokoro-sapi/models/default_voice.txt`, re-read by the engine
  (`KokoroTTSEngine.cpp`) on every `Speak`. Don't reintroduce spawn-time voice
  binding. (Only the SAPI side reads this file.)

## Hard-won gotchas (do not rediscover these)

- **DML can't run the stock Kokoro model**: 1-D ConvTranspose fails at *execute*
  time ("parameter is incorrect"), fp16 additionally trips STFT. Use
  `models/onnx/model_dml.onnx` (fp32, patched by `tools/patch_convtranspose_2d.py`
  from HF's `onnx/model.onnx`; the CPU path uses `models/onnx/model_uint8.onnx`).
  Any new session setup needs a warm-up inference before trusting the EP —
  session creation succeeds even when execution will fail.
- **Windows ships ORT 1.17 in System32/SysWOW64.** The engine delay-loads
  `onnxruntime.dll` with a `__pfnDliNotifyHook2` hook (`KokoroSynth.cpp`) pinning
  it to the DLL's own directory; the ORT C++ header resolves the API at
  static-init time, so an explicit LoadLibrary in engine code is too late.
- **Kindle (MSIX) shadows HKCU**: its SAPI default voice comes from the package
  hive (`…\Packages\AMZNKindle…\SystemAppData\Helium\User.dat`), not real HKCU.
  Patch `DefaultTokenId` inside that hive (reg load/unload, Kindle stopped). The
  OneCore voice registry is a dead end for third-party voices — Kindle uses
  classic SpVoice.
- **espeak-ng is linked statically** (x86 + x64 builds under
  `third_party/espeak-ng-1.52.0/build-*`). Consumers must define
  `LIBESPEAK_NG_EXPORT` before including `speak_lib.h` (C++) — otherwise
  dllimport `__imp_` symbols fail to link. espeak is process-global and not
  thread-safe: synthesis is serialized (mutex in the worker).
- **DML device index**: 0 is the GTX 1060 on this machine; override with
  `KOKORO_DML_DEVICE` (C++ side reads it).

## Environment quirks

- PowerShell 5.1: don't redirect native stderr (`2>&1` + `$ErrorActionPreference=Stop`
  turns harmless stderr into terminating errors); `Select-Object -First` kills
  the upstream pipeline (truncates running builds). A stray "vswhere.exe is not
  recognized" line from a cmd autorun is noise — ignore it.
- Rebuilds fail with LNK1104/copy errors while Kindle or `kokoro_worker.exe`
  hold the binaries — stop those processes first. Port 1420 lingers after a
  crashed dev session.
- Registering/unregistering the voice and editing HKLM voice tokens needs
  elevation (`Start-Process -Verb RunAs` → UAC prompt for the user).
- Use bun for all JS package/script work.
