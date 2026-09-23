# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "onnxruntime-webgpu==1.27.0",
#     "numpy",
#     "onnx",
# ]
# ///
"""Does THIS machine's WebGPU break Kokoro's harmonic source?

Standalone: one file, no espeak, no kokoro-host, no stack-check app. It answers the question
the stack-check investigation left open for Windows production (Dawn -> D3D12).

Background (measured on the Linux reference laptop, Iris Xe, Dawn -> Vulkan -> Mesa ANV):
  * WebGPU's Sin was accurate (~3e-5) up to |x| ~ 102,839 (~ 32768*pi), and garbage past it.
    WGSL only guarantees sin() accuracy inside [-pi, pi], so this is allowed, not a bug.
  * Kokoro's harmonic source (/decoder/decoder/generator/m_source/l_sin_gen/Sin) is fed an
    UNWRAPPED cumulative phase: ~2e5 rad after 6 s of audio. So on that GPU every model run
    longer than ~3.6 s lost its harmonic excitation in the tail -- muffled, ~12% quieter.
  * Wrapping the phase into [0, 2*pi) before that Sin made WebGPU match CPU (corr 0.997) and
    left CPU unchanged (corr 0.9995).
The Windows runtime is provisioned from the same onnxruntime_webgpu-1.27.0 wheel this script
installs, so a result here speaks for production's onnxruntime.dll + Dawn -- but the Sin
precision is decided by the GPU/driver/D3D12 stack, which is exactly what this measures.

Run (Windows, from the repo root) -- with uv, dependencies come from the header above:
    uv run stack-check\\webgpu_sin_check.py
Without uv:
    python -m venv %TEMP%\\sincheck
    %TEMP%\\sincheck\\Scripts\\python -m pip install onnxruntime-webgpu==1.27.0 numpy onnx
    %TEMP%\\sincheck\\Scripts\\python stack-check\\webgpu_sin_check.py

Part 1 (a one-node Sin model) needs nothing else. Part 2 runs the real Kokoro model from the
host's app-data dir (override with --model DIR, skip with --sin-only) and writes three WAVs
to listen to. Paste the whole output back into the chat.
"""
from __future__ import annotations

import argparse
import os
import platform
import subprocess
import sys
import tempfile
import time
import wave
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
from onnx import TensorProto, helper, numpy_helper

GPU_EP = ["WebGpuExecutionProvider", "CPUExecutionProvider"]
CPU_EP = ["CPUExecutionProvider"]
SIN_NODE = "/decoder/decoder/generator/m_source/l_sin_gen/Sin"
SR = 24000
BAD = 1e-3          # a correct fp32 sin is ~1e-7 off; a healthy GPU sin ~3e-5; "broken" is > this
KOKORO_6S = 2.0e5   # the sine phase Kokoro reaches after ~6 s of audio in one model run

# An invented passage (repo fixture rule), phonemized by the project's MODIFIED espeak-ng 1.52.0
# on Linux so this script needs no espeak. 203 tokens -> ~12.7 s of audio in one model window.
PHONEMES = ("ðə kwˈɪk bɹˈaʊn fˈɑːks dʒˈʌmps ˌoʊvɚ ðə lˈeɪzi dˈɑːɡ kˈoʊkəɹoʊ ɹˈʌnz ɔnðɪ ˈɑːŋŋks "
            "ɹˈʌntaɪm nˈeɪɾɪvli ˌɔn ðɪs məʃˈiːn ænd ðə lˈaɪthaʊs kˈiːpɚ wˈeɪvz æɾ ˈɛvɹi pˈæsɪŋ "
            "ʃˈɪp bᵻfˌoːɹ ðɪ ˈiːvnɪŋ tˈaɪd kˈʌmz ˈɪn")


def default_model_dir() -> Path:
    """Mirrors native-deps/fetch-model.py's app_data_dir() (and so kokoro-host's)."""
    if os.name == "nt":
        base = Path(os.environ.get("APPDATA", "")) / "com.phc260.kokoro-kindle-reader"
    elif os.environ.get("XDG_DATA_HOME"):
        base = Path(os.environ["XDG_DATA_HOME"]) / "kokoro-kindle-reader"
    else:
        base = Path.home() / ".local" / "share" / "kokoro-kindle-reader"
    return base / "onnx-community" / "Kokoro-82M-v1.0-ONNX"


def gpu_name() -> str:
    try:
        if os.name == "nt":
            cmd = ["powershell", "-NoProfile", "-Command",
                   "Get-CimInstance Win32_VideoController | ForEach-Object "
                   "{ \"$($_.Name) (driver $($_.DriverVersion))\" }"]
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=20).stdout
            return "; ".join(l.strip() for l in out.splitlines() if l.strip()) or "unknown"
        out = subprocess.run(["lspci"], capture_output=True, text=True, timeout=5).stdout
        return "; ".join(l.split(":", 2)[-1].strip() for l in out.splitlines()
                         if any(k in l.lower() for k in ("vga", "3d", "display"))) or "unknown"
    except Exception:
        return "unknown"


def section(title: str):
    print(f"\n=== {title} ===")


# ---- Part 1: Sin accuracy vs argument size -------------------------------------------------
def sin_model_bytes() -> bytes:
    x = helper.make_tensor_value_info("x", TensorProto.FLOAT, [None])
    y = helper.make_tensor_value_info("y", TensorProto.FLOAT, [None])
    g = helper.make_graph([helper.make_node("Sin", ["x"], ["y"])], "sin", [x], [y])
    # ir_version pinned: newer `onnx` stamps a version onnxruntime 1.27 refuses to load.
    return helper.make_model(g, opset_imports=[helper.make_opsetid("", 20)],
                             ir_version=10).SerializeToString()


def part1() -> float | None:
    """Return the |x| where WebGPU's Sin breaks down, or None if it holds up to 2e6."""
    section("Part 1: Sin error vs |x|  (reference: float64 sin of the same fp32 input)")
    blob = sin_model_bytes()
    gpu = ort.InferenceSession(blob, providers=GPU_EP)
    cpu = ort.InferenceSession(blob, providers=CPU_EP)
    rng = np.random.default_rng(1)

    def err(sess, xs):
        return float(np.abs(sess.run(["y"], {"x": xs})[0] - np.sin(xs.astype(np.float64))).max())

    edges = [0, 1e4, 5e4, 1e5, 1.5e5, 2e5, 5e5, 2e6]
    first_bad = None
    print(f"{'|x| band':>24} {'WebGPU':>10} {'CPU':>10}")
    for lo, hi in zip(edges[:-1], edges[1:]):
        xs = rng.uniform(lo, hi, 200_000).astype(np.float32)
        eg, ec = err(gpu, xs), err(cpu, xs)
        flag = "  <-- broken" if eg > BAD else ""
        print(f"[{lo:>9.0f}, {hi:>9.0f}) {eg:10.3g} {ec:10.3g}{flag}")
        if eg > BAD and first_bad is None:
            first_bad = (lo, hi)
    if first_bad is None:
        return None
    lo, hi = first_bad           # bisect inside the first broken band
    for _ in range(40):
        mid = (lo + hi) / 2
        xs = np.linspace(mid, mid * 1.001, 20_000, dtype=np.float32)
        lo, hi = (lo, mid) if err(gpu, xs) > BAD else (mid, hi)
    print(f"WebGPU Sin breaks down at |x| ~ {hi:,.0f}   (32768*pi = {32768 * np.pi:,.0f})")
    return hi


# ---- Part 2: the real model --------------------------------------------------------------
def wrap_phase(model: onnx.ModelProto) -> None:
    """In place: x -> x - 2*pi*floor(x / 2*pi) before the harmonic source's Sin."""
    g = model.graph
    idx = next((i for i, n in enumerate(g.node) if n.name == SIN_NODE), None)
    if idx is None:
        raise SystemExit(f"{SIN_NODE} not in the graph -- is this the Kokoro-82M v1.0 model?")
    src = g.node[idx].input[0]
    g.initializer.extend([
        numpy_helper.from_array(np.array(1 / (2 * np.pi), np.float32), "sine_wrap/inv_2pi"),
        numpy_helper.from_array(np.array(2 * np.pi, np.float32), "sine_wrap/two_pi")])
    nodes = [helper.make_node("Mul", [src, "sine_wrap/inv_2pi"], ["sine_wrap/cycles"]),
             helper.make_node("Floor", ["sine_wrap/cycles"], ["sine_wrap/whole"]),
             helper.make_node("Mul", ["sine_wrap/whole", "sine_wrap/two_pi"], ["sine_wrap/offset"]),
             helper.make_node("Sub", [src, "sine_wrap/offset"], ["sine_wrap/phase"])]
    for k, n in enumerate(nodes):
        g.node.insert(idx + k, n)
    g.node[idx + len(nodes)].input[0] = "sine_wrap/phase"


def cpp_db(x: np.ndarray, voiced: np.ndarray, n=1024, hop=256) -> float:
    """Mean cepstral peak prominence over `voiced` frames -- higher = clearer harmonics."""
    q = np.arange(n) / SR
    band = (q >= 1 / 400) & (q <= 1 / 70)
    fr = np.lib.stride_tricks.sliding_window_view(x, n)[::hop] * np.hanning(n)
    ce = 20 * np.log10(np.abs(np.fft.ifft(
        20 * np.log10(np.abs(np.fft.fft(fr, axis=1)) + 1e-9), axis=1)) + 1e-9)
    vals = []
    for row in ce[voiced[:len(ce)]]:
        seg, qs = row[band], q[band]
        k = int(np.argmax(seg))
        a, b = np.polyfit(qs, seg, 1)
        vals.append(seg[k] - (a * qs[k] + b))
    return float(np.mean(vals))


def write_wav(path: Path, pcm: np.ndarray):
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(SR)
        w.writeframes((np.clip(pcm, -1, 1) * 32767).astype("<i2").tobytes())


def part2(model_dir: Path, threshold: float | None, out_dir: Path) -> dict:
    section(f"Part 2: Kokoro model  ({model_dir})")
    import json
    vocab = json.loads((model_dir / "tokenizer.json").read_text(encoding="utf-8"))["model"]["vocab"]
    content = [int(vocab[c]) for c in PHONEMES if c in vocab]
    voice = np.frombuffer((model_dir / "voices" / "af_heart.bin").read_bytes(), "<f4").reshape(510, 256)
    feeds = {"input_ids": np.array([[0] + content + [0]], np.int64),
             "style": voice[min(len(content), 509)].reshape(1, 256).astype(np.float32),
             "speed": np.array([1.0], np.float32)}
    phase_name = SIN_NODE.rsplit("/", 1)[0] + "/Transpose_3_output_0"   # the Sin's input

    print("loading + patching the model (325 MB, takes a moment)...")
    model = onnx.load(str(model_dir / "onnx" / "model.onnx"))
    model.graph.output.append(helper.make_tensor_value_info(phase_name, TensorProto.FLOAT, None))
    plain = model.SerializeToString()
    wrap_phase(model)
    wrapped = model.SerializeToString()
    del model

    def run(blob, providers, names=("waveform",)):
        s = ort.InferenceSession(blob, providers=providers)
        t = time.perf_counter()
        out = s.run(list(names), feeds)
        return out, time.perf_counter() - t, s.get_providers()

    (cpu, phase), _, _ = run(plain, CPU_EP, ("waveform", phase_name))
    (gpu,), _, gp = run(plain, GPU_EP)
    (gpu_w,), _, _ = run(wrapped, GPU_EP)
    (cpu_w,), _, _ = run(wrapped, CPU_EP)
    cpu, gpu, gpu_w, cpu_w = (a.ravel().astype(np.float64) for a in (cpu, gpu, gpu_w, cpu_w))
    print(f"tokens {len(content)} | audio {len(cpu) / SR:.2f}s | WebGPU session providers: {gp}")

    ph = np.abs(np.squeeze(phase))
    if ph.ndim == 2 and ph.shape[0] < ph.shape[1]:
        ph = ph.T                                            # -> (samples, harmonic channels)
    ph = ph.reshape(ph.shape[0], -1)
    print(f"sine-source phase: max |x| = {ph.max():,.0f} rad over {ph.shape[1]} harmonic channels")
    cross = None
    if threshold is not None:
        over = np.nonzero((ph > threshold).any(axis=1))[0]
        if over.size:
            cross = over[0] / SR
            print(f"crosses this GPU's Sin limit {cross:.2f}s into the run; "
                  f"{100 * np.mean(ph > threshold):.1f}% of sine-source samples past it")
        else:
            print("never crosses this GPU's Sin limit in this run")

    frames = np.lib.stride_tricks.sliding_window_view(cpu, 1024)[::256]
    energy = np.sqrt(np.mean(frames ** 2, axis=1))
    voiced = energy > 0.25 * energy.max()
    corr = lambda a, b: float(np.corrcoef(a, b)[0, 1])
    print(f"{'':14} {'corr vs CPU':>12} {'CPP dB':>8} {'RMS':>8}")
    rows = {"CPU": cpu, "WebGPU": gpu, "WebGPU +wrap": gpu_w, "CPU +wrap": cpu_w}
    for k, x in rows.items():
        print(f"{k:14} {corr(cpu, x):12.4f} {cpp_db(x, voiced):8.2f} {np.sqrt(np.mean(x ** 2)):8.4f}")
    if cross is not None and 0.5 < cross < len(cpu) / SR - 0.5:
        c = int(cross * SR)
        print(f"WebGPU vs CPU before/after the crossing: {corr(cpu[:c], gpu[:c]):.4f} / "
              f"{corr(cpu[c:], gpu[c:]):.4f}")

    out_dir.mkdir(parents=True, exist_ok=True)
    for name, x in (("cpu", cpu), ("webgpu", gpu), ("webgpu_wrap", gpu_w)):
        write_wav(out_dir / f"{name}.wav", x)
    print(f"wrote {out_dir / 'cpu.wav'}, webgpu.wav, webgpu_wrap.wav -- listen to the tails")
    return {"corr_gpu": corr(cpu, gpu), "corr_gpu_wrap": corr(cpu, gpu_w),
            "corr_cpu_wrap": corr(cpu, cpu_w), "phase_max": float(ph.max()), "cross": cross,
            "seconds": len(cpu) / SR}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--model", type=Path, default=None, help="Kokoro-82M-v1.0-ONNX dir "
                    "(default: the host's app-data dir)")
    ap.add_argument("--sin-only", action="store_true", help="skip Part 2 (no model needed)")
    ap.add_argument("--out", type=Path, default=Path(tempfile.gettempdir()) / "webgpu_sin_check",
                    help="where Part 2 writes its WAVs (default: <temp>/webgpu_sin_check)")
    args = ap.parse_args()

    section("Machine")
    print(f"OS          : {platform.platform()}")
    print(f"Python      : {platform.python_version()}")
    print(f"onnxruntime : {ort.__version__}  providers {ort.get_available_providers()}")
    print(f"GPU         : {gpu_name()}")
    if "WebGpuExecutionProvider" not in ort.get_available_providers():
        raise SystemExit("\nNo WebGpuExecutionProvider -- install onnxruntime-webgpu==1.27.0 "
                         "(not plain onnxruntime) and rerun.")

    threshold = part1()
    result = None
    if not args.sin_only:
        model_dir = args.model or default_model_dir()
        if (model_dir / "onnx" / "model.onnx").exists():
            result = part2(model_dir, threshold, args.out)
        else:
            print(f"\n(Part 2 skipped: no model at {model_dir} -- pass --model DIR)")

    section("Verdict")
    if threshold is None:
        print("NOT AFFECTED: this GPU's WebGPU Sin is accurate up to |x| = 2e6.")
        if result:
            per_s = result["phase_max"] / result["seconds"]
            print(f"Kokoro's phase grows ~{per_s:,.0f} rad/s here, so a single model run stays "
                  f"inside that range for ~{2e6 / per_s:,.0f} s -- longer than any one window.")
    else:
        when = (f"~{result['cross']:.1f}s into each model run" if result and result["cross"]
                else "once the phase passes it")
        print(f"AFFECTED: WebGPU Sin breaks at |x| ~ {threshold:,.0f}; Kokoro's harmonic-source "
              f"phase reaches ~{KOKORO_6S:,.0f} after ~6 s, so it crosses {when}.")
        print("Anything longer than that is synthesized with a corrupted harmonic source on WebGPU.")
    if result:
        fixed = result["corr_gpu_wrap"] > 0.99
        print(f"Model output vs CPU: WebGPU {result['corr_gpu']:.3f} -> with phase wrap "
              f"{result['corr_gpu_wrap']:.3f} ({'wrap fixes it' if fixed else 'wrap does NOT fully fix it'}); "
              f"CPU with wrap {result['corr_cpu_wrap']:.4f}.")


if __name__ == "__main__":
    main()
