# kokoro-reader

Local, offline text-to-speech built on [Kokoro-82M](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX),
with two front ends sharing one set of model assets:

1. **A desktop reader app** (Tauri 2 + React) — paste text, pick a narrator,
   set the speed, listen. Inference runs in-process in the Rust backend
   (ONNX Runtime via `ort`, DirectML on GPU, CPU fallback).
2. **A SAPI5 voice for Windows** — "Kokoro (SAPI5)" appears in the system
   voice list, so apps like **Kindle for PC Read Aloud** narrate books with
   Kokoro. The 32-bit engine DLL stays thin and delegates synthesis to a
   64-bit worker process over a named pipe.

```
kokoro-reader app (Tauri)                Kindle.exe (x86, MSIX)
  React UI ── invoke ──▶ Rust backend      └─ KokoroSapi.dll  (SAPI engine, x86)
              src-tauri/src/synth.rs            │ named pipe \\.\pipe\KokoroSapiSynth
              in-process: ort + espeak-ng       ▼
                                           kokoro_worker.exe (x64)
                                           C++ KokoroSynth: ONNX Runtime + DirectML
        └──────────── shared assets: kokoro-sapi/models/ ────────────┘
                (kokoro.onnx, kokoro_dml.onnx, voices/, espeak-ng-data/,
                 tokenizer.json, default_voice.txt)
```

Pipeline (both implementations): text → sentence chunks → espeak-ng IPA
phonemes → token ids (tokenizer.json vocab) → Kokoro ONNX
(`input_ids`/`style`/`speed` → 24 kHz waveform) → PCM playback.

## Layout

| Path | What |
|---|---|
| `src/` | React frontend (narrator picker, speed slider, play/stop) |
| `src-tauri/src/tts.rs` | TTS thread: command channel, rodio playback, cancellation |
| `src-tauri/src/synth.rs` | In-process synthesis (ort + espeak-ng FFI) |
| `kokoro-sapi/src/` | SAPI engine DLL (`Dll.cpp`, `KokoroTTSEngine.cpp`), shared C++ synth core (`KokoroSynth.cpp`), pipe protocol/client |
| `kokoro-sapi/tools/` | `kokoro_worker.cpp` (pipe server), `kokoro_test.cpp` (WAV harness), `onecore_probe.cpp`, `patch_convtranspose_2d.py` |
| `kokoro-sapi/models/` | Model assets (not in source control; see below) |
| `kokoro-sapi/third_party/` | onnxruntime (x86 CPU + x64 DirectML NuGets), espeak-ng 1.52 source + x86/x64 static builds, DirectML |

## Building

Prerequisites: Visual Studio (MSVC, both x86 and x64 toolchains), CMake,
Rust, [bun](https://bun.sh).

```powershell
# 1. Native: builds the x64 worker, then the x86 engine DLL
cd kokoro-sapi
.\build.ps1

# 2. Register the SAPI voice (elevated; 32-bit regsvr32 is the one that matters)
C:\Windows\SysWOW64\regsvr32.exe "kokoro-sapi\build\KokoroSapi.dll"

# 3. The reader app
cd ..
bun install
bun run tauri dev
```

### Model assets (one-time)

`kokoro-sapi/models/` needs:

- `kokoro.onnx` — the **uint8** quantized model (`onnx/model_uint8.onnx` from
  [onnx-community/Kokoro-82M-v1.0-ONNX](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX)); used by CPU inference
- `kokoro_dml.onnx` — fp32 model (`onnx/model.onnx`) **patched** for DirectML:
  `uv run --with onnx python kokoro-sapi/tools/patch_convtranspose_2d.py model.onnx kokoro_dml.onnx`
  (DML cannot execute the stock model's 1-D ConvTranspose nodes)
- `tokenizer.json` — from the same repo
- `voices/<name>.bin` — style vectors, one per narrator (e.g. `af_heart`, `am_michael`)
- `espeak-ng-data/` — produced by the espeak-ng build (`cmake --build ... --target data`)

`kokoro-sapi/switch-voice.ps1 <voice>` downloads a voice and makes it the
SAPI default; the reader app's narrator dropdown writes
`models/default_voice.txt`, which the SAPI engine re-reads on every utterance
— so changing the narrator in the app also changes Kindle's narrator.

## Kindle for PC notes

- Kindle is a **32-bit** MSIX app; the engine DLL must be x86 and registered
  under the WOW6432Node registry view (the 32-bit regsvr32 does this).
- Kindle speaks with the *default* SAPI voice, and MSIX virtualization means
  it reads `DefaultTokenId` from its **private hive**, not real HKCU. To point
  it at Kokoro: stop Kindle, `reg load` …`\Packages\AMZNKindle…\SystemAppData\Helium\User.dat`,
  set `Software\Microsoft\Speech\Voices\DefaultTokenId` to the KokoroTTS
  token, `reg unload`.
- Windows ships an old ONNX Runtime (1.17) in System32/SysWOW64; the engine
  delay-loads `onnxruntime.dll` with a hook pinning it to its own directory.

## Performance (i7-8750H + GTX 1060)

| Backend | Realtime factor |
|---|---|
| x86 CPU in-proc (uint8) | 1.5× |
| x64 CPU (uint8) | 3.1× |
| x64 DirectML (fp32 patched) | 4.1× |
