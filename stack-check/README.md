# stack-check

A throwaway diagnostic GUI — **not part of the product** — to answer two questions on
whatever machine it runs on:

1. **Does the tech stack work?** espeak-ng (modified 1.52.0, via FFI) → phonemes →
   Kokoro-82M vocab tokens → **ONNX Runtime** → 24 kHz audio.
2. **How fast, CPU vs GPU?** Times full-page synthesis across execution providers and
   CPU thread counts.

It reuses the real assets the host uses — the provisioned espeak in
`../native-deps/linux/runtime/` and the Kokoro model under
`~/.local/share/kokoro-kindle-reader/…` — but the synthesis core here
(`kokoro_min.py`) is a *deliberately minimal* stand-in, not a port of `native_synth.rs`.
It keeps the model I/O and the vocab-critical phoneme substitutions; it skips number
normalization, punctuation segmentation and the word-timing machinery.

The phonemizer is the project's **modified espeak-ng 1.52.0** over a ctypes FFI — the exact
build the product ships. (The `espeak-english` pip package was considered but bundles
libespeak-ng **1.51**, which differs from 1.52 on ~7% of words, so it was rejected for
consistency with the product.)

The GUI is **Slint**, the toolkit production's settings panel uses, through Slint's Python
bindings. It's pinned to `slint==1.17.1b2`, the Python release of the Slint version
`kokoro-panel` locks (Rust 1.17.1); the "b" marks the Python bindings as beta. The window
is `ui/stack_check.slint`, loaded with the panel's **Fluent** style, and it follows the panel's
split: the `.slint` file only draws state, and `stack_check.py` sets the properties and
handles the callbacks.

---

## Developer setup (Linux)

Written against a fresh machine. It has three moving parts: **(A)** the project's native
deps (the modified espeak 1.52.0), **(B)** the Kokoro model files, and **(C)** this app's
own Python environment. `run.sh` handles (C) automatically; (A) and (B) are shared project
assets you provision once with the repo's existing scripts.

### 0. Prerequisites

| Tool | Why | Install |
|---|---|---|
| `python3` (3.9+) | drives the provisioning scripts | usually preinstalled |
| `git`, `cmake`, a C toolchain (`gcc`/`clang` + `make`) | **espeak-ng 1.52.0 is built from source** by `fetch-deps.py` (a distro's own libespeak is unmodified and changes the phonemes) | `sudo apt install git cmake build-essential` |
| an audio player: `pw-play` (PipeWire) or `aplay` (ALSA) | the GUI's Play button shells out to it | usually preinstalled; else `sudo apt install pipewire-bin` or `alsa-utils` |
| `curl` | to fetch the uv installer | usually preinstalled |
| *(optional, for WebGPU)* the Vulkan loader + a Vulkan driver | the WebGPU EP runs on Dawn, which uses **Vulkan** on Linux; without it only CPU works | `sudo apt install libvulkan1 mesa-vulkan-drivers` (Intel/AMD); check with `ls /usr/share/vulkan/icd.d/` |

Note: the app itself needs **no** `sudo` and **no** system Python packages — uv provides a
self-contained CPython. `sudo` above is only for the one-time build toolchain and (maybe) an
audio player. Run the repo's toolchain doctor to see what's missing in one pass:

```bash
./packaging/doctor.sh          # from the repo root; checks python3/cmake/C-toolchain/etc.
```

### A. Provision the modified espeak-ng 1.52.0

This app loads `../native-deps/linux/runtime/libespeak-ng.so.1.52.0` + its
`espeak-ng-data/` directly. Build them from source (needs cmake + C toolchain from step 0):

```bash
python3 native-deps/fetch-deps.py     # from the repo root
```

This populates `native-deps/linux/runtime/` (it also fetches the CPU onnxruntime `.so`,
which this app doesn't use — the `onnxruntime-webgpu` wheel provides ONNX Runtime for the
Python side — but it's harmless and part of the same recipe).

### B. Provision the Kokoro model + voice

The app reads the model from the host's app-data dir. Fetch it with the repo's script
(verifies SHA-256 against `model-manifest.json`):

```bash
python3 native-deps/fetch-model.py            # -> ~/.local/share/kokoro-kindle-reader/onnx-community/Kokoro-82M-v1.0-ONNX/
python3 native-deps/fetch-model.py --verify-only   # re-check an existing copy
python3 native-deps/fetch-model.py --dest DIR      # provision somewhere else
```

You need `config.json`, `tokenizer.json`, `onnx/model.onnx`, and `voices/af_heart.bin`.

### C. Set up this app's environment (uv)

The dependencies (`onnxruntime-webgpu==1.27.0`, `numpy`, `onnx`), the Python version
(`.python-version`), and the managed-CPython / copy-link-mode settings are all declared in
**`pyproject.toml`** and locked in **`uv.lock`** — so `uv run` reproduces the exact
environment. `onnxruntime-webgpu` is the same `onnxruntime` Python module plus the WebGPU
EP (and the CPU EP), pinned to the version the project's Windows build uses; `numpy` is
imported directly and is also required by ONNX Runtime's Python API; `onnx` is used only to
patch the graph for the **Wrap sine phase** toggle (see below). Install **uv** if you
don't have it, then run:

```bash
curl -LsSf https://astral.sh/uv/install.sh | sh
source "$HOME/.local/bin/env"      # or restart your shell; puts uv on PATH
```

```bash
cd stack-check
./run.sh
```

`run.sh` is a one-liner around `uv run stack_check.py`. On first use, `uv run` fetches a
managed **CPython 3.12**, creates the local venv **`stack-check/.venv`**, and syncs the locked
dependencies into it. Subsequent launches reuse it. Why these settings live in
`pyproject.toml` rather than flags: the system Python here ships without pip/ensurepip, and a
uv-managed interpreter is the same everywhere (`python-preference = "only-managed"`). The
venv sits on the project's fuse mount, a different device from uv's wheel cache, so wheels are
copied not hardlinked (`link-mode = "copy"`).

#### Without run.sh

```bash
cd stack-check
uv sync                    # create .venv + install locked deps (optional; uv run does it too)
uv run stack_check.py      # the GUI
```

### Headless check (no GUI)

Sanity-check the whole pipeline from a terminal — same core the GUI drives:

```bash
uv run kokoro_min.py "Some text to speak." /tmp/out.wav
```

It prints the espeak lib + version, the phonemes, token/timing stats, and writes a WAV.

---

## GUI notes (Slint from Python)

- **Fonts:** Slint renders its own anti-aliased text with the system fonts, so IPA phonemes
  (ð, ɹ, ˈ, ɪ …) display correctly with no font setup. The earlier tkinter version couldn't
  do either: the uv-managed Tk has no Xft.
- **Threading:** Slint objects may only be touched on the UI thread, and the Python bindings
  have no `invoke_from_event_loop`. Worker threads post closures to a queue, and a repeating
  `slint.Timer` drains it on the UI thread.
- **Garbage collection is confined to the UI thread.** Slint's struct values are pyo3
  `unsendable` objects. Python's cyclic GC runs on whichever thread happens to allocate, which
  here means the synthesis workers, and if it clears a Slint struct there, pyo3 panics and the
  whole process aborts. That happened on the first run. So automatic GC is off and
  a timer runs `gc.collect()` on the UI thread every 5 s; reference counting still frees
  everything acyclic immediately.
- **No file dialog in Slint:** Save WAV writes to the path in the text field next to it.
- **Theme:** Fluent follows the system light/dark setting through `Palette`, as the panel does.

## The "Wrap sine phase" toggle

A checkbox above the tabs, off by default, that applies to both Synthesize and Speed test.
When it's on, the model is patched in memory: four nodes (`Mul → Floor → Mul → Sub`) wrap the
harmonic source's phase into [0, 2π) before `…/m_source/l_sin_gen/Sin` (`wrap_sine_phase()`
in `kokoro_min.py`). That's the fix for the WebGPU muffling described under findings below.
Results carry a `+wrap` suffix (e.g. `WebGPU +wrap`), so wrapped and unwrapped runs can't be
confused.

- The first wrapped run reloads and re-serializes the 325 MB model, which takes a few
  seconds; the patched model is then cached for the rest of the session.
- Switching the toggle drops the sessions built for the other state, because each session
  holds a full copy of the weights.
- Measured on a 5.9 s sentence: `WebGPU +wrap` matches CPU (corr 0.997, same harmonic clarity
  and loudness) with no speed cost, and `CPU +wrap` is essentially unchanged (corr 0.9994).
- It's off by default so the raw WebGPU behaviour stays reproducible. Leave it off to hear the
  bug, turn it on to hear the fix.

## Checking another machine: `webgpu_sin_check.py`

A standalone, single-file version of the WebGPU `Sin` investigation, for machines where the
GUI isn't set up. It's meant above all for the **Windows** dev box, where production runs
WebGPU via Dawn → D3D12. Nothing else is needed: no espeak (the phonemes are embedded), no
host, no app venv. Its dependencies are declared inline (PEP 723), so `uv run` fetches them:

```bash
uv run stack-check/webgpu_sin_check.py          # from the repo root (Windows: stack-check\webgpu_sin_check.py)
uv run stack-check/webgpu_sin_check.py --sin-only           # Part 1 only, no model needed
uv run stack-check/webgpu_sin_check.py --model DIR          # model somewhere other than the host's app-data dir
```

Without uv: make a venv, `pip install onnxruntime-webgpu==1.27.0 numpy onnx`, then run it
with that venv's Python.

- **Part 1** measures WebGPU vs CPU `Sin` error by argument size and bisects the breakdown
  point.
- **Part 2** runs the real model (default: the host's app-data dir — on Windows
  `%APPDATA%\com.phc260.kokoro-kindle-reader\onnx-community\Kokoro-82M-v1.0-ONNX`) on a
  12.7 s passage. It reports where the harmonic-source phase crosses that point, correlation
  and harmonic clarity for CPU / WebGPU / WebGPU +wrap / CPU +wrap, and writes
  `cpu.wav`, `webgpu.wav` and `webgpu_wrap.wav` to `<temp>/webgpu_sin_check/`.
- It ends with a one-line **verdict**: AFFECTED or NOT AFFECTED.

Production's Windows runtime is provisioned from the same `onnxruntime_webgpu-1.27.0` wheel,
so the ONNX Runtime and Dawn code under test is the code that ships. What differs per machine
is the GPU/driver `sin`, and that's what this measures. Reference result on the Linux Iris Xe:
**AFFECTED**, breakdown at 102,839, crossing 4.3 s into the passage, and WebGPU vs CPU
0.629 → 0.997 with the wrap.

## The three tabs

- **Environment** — Python/slint/numpy/onnxruntime versions, the available execution
  providers, detected CPU & GPU, espeak-ng, model and voice files, and an audio player.
- **Synthesize** — type text, pick a provider, set speed, hear it; shows the IPA phonemes,
  token count, audio length, synth time and real-time factor (RTF).
- **Speed test** — benchmarks the chosen configurations (each available EP, plus CPU at
  1/2/4/8 intra-op threads), reporting median / p90 / min ms, RTF and relative speed.

## What it found on the reference laptop (i7-1185G7, Iris Xe, Linux)

- **WebGPU works on Linux — but only with the right wheel.** The plain `onnxruntime` wheel
  exposes only `CPUExecutionProvider` (+ Azure). **`onnxruntime-webgpu` 1.27.0** (the same
  version the Windows build pins) exposes `WebGpuExecutionProvider` and runs the Kokoro model
  on the Iris Xe via Dawn → Vulkan → Mesa ANV.
- **WebGPU audio is muffled, and the cause is the `Sin` op's input range.** On this GPU,
  WebGPU's `Sin` is accurate up to |x| ≈ 102,839 (≈ 32768·π) and returns garbage above it;
  CPU's `Sin` is accurate at any size. Kokoro's harmonic source
  (`decoder/generator/m_source/l_sin_gen`) feeds `Sin` an *unwrapped* cumulative phase that
  crosses that line 3.6–5.5 s into each model run (≈2×10⁵ rad by 5.9 s). So any utterance
  longer than ~3.6 s loses the harmonic excitation in its tail: breathy/muffled, ≈12%
  quieter, cepstral peak prominence −2 dB, correlation with CPU 0.29 after the crossing.
  Shorter utterances are nearly unaffected. **Wrapping the phase into [0, 2π) before `Sin`**
  (Mul → Floor → Mul → Sub, 4 nodes) makes WebGPU match CPU (corr 0.997; identical CPP and
  RMS) and leaves CPU unchanged (corr 0.9995). The app applies it behind the **Wrap sine
  phase** toggle (see above).
- **On this iGPU, WebGPU is *slower* than CPU:** ≈1.35× slower on a 90-token sentence
  (2.1× vs 2.9× real-time), ≈2.2× slower on a 50-token one, warmed up. A direct, single-EP
  confirmation of the `CLAUDE.md` finding that the integrated GPU loses to the CPU on this
  class of machine.
- **CPU thread scaling confirms the documented invariant:** synthesis is fastest at the
  physical-core count (x4) and gets *slower* past it (x8 hyperthreads add contention).
- End to end runs comfortably faster than real time on CPU (RTF ≈ 3x for a page of prose).

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `FileNotFoundError: libespeak-ng not found under …/native-deps/linux/runtime` | Step A not done — run `python3 native-deps/fetch-deps.py`. |
| `fetch-deps.py` fails on cmake / a compiler | Missing build toolchain — `sudo apt install git cmake build-essential`, or see `./packaging/doctor.sh`. |
| model / voice errors, or `model.onnx` missing | Step B not done — run `python3 native-deps/fetch-model.py`. |
| `uv: command not found` after install | uv is at `~/.local/bin`; `source "$HOME/.local/bin/env"` or add it to PATH. |
| Environment tab shows only `CPUExecutionProvider` (+Azure) | The venv has plain `onnxruntime` instead of `onnxruntime-webgpu` — run `uv sync` in `stack-check/`. |
| WebGPU listed, but its session fails or Synthesize errors on WebGPU | No usable Vulkan driver — install `libvulkan1 mesa-vulkan-drivers`; check `/usr/share/vulkan/icd.d/`. CPU still works. |
| `uv sync` times out downloading `onnxruntime-webgpu` (24 MiB) | Slow link to `files.pythonhosted.org` — retry with `UV_HTTP_TIMEOUT=900 uv sync`. |
| Dawn prints `maxDynamic…BuffersPerPipelineLayout artificially reduced` | Harmless Dawn warning on startup. |
| WebGPU speech sounds muffled/breathy toward the end of longer sentences | The known WebGPU `Sin` range bug — tick **Wrap sine phase**. |
| The **Wrap sine phase** checkbox is greyed out | `onnx` isn't installed in the venv (the Environment tab says so) — run `uv sync`. |
| No sound on Play | No `pw-play`/`aplay` on PATH — install one; test with `pw-play /tmp/out.wav`. |
| Process aborts with `PyStruct is unsendable, but sent to another thread` | Something re-enabled automatic GC, or a worker touched a Slint object. See GUI notes above. |
| The window doesn't appear | No display — set `DISPLAY` (e.g. `DISPLAY=:0 ./run.sh`) on a machine with an X session. |

## Paths & layout reference

| What | Location | Provisioned by |
|---|---|---|
| App source | `stack-check/` | (this repo) |
| Python venv (packages) | `stack-check/.venv/` (gitignored) | `uv run` / `uv sync` |
| uv-managed CPython 3.12 | `~/.local/share/uv/python/` | `uv python install` |
| Modified espeak-ng 1.52.0 lib + data | `native-deps/linux/runtime/` | `native-deps/fetch-deps.py` |
| Kokoro model + `af_heart` voice | `~/.local/share/kokoro-kindle-reader/onnx-community/Kokoro-82M-v1.0-ONNX/` | `native-deps/fetch-model.py` |

Defaults are set in `kokoro_min.py` (`DEFAULT_MODEL_DIR`, `DEFAULT_ESPEAK_RUNTIME`); the
repo root is inferred as the parent of `stack-check/`.

## Files

- `kokoro_min.py` — synthesis core (espeak FFI, tokenizer, ONNX session, WAV) + headless CLI.
- `stack_check.py` — the GUI's logic (Slint; workers, UI-thread pump, playback).
- `ui/stack_check.slint` — the window itself.
- `webgpu_sin_check.py` — standalone WebGPU `Sin`-range check for other machines (Windows).
- `pyproject.toml` — project metadata + dependencies + uv settings (managed python, copy link-mode).
- `uv.lock` — pinned dependency versions (committed, for reproducible installs).
- `.python-version` — pins the interpreter to CPython 3.12.
- `run.sh` — installs uv if needed, then `uv run stack_check.py`.
