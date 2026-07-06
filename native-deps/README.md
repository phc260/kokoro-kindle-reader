# native-deps — synth dependency provisioning

**Not a crate** — just the scripts that provision the native runtime the synth needs, plus
their output. Populates `third_party/` (gitignored; re-created by the scripts):

- the **Dawn/WebGPU runtime DLLs** from the `onnxruntime-webgpu` pip wheel
  (`onnxruntime.dll` + `onnxruntime_providers_shared.dll` + `dxcompiler.dll` + `dxil.dll`)
- an **espeak-ng x64 build** (`espeak-ng.dll` + import lib + `espeak-ng-data`)

## Run this first

```powershell
.\tools\fetch-deps.ps1   # downloads the wheel + builds espeak; idempotent (-Force to redo)
```

`kokoro-host`'s `build.rs` panics if `third_party/` is missing, so this must run before
building the host. It also stages the 5 runtime DLLs next to the exe. The ONNX model runs
on the `ort` crate's WebGPU EP via load-dynamic, so `onnxruntime.dll` is loaded at runtime
(not linked) — no ORT headers/import lib needed.

Requires Python+pip (for `pip download` of the wheel), CMake + MSVC (to build espeak), and
network. `tools/build-espeak.ps1` is called by `fetch-deps.ps1`; it builds espeak-ng
1.52.0 x64 with the horse-hoarse phoneme revert this model expects.
