# native-deps — synth dependency provisioning

**Not a crate** — just the scripts that provision the native runtime the synth needs.
Populates the gitignored dep folders alongside them (`runtime/` or `linux/runtime/`, plus
`espeak-ng-src/`; re-created by the scripts).

**Windows** (`fetch-deps.ps1` -> `runtime/`):

- the **Dawn/WebGPU runtime DLLs** from the exact SHA-256-pinned
  `onnxruntime-webgpu` CPython 3.12 Windows wheel
  (`onnxruntime.dll` + `onnxruntime_providers_shared.dll` + `dxcompiler.dll` + `dxil.dll`)
- an **espeak-ng x64 build** (`espeak-ng.dll` + import lib + `espeak-ng-data`)

**Linux** (`fetch-deps.sh` -> `linux/runtime/`):

- the **CPU ONNX Runtime** (`libonnxruntime.so*`) from the exact SHA-256-pinned
  `onnxruntime` CPython 3.12 manylinux wheel — **the CPU wheel, not the WebGPU one, and
  deliberately.** The equivalent WebGPU wheel exists, but native WebGPU is Vulkan on Linux
  and none of it has been validated: not the library's own dependencies, not the `ort`
  binding's registration, not the drivers. The CPU milestone must not be able to fail for a
  GPU reason. Evaluating that wheel is the GPU stage's job, and it is a separate pin.
- an **espeak-ng shared-library build** (`libespeak-ng.so*` + `espeak-ng-data`), from the
  same pinned commit and the same one-line modification as Windows

Both, by a second script: the **Cloud Reader OCR models** (`ocr/`).

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

On Linux:

```bash
./fetch-deps.sh          # same, for linux/runtime/ (--force to redo)
```

`kokoro-host`'s `build.rs` panics if the dep folders for the target being built are missing,
so this must run before building the host — and it branches on the **target**, so
cross-checking a Linux target needs the Linux provision, not the Windows one. It also stages
the runtime libraries next to the exe (found there at run time by an `$ORIGIN` rpath on
Linux). The ONNX model runs on the `ort` crate's execution providers via load-dynamic, so
the runtime library is loaded at run time (not linked) — no ORT headers/import lib
needed.

Requires CMake + a C toolchain (to build espeak) and network: MSVC on Windows, gcc/clang on
Linux. `build-espeak.ps1` / `build-espeak.sh` are called by their respective fetch scripts;
each builds espeak-ng 1.52.0 commit `4870adfa25b1a32b4361592f1be8a40337c58d6c` with the
horse-hoarse phoneme revert this model expects.

**The two espeak recipes must stay pin-for-pin identical**: same immutable commit, same
single documented modification, the same `ffa5cbde...` digest of the patched
`phsource/ph_english_us`, and the same refusal to build a tree carrying anything else. They
differ only in how they drive CMake. A phoneme difference between the platforms would not
surface as an error — it would surface as the voice saying something slightly different, on
one OS only. **A distribution's own libespeak-ng is not a substitute**: it is unmodified, and
probably not 1.52.0 either, and either difference changes the phonemes.

## fetch-ocr-models.ps1

Separate because it needs **network and nothing else** — no Python, no compiler, minutes
faster. It is also not a build dependency: `kokoro-ocr` loads these at run time, so the host
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
