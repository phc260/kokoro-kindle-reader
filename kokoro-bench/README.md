# kokoro-bench — GPU-vs-CPU synth timing tool

A standalone benchmark, **not part of the shipping app**: times one fixed paragraph
through the Kokoro model on WebGPU fp32 (the host's default), CPU fp32 at ORT's
default intra-op thread count (physical cores — what the host ships), and CPU fp32
with intra-op threads raised to every logical core ("multi-CPU"), so a laptop's real
synthesis speed can be measured before deciding whether to flip `controls.json`'s
`gpu_synth` off (the panel's "Synthesize on GPU" checkbox). This is what found that an
Intel UHD 620 runs WebGPU at 0.50x realtime but CPU fp32 at 1.07x — see
`kokoro-host/src/native_synth.rs`'s `Engine`.

The multi-CPU row exists to answer "does raising thread count actually help, or just
raise the Task Manager number?" — see the settled finding below. The int8-quantized
row that used to be here was dropped: quantization was a separate, already-settled
dead end (consistently slower than fp32 CPU on every machine tested — the model's ops
don't hit fast int8 kernels in ORT's default CPU EP), so it no longer earns a row.

It then runs a **concurrent GPU+CPU** pass (two sessions, two threads, a shared
deadline): the go/no-go gate for a hybrid dispatcher that would alternate chunks
between both engines on machines where neither alone sustains realtime. Sequential
rows overstate what a hybrid would get — an iGPU and the CPU cores share one package
power budget and throttle each other — so this measures the real combined rate. The
window is deliberately long (60 s default, `--hybrid-secs` to change) to reach
thermal steady state rather than boost clocks.

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
# or against a different model dir / narrator / hybrid window:
cargo run --release -- --model-dir <path> --voice af_bella --hybrid-secs 30
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
CPU     fp32 (N threads)             ...            ...             ...x

hybrid  fp32 GPU+CPU concurrent (60s window):
config                               runs      audio (s)  realtime factor
  WebGPU share                        ...            ...             ...x
  CPU share                           ...            ...             ...x
  combined                            ...            ...             ...x
```

"Cold" is the first run per engine (pays shader-compile / kernel-selection cost);
"warm avg" is the mean of 3 subsequent runs. Realtime factor > 1 means faster than the
audio it produces. `N` in the "CPU fp32 (N threads)" row is
`std::thread::available_parallelism()` (all logical cores, hyperthreads included) —
compare it against the plain "CPU fp32" row (ORT's default, physical cores only) to
see whether the extra threads buy real throughput or just higher CPU-meter readings.

In the hybrid section, each engine's rate is its audio produced over its own busy
span, and "combined" is the two rates summed — expect each share to come in *below*
its solo row (that drop is the shared-power throttling the pass exists to measure).
The hybrid dispatcher is worth building only if "combined" clears ~1.15x on the
target machine; at or below realtime, gaps are unavoidable no matter how chunks are
scheduled.

**Settled finding (2026-07): more CPU threads made synthesis *slower* on the i5-8350U.**
Measured with this bench pass: default (4 physical cores) 14749ms warm avg / 1.12x
realtime, vs. all 8 logical cores 16771ms warm avg / 0.98x — the 8-thread run was ~14%
slower, not merely flat. Consistent with the earlier real-host observation (CPU% went
up, the inter-chunk gap didn't shrink): on this power/FMA-limited 15W chip,
hyperthreads add contention, not throughput — `SetIntraOpNumThreads` above the
physical core count is a net loss here. This is why `native_synth.rs` never adopted a
thread-count override and ships ORT's default. Don't re-add it without a different
chip class to test against.

**Settled finding (2026-07): the gate FAILED on the reference integrated-GPU laptop**
(i5-8350U / UHD 620, 15W), confirmed across two runs: solo WebGPU 0.49-0.56x / CPU
1.01-1.12x, but concurrent shares were consistently ~0.4x + ~0.5x = **0.88-0.94x
combined** — below CPU running alone every time. The active iGPU starves the CPU cores
of package power so badly (CPU per-run time roughly doubled) that hybrid is *worse
than CPU alone*, not merely no-better. Machines that would need a hybrid can't benefit
from one, and machines with a real GPU don't need one — the hybrid dispatcher was
therefore never built. Don't re-litigate without meaningfully different hardware (e.g.
a desktop iGPU without the 15W power ceiling).

See the repo-root [`CLAUDE.md`](../CLAUDE.md) and [`ARCHITECTURE.md`](../ARCHITECTURE.md)
for how the GPU/CPU engine choice fits the synth pipeline.
