# native-deps — synth dependency provisioning

**Not a crate** — just the scripts that provision the native runtime the synth needs.
Populates the gitignored dep folders alongside them (`runtime/` + `espeak-ng-src/`;
re-created by the scripts):

- the **Dawn/WebGPU runtime DLLs** from the `onnxruntime-webgpu` pip wheel
  (`onnxruntime.dll` + `onnxruntime_providers_shared.dll` + `dxcompiler.dll` + `dxil.dll`)
- an **espeak-ng x64 build** (`espeak-ng.dll` + import lib + `espeak-ng-data`)
- the **Cloud Reader OCR models** (`ocr/`), by a second script

## Run this first

```powershell
.\fetch-deps.ps1        # downloads the wheel + builds espeak; idempotent (-Force to redo)
.\fetch-ocr-models.ps1  # the 9.80 MB PP-OCR pair + dictionary; network only, no toolchain
```

`kokoro-host`'s `build.rs` panics if these dep folders are missing, so this must run
before
building the host. It also stages the 5 runtime DLLs next to the exe. The ONNX model runs
on the `ort` crate's WebGPU EP via load-dynamic, so `onnxruntime.dll` is loaded at runtime
(not linked) — no ORT headers/import lib needed.

Requires Python+pip (for `pip download` of the wheel), CMake + MSVC (to build espeak), and
network. `build-espeak.ps1` is called by `fetch-deps.ps1`; it builds espeak-ng
1.52.0 x64 with the horse-hoarse phoneme revert this model expects.

## fetch-ocr-models.ps1

Separate because it needs **network and nothing else** — no Python, no compiler, minutes
faster. It is also not a build dependency: `kokoro-ocr` loads these at run time, so the host
builds without them and reports OCR `missing` until they are there. `build-installer.ps1`
stages `ocr/` into the package and calls this script's **`-VerifyOnly`** mode first, which checks
all three digests and throws without touching the network. An existence check on one file is not
enough: an interrupted download leaves one present and another absent, the build succeeds, and
the host reports `missing` on the first page.

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
