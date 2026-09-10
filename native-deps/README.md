# native-deps — synth dependency provisioning

**Not a crate** — just the scripts that provision the native runtime the synth needs.
Populates the gitignored dep folders alongside them (`runtime/` or `linux/runtime/`, plus
`espeak-ng-src/`; re-created by the scripts).

## One recipe, two harnesses

`fetch-deps.py` and `build-espeak.py` are the recipe, shared by both platforms.
`fetch-deps.ps1` and `build-espeak.ps1` are harnesses over it, so the entry points and flags
Windows callers already use keep working — and because they earn their keep: they resolve
Python by *running* `py`/`python`/`python3`, since a Windows App Execution Alias is a 0-byte
reparse point that works fine, and they give an install hint when there is none. Linux calls
`fetch-deps.py` directly; python3 is guaranteed there, so a shell wrapper would only be a
second name for the same call.

That is a deliberate reversal. These were once a PowerShell script and a bash twin that had
to be kept pin-for-pin identical **by hand** — an invariant that existed only because the
recipe was duplicated, and whose failure mode was the worst kind: a phoneme or pin
difference raises no error, it just makes one platform quietly build something else. They
had already drifted by the time they were merged (the bash side accepted the first `LICENSE`
found anywhere in the wheel, exactly the lax check the PowerShell side's comments warned
against). Platform differences now live in the `WHEELS` table and `layout()`, where they can
be read side by side.

**Python 3 is therefore a prerequisite on both platforms.** The recipe uses only the standard
library. The `.ps1` harnesses resolve it by *running* `py`/`python`/`python3` and matching
`Python 3` in the output — not by looking at the file, because a Windows App Execution Alias
is a 0-byte reparse point that works perfectly when Python is installed.

**Windows** (`fetch-deps.ps1` -> `runtime/`):

- the **Dawn/WebGPU runtime DLLs** from the exact SHA-256-pinned
  `onnxruntime-webgpu` CPython 3.12 Windows wheel
  (`onnxruntime.dll` + `onnxruntime_providers_shared.dll` + `dxcompiler.dll` + `dxil.dll`)
- an **espeak-ng x64 build** (`espeak-ng.dll` + import lib + `espeak-ng-data`)

**Linux** (`fetch-deps.py` -> `linux/runtime/`):

- the **CPU ONNX Runtime** (`libonnxruntime.so*`) from the exact SHA-256-pinned
  `onnxruntime` CPython 3.12 manylinux wheel — **the CPU wheel, not the WebGPU one, and
  deliberately.** The equivalent WebGPU wheel exists, but native WebGPU is Vulkan on Linux
  and none of it has been validated: not the library's own dependencies, not the `ort`
  binding's registration, not the drivers. The CPU milestone must not be able to fail for a
  GPU reason. Evaluating that wheel is the GPU stage's job, and it is a separate pin.
- an **espeak-ng shared-library build** (`libespeak-ng.so*` + `espeak-ng-data`), from the
  same pinned commit and the same one-line modification as Windows

Both, by a second script: the **Cloud Reader OCR models** (`ocr/`).

### Verified layout facts (Linux)

Read from the pinned wheel and the pinned espeak tree, not assumed — the scripts depend on
all of these:

- the wheel carries `onnxruntime/capi/libonnxruntime.so.1.27.0` (SONAME
  `libonnxruntime.so.1`) and `onnxruntime/capi/libonnxruntime_providers_shared.so`. There is
  **no plain-name symlink**, so `fetch-deps.py` is what creates the `libonnxruntime.so` that
  `init_ort` opens by name.
- `libonnxruntime.so*` as a pattern does **not** match `libonnxruntime_providers_shared.so`.
  That shim is dlopened by ORT under its plain name, so it is named separately in both the
  provisioning script and `build.rs`. It was missing from the first draft of both.
- the wheel's `LICENSE`, `Privacy.md` and `ThirdPartyNotices.txt` sit one level down, under
  `onnxruntime/` — the same shape as the Windows wheel, so the search is recursive.
- espeak's shared library lands in `<build>/src/libespeak-ng/`, not `<build>/src/`. The
  `RUNTIME_OUTPUT_DIRECTORY ..` redirect that puts the DLL in `src/` on Windows is inside an
  `if (MINGW OR WIN32 OR MSVC)`, and a shared library is a LIBRARY target on Linux. With
  `SOVERSION 1` / `VERSION 1.52.0` the chain is
  `libespeak-ng.so -> .so.1 -> .so.1.52.0`.
- `espeak-ng-data/` is generated at the **build root**, not under `src/`.
- the ONNX Runtime needs **glibc >= 2.27** and **libstdc++6** (`GLIBCXX_3.4.21`,
  `CXXABI_1.3.11`). The `manylinux_2_28` tag on the filename is more conservative than the
  binary actually is.

`ORT-PROVISION.txt` and `ESPEAK-PROVISION.txt` (in whichever runtime tree) identify the
exact cached recipes. A missing or mismatched marker forces re-provisioning instead of
silently reusing binaries from an older version. The espeak marker includes the
newline-normalized SHA-256 of the build script that produced it, so a build-flag or
patch-recipe edit also invalidates the cache. Its
provision records a SHA-256 manifest of the source tree used for the build;
corresponding-source packaging refuses a tree that no longer
matches it.

## Run this first

```powershell
.\fetch-deps.ps1        # downloads the wheel + builds espeak; idempotent (-Force to redo)
.\fetch-ocr-models.ps1  # the 9.80 MB PP-OCR pair + dictionary; network only, no toolchain
```

Or the recipes directly, on either platform:

```bash
python3 native-deps/fetch-deps.py
python3 native-deps/fetch-ocr-models.py
```

On Linux:

```bash
python3 native-deps/fetch-deps.py            # same, for linux/runtime/
python3 native-deps/fetch-deps.py --force    # re-provision from scratch
python3 native-deps/fetch-model.py           # the voice model - see below; Linux has no panel yet
```

`kokoro-host`'s `build.rs` panics if the dep folders for the target being built are missing,
so this must run before building the host — and it branches on the **target**, so
cross-checking a Linux target needs the Linux provision, not the Windows one. It also stages
the runtime libraries next to the exe (found there at run time by an `$ORIGIN` rpath on
Linux). The ONNX model runs on the `ort` crate's execution providers via load-dynamic, so
the runtime library is loaded at run time (not linked) — no ORT headers/import lib
needed.

Requires Python 3, CMake + a C toolchain (to build espeak) and network: MSVC on Windows,
gcc/clang on Linux. `build-espeak.py` is invoked by `fetch-deps.py`; it builds espeak-ng
1.52.0 commit `4870adfa25b1a32b4361592f1be8a40337c58d6c` with the horse-hoarse phoneme revert
this model expects, pins the patched `phsource/ph_english_us` to digest `ffa5cbde...`, and
refuses to build a tree carrying any modification beyond that one.

**A distribution's own libespeak-ng is not a substitute**: it is unmodified, and probably not
1.52.0 either, and either difference changes the phonemes — audibly, and with no error.

## fetch-ocr-models.py

Separate because it needs **network and nothing else** — no compiler, no CMake, minutes
faster. (It does need Python, like every recipe here; `fetch-ocr-models.ps1` is its harness.) It is also not a build dependency: `kokoro-ocr` loads these at run time, so the host
builds without them and reports OCR `missing` until they are there.

**This is for DEV only.** The OCR models are **not bundled in the installer** — the settings
panel downloads them at first run into `%APPDATA%\...\ocr\`, exactly like the Kokoro voice model
(see [`ocr-manifest.json`](../ocr-manifest.json) and `kokoro-panel::download`). So
`build-installer.ps1` stages nothing under `ocr\` and no longer calls this script. Run it to
populate `native-deps\ocr\` so a debug `cargo run` of the host finds the models without a
download; the digests and URLs here are the same ones `ocr-manifest.json` and
`kokoro-ocr/src/lib.rs` carry (keep the three in sync).

Two things it does that are worth knowing:

- **Every URL pins a revision**, never a branch. These are weights; a branch is whatever it
  points at today, and the digest would start failing on some future commit in a way that reads
  as a corrupt download rather than as upstream moving.
- **The recognizer comes from `media.githubusercontent.com`, not `raw.`** It is stored in Git
  LFS, and `raw` hands back the 132-byte *pointer* file with a 200 — exactly the shape of
  download a size check waves through. The digests are what catch it; the pointer even carries
  the same sha256, so the check is that the file **is** those bytes rather than describes them.
  The dictionary is not an LFS file, and `media` 404s for anything that is not, so the two
  come from different hosts on purpose.

## fetch-model.py

The ~324 MiB Kokoro voice model, into the host's own app-data dir
(`<app_data>/onnx-community/Kokoro-82M-v1.0-ONNX/`) rather than anywhere under `native-deps/`
— that is where `kokoro-host` looks (`boot` in `main.rs`), and there is no dev fallback for
the model as there is for the OCR pair.

**It exists for Linux.** On Windows the settings panel downloads the model at first run and
remains the normal route. The panel does not build for Linux yet (the port plan's
desktop-integration step), so without this a freshly provisioned Ubuntu host starts, logs
`model.onnx not found` and synthesizes nothing — which makes the Linux narration milestone
unreachable on the machine it is about. It works on Windows too, and its `--verify-only` is
the panel's **Verify & repair** without a GUI.

It reads [`model-manifest.json`](../model-manifest.json) — **the same file the panel embeds**,
not a copy of the digests. One more copy is one more thing to keep in sync, and the failure it
would cause is a dev provisioning different weights from the ones a release installs. The
`base_url` there pins an immutable HuggingFace revision.

```bash
python3 native-deps/fetch-model.py                 # idempotent; re-run to repair
python3 native-deps/fetch-model.py --verify-only   # hash everything; never touches the network
python3 native-deps/fetch-model.py --dest DIR      # provision somewhere else
```

Unlike the panel, an already-present file is **hashed** rather than accepted on its length: the
panel skips on size because it has a progress bar and a repair button, and this script is that
repair. A fetched file lands as `.part` and is renamed only once its digest matches, so an
interrupted run never leaves a short file where the host would load it.

**DEV only**, like `fetch-ocr-models.py`: a release install never runs it, and the installer
stages no model.
