# kokoro-bench — GPU-vs-CPU synth timing tool

A standalone benchmark, **not part of the shipping app**: times one fixed paragraph
through the Kokoro model on WebGPU fp32 (the host's default), CPU fp32, and CPU
int8-quantized, so a laptop's real synthesis speed can be measured before deciding
whether to flip `controls.json`'s `gpu_synth` off (the panel's "Synthesize on GPU"
checkbox). This is what found that an Intel UHD 620 runs WebGPU at 0.50x realtime but
CPU fp32 at 1.07x — see `kokoro-host/src/native_synth.rs`'s `Engine`.

A separate crate (own target dir) rather than a `kokoro-host` bin, so it never compiles
as part of a normal `cargo build`/`check` on the shipping tray daemon. It reuses
`kokoro-host/src/text.rs` and `espeak.rs` via `#[path]` includes — `kokoro-host` is
bin-only (no lib target) — so those two files must stay pure/self-contained (no
`kokoro-host`-specific state) for this to keep building.

## Build & run

```powershell
# One-time: provision the synth runtime deps (must run first — build.rs panics without it).
..\native-deps\fetch-deps.ps1

cargo run --release
# or against a different model dir / narrator:
cargo run --release -- --model-dir <path> --voice af_bella
```

Defaults to the model dir the panel actually downloads into
(`%APPDATA%\com.phc260.kokoro-kindle-reader\onnx-community\Kokoro-82M-v1.0-ONNX`) and
voice `af_heart`, so a normal install needs no flags.

## Testing on another machine

To measure a laptop that doesn't have the dev toolchain, copy `target\release\`'s
`bench_synth.exe` + its 5 `*.dll` files + the `espeak-ng-data` folder to that machine
(alongside each other) and run it there — it only needs the model already downloaded
via a normal app install (same `%APPDATA%` path).

## Output

```
config                          cold (ms)  warm avg (ms)  realtime factor
WebGPU  fp32 (shipping)              ...            ...             ...x
CPU     fp32                         ...            ...             ...x
CPU     int8 (quantized)             ...            ...             ...x
```

"Cold" is the first run per engine (pays shader-compile / kernel-selection cost);
"warm avg" is the mean of 3 subsequent runs. Realtime factor > 1 means faster than the
audio it produces. Quantized has consistently run *slower* than fp32 CPU on every
machine tested so far (not a fluke — this model's ops don't hit fast int8 kernels in
ORT's default CPU EP) — treat that as a settled finding, not something to re-litigate
per-machine.

See the repo-root [`CLAUDE.md`](../CLAUDE.md) and [`ARCHITECTURE.md`](../ARCHITECTURE.md)
for how the GPU/CPU engine choice fits the synth pipeline.
