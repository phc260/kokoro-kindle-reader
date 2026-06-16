# CLAUDE.md

Local Kokoro TTS with two consumers sharing `kokoro-sapi/models/`:
a Tauri reader app (in-process Rust inference) and a SAPI5 voice for 32-bit
hosts like Kindle for PC (thin x86 DLL → x64 worker over a named pipe).
See README.md for the architecture diagram and asset setup.

## Commands

```powershell
# Native (x64 worker first, then x86 engine DLL; NMake via vcvarsall)
kokoro-sapi\build.ps1

# Quick native rebuild of one arch (build.ps1 wipes nothing if arch matches)
#   x64: cmake --build kokoro-sapi\build-x64
#   x86: cmake --build kokoro-sapi\build     (run inside matching vcvarsall env)

# Register voice (elevated; MUST be the 32-bit regsvr32)
C:\Windows\SysWOW64\regsvr32.exe "kokoro-sapi\build\KokoroSapi.dll"

# Reader app (bun, not npm)
bun run tauri dev

# Headless checks
kokoro-sapi\build-x64\kokoro_test.exe -o out.wav "text"   # C++ synth → WAV, prints provider + realtime factor
cargo run --example voicetest                              # (in src-tauri) Rust synth, narrator A/B check

# SAPI smoke test — run under 32-BIT PowerShell to see Kindle's SAPI view
C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe -File kokoro-sapi\test-speak.ps1
```

## Architecture invariants

- **The engine DLL must stay x86** (Kindle is a 32-bit process); the worker
  and the Tauri app are x64. Never link the model into the DLL — it delegates
  to the worker, with in-proc CPU as fallback only.
- The pipe protocol lives in `kokoro-sapi/src/WorkerProtocol.h`; the Rust app
  no longer uses it (in-proc `synth.rs`), but the SAPI engine does. If you
  change it, update `WorkerClient.cpp` (C++) and bump both worker + engine.
- C++ (`KokoroSynth.cpp`) and Rust (`src-tauri/src/synth.rs`) implement the
  same pipeline (phonemize → tokens → ONNX). Changes to phonemization,
  the vocab remaps, or chunking must be mirrored in both.
- The narrator is per-request everywhere. The cross-app default is
  `models/default_voice.txt` (written by the app's `tts_set_voice`, re-read by
  the SAPI engine on every `Speak`). Don't reintroduce spawn-time voice binding.

## Hard-won gotchas (do not rediscover these)

- **DML can't run the stock Kokoro model**: 1-D ConvTranspose fails at
  *execute* time ("parameter is incorrect"), fp16 additionally trips STFT.
  Use `models/kokoro_dml.onnx` (fp32, patched by
  `tools/patch_convtranspose_2d.py`). Any new session setup needs a warm-up
  inference before trusting the EP — session creation succeeds even when
  execution will fail.
- **Windows ships ORT 1.17 in System32/SysWOW64**. The engine delay-loads
  `onnxruntime.dll` with a `__pfnDliNotifyHook2` hook (KokoroSynth.cpp)
  pinning it to the DLL's own directory; the ORT C++ header resolves the API
  at static-init time, so an explicit LoadLibrary in engine code is too late.
- **Kindle (MSIX) shadows HKCU**: its SAPI default voice comes from the
  package hive (`…\Packages\AMZNKindle…\SystemAppData\Helium\User.dat`), not
  real HKCU. Patch `DefaultTokenId` inside that hive (reg load/unload,
  Kindle stopped). The OneCore voice registry is a dead end for third-party
  voices — Kindle uses classic SpVoice.
- **espeak-ng is linked statically** (x86 + x64 builds under
  `third_party/espeak-ng-1.52.0/build-*`). Consumers must define
  `LIBESPEAK_NG_EXPORT` before including `speak_lib.h` (C++) — otherwise
  dllimport `__imp_` symbols fail to link. Rust links it via build.rs
  (`espeak-ng`, `speechPlayer`, `ucd`). espeak is process-global and not
  thread-safe: synthesis is serialized (mutex in C++, single TTS thread in Rust).
- **DML device index**: 0 is the GTX 1060 on this machine; override with
  `KOKORO_DML_DEVICE` (C++ side reads it; Rust uses ort's default device).
- Vite must not watch `kokoro-sapi/**` (native rebuilds crash the watcher) —
  configured in vite.config.ts, along with `host: 127.0.0.1` (Vite 7 binds
  ::1 for localhost but the Tauri CLI polls IPv4).

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
- Use bun for all JS package/scripts work.
