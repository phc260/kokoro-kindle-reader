# kokoro-reader

Local, offline text-to-speech built on [Kokoro-82M](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX),
with two **independent** front ends. They use different inference engines on
purpose (see [the engine chain](#how-kindle-reads-with-kokoro-the-engine-chain)):

1. **A desktop reader app** (Tauri 2 + React) — paste text, pick a narrator,
   listen. Inference runs **in the webview** via [`kokoro-js`](https://www.npmjs.com/package/kokoro-js)
   / `@huggingface/transformers` (WebGPU, wasm fallback). The Rust backend only
   downloads the model on first run and serves it to the webview over a custom
   `kokoro://` URI scheme — it does **not** synthesize.
2. **A SAPI5 voice for Windows** — "Kokoro (SAPI5)" appears in the system voice
   list, so apps like **Kindle for PC Read Aloud** narrate books with Kokoro.
   The 32-bit engine DLL stays thin and delegates synthesis to a 64-bit worker
   process over a named pipe.

```
Reader app (Tauri 2)                     Kindle.exe (x86, MSIX)
  React UI                                 │ classic SAPI5 (ISpVoice)
   │ Worker (kokoro-js, WebGPU)            ▼
   │ Rust backend: download + serve      KokoroSapi.dll  (SAPI engine, x86, in-proc)
   ▼   model via kokoro:// scheme          │ spawns + named pipe \\.\pipe\KokoroSapiSynth
  app-data dir (HF download)               ▼
                                          kokoro_worker.exe (x64, single-instance, warm)
                                          C++ KokoroSynth: espeak-ng → ONNX + DirectML
                                           └── assets: kokoro-sapi/models/
                                               (onnx/model_uint8.onnx, onnx/model_dml.onnx,
                                                voices/, espeak-ng-data/, tokenizer.json,
                                                default_voice.txt)
```

Synthesis pipeline (identical in both engines): text → sentence chunks →
espeak-ng IPA phonemes → token ids (tokenizer.json vocab) → Kokoro ONNX
(`input_ids`/`style`/`speed` → 24 kHz waveform) → PCM playback. The C++
`KokoroSynth` implements it natively; the app gets the same pipeline from the
upstream `kokoro-js` package.

## Layout

| Path | What |
|---|---|
| `src/` | React frontend; `tts.worker.ts` (kokoro-js Web Worker), `tts.ts` (main-thread client), `voices.ts` |
| `src-tauri/src/lib.rs` | Rust backend: model download/verify + `kokoro://` asset server (no synthesis) |
| `src-tauri/model-manifest.json` | Files the app downloads from HF (paths + sizes + SHA-256); kept in sync with `src/voices.ts` |
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

- `onnx/model_uint8.onnx` — the **uint8** quantized model (the file of the same
  name from [onnx-community/Kokoro-82M-v1.0-ONNX](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX)); used by CPU inference
- `onnx/model_dml.onnx` — HF's fp32 `onnx/model.onnx` **patched** for DirectML:
  `uv run --with onnx python kokoro-sapi/tools/patch_convtranspose_2d.py onnx/model.onnx onnx/model_dml.onnx`
  (DML cannot execute the stock model's 1-D ConvTranspose nodes)
- `tokenizer.json` — from the same repo
- `voices/<name>.bin` — style vectors, one per narrator (e.g. `af_heart`, `am_michael`)
- `espeak-ng-data/` — produced by the espeak-ng build (`cmake --build ... --target data`)

`kokoro-sapi/switch-voice.ps1 <voice>` downloads a voice and makes it the
SAPI default; the reader app's narrator dropdown writes
`models/default_voice.txt`, which the SAPI engine re-reads on every utterance
— so changing the narrator in the app also changes Kindle's narrator.

## How Kindle reads with Kokoro (the engine chain)

The trick is making a 32-bit app drive GPU-accelerated neural TTS it could
never run itself. Five things stack up:

**1. SAPI5 is a COM plugin system, and we register as the plugin.** Kindle
never knows what "Kokoro" is — it just asks SAPI for its default voice. SAPI
voices are COM objects discovered via the registry. `DllRegisterServer`
(`Dll.cpp`) writes two records: `CLSID\{guid}\InprocServer32` → the engine
DLL's path, and a voice token `…\Speech\Voices\Tokens\KokoroTTS` whose `CLSID`
points back at that GUID (plus `Attributes`: Gender, Language, **AssetDir**,
**VoiceFile**). Chain: token → CLSID → DLL. COM loads our DLL **into Kindle's
process** and asks it for `ISpTTSEngine`. We *are* the voice — nothing is
copied into Kindle's folders; the registry just references the DLL by path.

**2. Bitness + the WOW64 registry mirror.** A 32-bit process can only load a
32-bit DLL in-process, so the engine **must** be x86 (the worker is x64). When
the 32-bit `regsvr32` writes under `HKLM\SOFTWARE\Classes\…`, WOW64 redirects
it to `WOW6432Node` — exactly the view 32-bit Kindle reads. The two halves line
up automatically.

**3. Finding the *default* voice.** Kindle plays whichever token equals
`DefaultTokenId`. Because it's a sandboxed MSIX app, that value lives in
Kindle's **private hive** (`…\Packages\AMZNKindle…\SystemAppData\Helium\User.dat`),
not real HKCU. Point it at Kokoro: stop Kindle, `reg load` the hive, set
`Software\Microsoft\Speech\Voices\DefaultTokenId` to the `KokoroTTS` token,
`reg unload`.

**4. The x86→x64 escape hatch (the core).** The 32-bit DLL can't touch x64
DirectML and is capped near 2 GB, so it **refuses to synthesize** and delegates:
- First `Speak`, `EnsureSynth()` (`KokoroTTSEngine.cpp`) calls
  `WorkerClient::EnsureConnected`, which opens the pipe `\\.\pipe\KokoroSapiSynth`
  if a worker is serving it, else `CreateProcess`-es
  `worker-x64\kokoro_worker.exe --assets … --voice …` and waits up to 60 s for
  the model to load.
- The worker creates the pipe with `FILE_FLAG_FIRST_PIPE_INSTANCE` — a
  **single-instance lock**. A second worker exits and the engine connects to
  the running one, so **Kindle and the reader app share one warm GPU model**.
  The worker stays warm and idle-exits after 5 min; a broken pipe between
  utterances triggers one respawn+retry.
- Wire protocol (`WorkerProtocol.h`): byte `'S'` + `[speed][voice][text]` →
  `[u32 nSamples][float32 samples…]` at 24 kHz mono; `'I'` returns
  `{"provider":…,"voice":…}`.
- If the worker can't start at all, the engine falls back to **in-process x86
  CPU** synthesis (same `KokoroSynth`, linked into the DLL).

**5. Streaming, abort, live rate/voice.** `Speak` declares 24 kHz/16-bit/mono
(SAPI inserts a resampler if needed), synthesizes sentence by sentence (audio
starts after sentence 1), maps SAPI rate `-10..10` → speed `1/3×..3×` (log),
re-reads rate/volume mid-utterance, and streams PCM to the host in ~250 ms
blocks while checking `SPVES_ABORT` between blocks (instant stop / page-turn).
It re-reads `models/default_voice.txt` **every utterance**, so changing the
narrator in the reader app changes Kindle's voice on the next sentence.

**Bonus — delay-loaded onnxruntime.** Loaded inside Kindle's directory (no
`onnxruntime.dll` there, and Windows ships an old ORT 1.17 in System32), the
engine **delay-loads** `onnxruntime.dll` and a hook in `KokoroSynth.cpp` pins
resolution to the engine DLL's own folder. The worker sidesteps this entirely:
the build copies `onnxruntime.dll` + `DirectML.dll` next to `kokoro_worker.exe`
in `build\worker-x64\`.

> One-line version: Kindle loads us as a 32-bit COM voice through the registry,
> but the neural synthesis is pushed out to a shared, single-instance 64-bit GPU
> worker over a named pipe.

### Operational notes

- Don't move/delete `kokoro-sapi/` or rebuild while Kindle holds the DLL — the
  voice token references those paths; relocating means re-running `regsvr32` and
  re-pointing `AssetDir`.
- `kokoro-sapi/kindle-voice-guard.ps1` re-applies the hive patch if a Kindle
  update resets `DefaultTokenId`.

## Performance (i7-8750H + GTX 1060)

| Backend | Realtime factor |
|---|---|
| x86 CPU in-proc (uint8) | 1.5× |
| x64 CPU (uint8) | 3.1× |
| x64 DirectML (fp32 patched) | 4.1× |
