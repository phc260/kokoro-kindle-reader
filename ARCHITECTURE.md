# Architecture

How kokoro-kindle-reader works under the hood, and how to build it from source. For
installation and day-to-day use, see the [README](README.md).

Local, offline text-to-speech built on [Kokoro-82M](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX),
with **one** synthesis engine — [`kokoro-js`](https://www.npmjs.com/package/kokoro-js)
on **WebGPU**, in a Tauri webview — serving two front ends:

1. **A desktop control panel** (Tauri 2 + React) — pick a narrator, tune speed and
   volume, and audition it with a Preview button (there's no free-text reading box;
   the app's job is choosing/hosting the voice, not reading pasted text). A
   Microsoft/Kokoro toggle also switches Kindle's default voice from inside the app.
2. **A SAPI5 voice for Windows** — "Kokoro (SAPI5)" appears in the system voice
   list, so apps like **Kindle for PC Read Aloud** narrate books with Kokoro. A
   thin **x86** COM DLL that Kindle loads in-process forwards each utterance over
   a named pipe to the running app, which synthesizes on the GPU and returns
   audio.

```
Kindle.exe (x86, MSIX)                         kokoro-kindle-reader app (Tauri 2, x64)
  │ classic SAPI5 (ISpVoice)                      React UI ──┐
  ▼  loads in-process via COM                                │ kokoro-js on WebGPU
KokoroSapi.dll  (x86 SAPI shim, connect-only)     webview ◀──┘   (the one engine)
  │  named pipe \\.\pipe\KokoroSapiSynth             ▲ synth-request / synth_result
  └──────────────────────────────────────────▶ Rust pipe_server.rs
                                                    └ also: download + serve model
                                                      (app-data dir, kokoro:// scheme)
```

The app's webview is the **only** place audio is produced. The Rust backend
downloads the model on first run, serves it to the webview, and hosts the named
pipe that bridges Kindle's SAPI engine to that webview. **The app must be running
for Kindle to speak.**

## How Kindle reads with Kokoro (the engine chain)

The trick is letting a 32-bit app drive GPU TTS that lives in a *different*,
64-bit process. It does **not** connect to anything in the networking sense —
COM loads our DLL straight into Kindle and calls its functions:

1. **SAPI5 is a registry-discovered COM plugin.** `DllRegisterServer` (`Dll.cpp`)
   writes `CLSID\{guid}\InprocServer32` → the DLL's path, and a voice token
   `…\Speech\Voices\Tokens\KokoroTTS` whose `CLSID` points back at that GUID. The
   32-bit `regsvr32` lands these in `WOW6432Node`, the view 32-bit Kindle reads.
2. **Kindle loads the DLL in-process.** It resolves its default voice token →
   CLSID → `CoCreateInstance(CLSCTX_INPROC_SERVER)` → COM `LoadLibrary`s
   `KokoroSapi.dll` into *Kindle's* address space and calls `ISpTTSEngine::Speak`.
   This is why the engine **must be x86** (matching Kindle) and a native COM DLL —
   a webview/JS thing can't be loaded this way, and it can't be merged into the
   x64 app.
3. **The DLL is a thin shim → the app.** `Speak` sends the *whole* utterance over
   the pipe `\\.\pipe\KokoroSapiSynth` (`WorkerProtocol.h`) in one `'S'` request
   (`[rate][textBytes][text]`) and gets back a **stream** of PCM frames
   (`[nSamples][gain][f32…]`, ended by a `kStreamEnd`/`kSynthError` marker).
   `pipe_server.rs` owns the chunking: it splits the text, renders each chunk in
   the webview (kokoro-js on **WebGPU**) and streams the PCM back; the engine just
   writes each frame to Kindle's audio site.
4. **Default-voice selection (MSIX).** Kindle plays whichever token equals
   `DefaultTokenId` — and because it's sandboxed, that value lives in its
   **private hive** (`…\Packages\AMZNKindle…\SystemAppData\Helium\User.dat`), not
   real HKCU. Point it at Kokoro: stop Kindle, `reg load` the hive, set
   `Software\Microsoft\Speech\Voices\DefaultTokenId` to the `KokoroTTS` token,
   `reg unload`. `kindle-voice-guard.ps1 -Set kokoro|david` automates this; the
   installer runs it at install time, and the app's **Microsoft/Kokoro toggle**
   re-runs it elevated (UAC) on demand. The chosen voice is recorded in the
   webview's `localStorage` (`kindle-agency`) so the toggle initializes to the
   last-set state.

**Streaming.** `pipe_server.rs` synthesizes **sentence by sentence** — a small
first chunk (fast first sound) then N-sentence chunks (user-tunable via the
`tts-chunk` setting) — with a **depth-1 prefetch pipeline**: chunk N+1 synthesizes
while chunk N streams back, bounded by pipe backpressure. The engine writes each
frame to the host in ~250 ms blocks, so there's no gap at chunk boundaries and
`SPVES_ABORT` stops playback promptly (it closes the pipe, which cancels the rest
of the stream). (Gaps *between Kindle pages* are Kindle's own page-turn time —
each page is a fresh `Speak` whose text we can't see in advance.)

**Volume responsiveness (Kindle path).** Gain/volume is baked into the int16
samples that sit in Kindle's audio buffer ahead of the speaker, so a naïve
implementation lags a slider move by a whole buffered chunk. `pipe_server.rs`
counters this by **pacing** its sends to ~real time (keeping at most
`tts-lead` ms of audio queued ahead) and **sub-framing** each chunk
(`tts-subframe` ms), re-reading the current gain per sub-frame. Both knobs are
exposed as frontend sliders for tuning across machines: lower `tts-lead` = snappier
volume but riskier underruns; smaller `tts-subframe` = finer gain re-read at the
cost of more pipe round-trips. See the README's user-facing notes for the
practical tuning advice.

## Layout

| Path | What |
|---|---|
| `src/` | React frontend; `tts.worker.ts` (kokoro-js Web Worker), `tts.ts` (client; `synthesize` / `synthesizeRaw`), `bridge.ts` (SAPI bridge listener; reads narrator/speed/gain from `localStorage`), `voices.ts` |
| `src-tauri/src/lib.rs` | Model download/verify + `kokoro://` asset server + `set_kindle_voice` (UAC voice-guard) |
| `src-tauri/src/pipe_server.rs` | Named-pipe server bridging the SAPI engine to webview synthesis; owns text chunking + the prefetch pipeline |
| `src-tauri/model-manifest.json` | Files the app downloads from HF (paths + sizes + SHA-256); kept in sync with `src/voices.ts` |
| `kokoro-sapi/src/` | The x86 SAPI engine: `Dll.cpp`, `KokoroTTSEngine.cpp`, `WorkerClient.cpp`, `WorkerProtocol.h` (thin COM shim + pipe client, no deps) |
| `kokoro-sapi/build.ps1` | Builds the x86 engine (NMake via vcvarsall) |
| `kokoro-sapi/*.ps1` | `test-speak.ps1` (SAPI smoke test), `kindle-voice-guard.ps1` (hive patch), `switch-voice.ps1` |

## Building from source

Prerequisites: [bun](https://bun.sh), Rust, and (for the SAPI voice) Visual
Studio with the **x86** MSVC toolchain + CMake.

```powershell
# Reader app
bun install
bun run tauri dev        # also serves the SAPI pipe while running

# SAPI engine (x86) — thin shim, no third-party deps
.\kokoro-sapi\build.ps1

# Register the voice (ELEVATED prompt; the 32-bit regsvr32 is the one that matters)
C:\Windows\SysWOW64\regsvr32.exe "kokoro-sapi\build\KokoroSapi.dll"
```

The TTS model (~430 MB: `onnx/model.onnx` for WebGPU, `onnx/model_quantized.onnx`
for the wasm fallback, voices, config/tokenizer) is **downloaded by the app** on
first run into its app-data dir — there's a setup wizard; no manual asset step.

To build the packaged installer (runs `build.ps1` so the DLL exists, then
`tauri build`, which bundles + registers the DLL via the NSIS hook):

```powershell
.\kokoro-sapi\build.ps1; bun run tauri build
```

CI does this automatically on a `v*` tag (`.github/workflows/installer.yml`).

## Kindle for PC notes (technical)

- Kindle is **32-bit MSIX**; the engine must be x86, registered under
  `WOW6432Node` (the 32-bit `regsvr32` does this), and its default voice patched
  in the package hive (above). The installer patches it to Kokoro at install time
  and reverts it to Microsoft David on uninstall (so Kindle isn't left pointing at
  a removed token); the app's Microsoft/Kokoro toggle re-runs
  `kindle-voice-guard.ps1` on demand; re-run it manually if a Kindle update resets
  the voice. Reopen Kindle after a switch for it to take effect.
- **The app must be running** when Kindle reads — it's the synthesizer. If it
  isn't, the voice is silent (the shim has no local fallback by design).
- Don't move/delete `kokoro-sapi/` — the registered token references the DLL by
  path.
