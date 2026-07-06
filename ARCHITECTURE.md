# Architecture

How kokoro-kindle-reader works under the hood, and how to build it from source. For
installation and day-to-day use, see the [README](README.md).

Local, offline text-to-speech built on [Kokoro-82M](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX),
synthesized **natively on the Dawn WebGPU** execution provider of ONNX Runtime (the
same Dawn that Chrome/WebView2 ship) — **no browser, no WebView2, no C++**. The synth
core is pure Rust: the `ort` crate drives the model on the WebGPU EP and espeak-ng is
reached over a thin FFI. It serves two front ends:

1. **A native settings panel** (Slint) — pick a narrator, tune speed and volume, and
   audition it with a Preview button (there's no free-text reading box; the app's job
   is choosing/hosting the voice, not reading pasted text). A checkbox also switches
   Kindle's default voice between Kokoro and Microsoft David. The panel is spawned on
   demand from the tray; nothing UI is resident at idle.
2. **A SAPI5 voice for Windows** — "Kokoro (SAPI5)" appears in the system voice list,
   so apps like **Kindle for PC Read Aloud** narrate books with Kokoro. A thin **x86**
   COM DLL that Kindle loads in-process forwards each utterance over a named pipe to
   the running host, which synthesizes on the GPU and returns audio.

```mermaid
flowchart TB
  subgraph K["🪟 Kindle for PC · 32-bit (x86, MSIX)"]
    direction LR
    RA["Read Aloud<br/>SAPI5 · ISpVoice"]
    DLL["KokoroSapi.dll<br/>x86 COM shim · connect-only"]
    RA -->|"in-process COM<br/>ISpTTSEngine::Speak"| DLL
  end

  subgraph H["⚙️ kokoro-host.exe · 64-bit, windowless tray"]
    direction LR
    PIPE["pipe.rs<br/>named-pipe server · owns chunking"]
    SYNTH["native_synth.rs · Rust<br/>★ text + espeak + ort on Dawn WebGPU EP"]
    PIPE -->|"synth each chunk"| SYNTH
    SYNTH -->|"raw f32 PCM"| PIPE
  end

  PANEL["kokoro-panel.exe · Slint (spawned on demand)<br/>narrator/speed/gain/chunk → controls.json<br/>model download/verify · Kindle-voice toggle · Preview"]
  CFG[("controls.json<br/>%APPDATA%\\…")]
  PANEL -->|writes| CFG
  CFG -->|"read live per utterance / sub-frame"| PIPE

  DLL <==>|"named pipe · KokoroSapiSynth<br/>'S' utterance → PCM frames"| PIPE

  classDef engine fill:#ff4fa3,stroke:#b30059,color:#ffffff;
  classDef shim fill:#1f6feb,stroke:#0b3d91,color:#ffffff;
  class SYNTH engine
  class DLL,PIPE shim
```

`kokoro-host` is the only place audio is produced. It runs windowless in the system
tray, hosts the named pipe that bridges Kindle's SAPI engine to the native synth, and
reads its settings live from `controls.json`. **The host must be running for Kindle to
speak.**

## How Kindle reads with Kokoro (the engine chain)

The trick is letting a 32-bit app drive GPU TTS that lives in a *different*, 64-bit
process. It does **not** connect to anything in the networking sense — COM loads our
DLL straight into Kindle and calls its functions:

1. **SAPI5 is a registry-discovered COM plugin.** `DllRegisterServer` (`Dll.cpp`)
   writes `CLSID\{guid}\InprocServer32` → the DLL's path, and a voice token
   `…\Speech\Voices\Tokens\KokoroTTS` whose `CLSID` points back at that GUID. The
   32-bit `regsvr32` lands these in `WOW6432Node`, the view 32-bit Kindle reads.
2. **Kindle loads the DLL in-process.** It resolves its default voice token → CLSID →
   `CoCreateInstance(CLSCTX_INPROC_SERVER)` → COM `LoadLibrary`s `KokoroSapi.dll` into
   *Kindle's* address space and calls `ISpTTSEngine::Speak`. This is why the engine
   **must be x86** (matching Kindle) and a native COM DLL — and why it stays a separate
   file from the x64 host.
3. **The DLL is a thin shim → the host.** `Speak` sends the *whole* utterance over the
   pipe `\\.\pipe\KokoroSapiSynth` (wire format: the `kokoro-protocol` crate) in one `'S'` request
   (`[rate][textBytes][text]`) and gets back a **stream** of PCM frames
   (`[nSamples][gain][f32…]`, ended by a `kStreamEnd` / `kSynthError` marker — the u32
   sentinels `0xFFFF_FFFE` / `0xFFFF_FFFF`). `kokoro-host`'s `pipe.rs` owns the
   chunking: it splits the text, renders each chunk on the native Dawn WebGPU synth,
   and streams the PCM back; the engine just writes each frame to Kindle's audio site.
4. **Default-voice selection (MSIX).** Kindle plays whichever token equals
   `DefaultTokenId` — and because it's sandboxed, that value lives in its **private
   hive** (`…\Packages\AMZNKindle…\SystemAppData\Helium\User.dat`), not real HKCU.
   Point it at Kokoro: stop Kindle, `reg load` the hive, set
   `Software\Microsoft\Speech\Voices\DefaultTokenId` to the `KokoroTTS` token, `reg
   unload`. `kindle-voice-guard.ps1 -Set kokoro|david` automates this; the installer
   runs it at install time, and the panel's **Kindle-voice checkbox** re-runs it
   elevated (UAC) on demand. The chosen state is recorded in `controls.json`
   (`kindle_kokoro`) so the checkbox initializes to the last-set state.

**Native synthesis.** `native_synth.rs` is the pure-Rust synth core: `text.rs`
normalize/segment → `espeak.rs` phonemize (espeak-ng FFI) → tokenize → the Kokoro ONNX
model on the ORT **Dawn WebGPU** EP via the `ort` crate (load-dynamic against the staged
`onnxruntime.dll`) → f32 PCM. espeak keeps global state and isn't thread-safe (and the
`ort` session lives here), so all synthesis is **serialized onto one dedicated worker
thread** that owns the session for the process lifetime; requests arrive over an mpsc
channel and replies come back on tokio oneshots so the async pipe tasks never block.

**Streaming.** `pipe.rs` synthesizes **sentence by sentence** — a small first chunk
(fast first sound) then N-sentence chunks (user-tunable via the `chunk` setting) —
with a **depth-1 prefetch pipeline**: chunk N+1 synthesizes while chunk N streams back,
bounded by pipe backpressure. The engine writes each frame to the host in ~250 ms
blocks, so there's no gap at chunk boundaries and `SPVES_ABORT` stops playback promptly
(it closes the pipe, which cancels the rest of the stream). (Gaps *between Kindle
pages* are Kindle's own page-turn time — each page is a fresh `Speak`.)

**Volume responsiveness (Kindle path).** Gain/volume is baked into the int16 samples
that sit in Kindle's audio buffer ahead of the speaker, so a naïve implementation lags
a slider move by a whole buffered chunk. `pipe.rs` counters this by **pacing** its
sends to ~real time (keeping at most a fixed lead of audio queued ahead) and
**sub-framing** each chunk, re-reading the current gain from `controls.json` per
sub-frame. The lead (500 ms) and sub-frame (250 ms) are fixed built-in defaults
(`DEFAULT_LEAD_MS` / `DEFAULT_SUBFRAME_MS` in `pipe.rs`); only the narrator, speed,
gain, and per-chunk sentence count are user-facing.

## Layout

| Path | What |
|---|---|
| `kokoro-host/` | The windowless tray host (x64): `main.rs` (tao event loop + tray + `auto-launch`), `pipe.rs` (named-pipe server; owns chunking + prefetch + pacing), `native_synth.rs` (serialized Rust WebGPU synth + `controls.json` reader) + `text.rs`/`espeak.rs` (kokoro-js text normalizer + espeak-ng FFI), `split_text.rs` (the sentence-chunk splitter). `build.rs` links the espeak-ng import lib and stages the runtime DLLs + `espeak-ng-data`. |
| `kokoro-panel/` | The native settings panel (Slint/Fluent): `ui/panel.slint` + `src/main.rs`, and the framework-agnostic `download.rs` / `kindle.rs` / `preview.rs`. Writes `controls.json`. |
| `kokoro-worker/` | Synth **dependency provisioning** only (no source): `tools/fetch-deps.ps1` populates `third_party/` — the Dawn/WebGPU runtime DLLs (from the `onnxruntime-webgpu` wheel) + espeak-ng (x64 build + import lib + `espeak-ng-data`). |
| `kokoro-sapi-rs/` | The x86 SAPI engine — a Rust `cdylib` (thin COM shim + pipe client, no deps): `lib.rs` (COM exports + registration), `engine.rs` (`ISpTTSEngine`), `worker.rs` (pipe client), `sapi.rs` (hand-declared `sapiddk.h` interfaces). Plus the `voice-setup.ps1` / `kindle-voice-guard.ps1` (Kindle hive patch) / `test-speak.ps1` scripts. |
| `kokoro-sapi-smoke/` | No-Kindle COM + Speak smoke test for the engine (`run-speak-test.ps1`). |
| `kokoro-protocol/` | The named-pipe wire constants (pipe name, `'S'`/`'I'`, `STREAM_END`/`SYNTH_ERROR`, sample rate) as a small crate shared by **both** `kokoro-host` and `kokoro-sapi-rs` — the single source of truth for the format. |
| `model-manifest.json` | Files the model downloads from HF (paths + sizes + SHA-256); embedded in `kokoro-panel` (the narrator list is derived from it). |
| `icons/` | Shared app icons (LFS); embedded in the exes' version resource and the installer. |
| `packaging/` | `installer.nsi` + `build-installer.ps1` (standalone NSIS build). |

## Building from source

Prerequisites: Rust (x64 + the `i686-pc-windows-msvc` target for the SAPI DLL), Visual
Studio with the MSVC toolchain + CMake, Python (for the onnxruntime-webgpu wheel), and
[NSIS](https://nsis.sourceforge.io/) (for the installer).

```powershell
# 1. One-time: provision the synth runtime deps
#    (Dawn runtime DLLs + espeak-ng x64 import lib/DLL + espeak-ng-data)
.\kokoro-worker\tools\fetch-deps.ps1
rustup target add i686-pc-windows-msvc   # for the x86 SAPI DLL

# 2. Build + run the headless host (tray). Right-click the tray → Settings for the panel.
cargo run --manifest-path kokoro-host\Cargo.toml
cargo run --manifest-path kokoro-panel\Cargo.toml   # or launched from the tray

# 3. The x86 SAPI engine (Rust cdylib, no third-party deps) — for a real Kindle test
cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-sapi-rs\Cargo.toml

# Register the voice (ELEVATED; the 32-bit regsvr32 is the one that matters)
C:\Windows\SysWOW64\regsvr32.exe "kokoro-sapi-rs\target\i686-pc-windows-msvc\release\KokoroSapi.dll"
```

The TTS model (~430 MB: `onnx/model.onnx`, voices, config/tokenizer) is **downloaded by
the panel** on first run into the app-data dir — no manual asset step.

To build the packaged installer (release-builds both crates, stages everything, and
runs `makensis`):

```powershell
.\packaging\build-installer.ps1
```

CI does this on a `headless-v*` tag (`.github/workflows/headless-installer.yml`); the
`sapi-rs.yml` workflow builds the DLL + runs the COM smoke test on engine changes.

## Kindle for PC notes (technical)

- Kindle is **32-bit MSIX**; the engine must be x86, registered under `WOW6432Node`
  (the 32-bit `regsvr32` does this), and its default voice patched in the package hive
  (above). The installer patches it to Kokoro at install time and reverts it to
  Microsoft David on uninstall (so Kindle isn't left pointing at a removed token); the
  panel's Kindle-voice checkbox re-runs `kindle-voice-guard.ps1` on demand; re-run it
  manually if a Kindle update resets the voice. Reopen Kindle after a switch.
- **The host must be running** when Kindle reads — it's the synthesizer. If it isn't,
  the voice is silent (the shim has no local fallback by design).
- Don't move/delete `kokoro-sapi-rs/` — the registered token references the DLL by path.
