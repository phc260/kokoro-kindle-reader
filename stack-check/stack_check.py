#!/usr/bin/env python3
"""Kokoro Kindle Reader -- stack check & CPU/GPU speed test (Slint GUI).

A throwaway diagnostic, not part of the product. It answers, on THIS machine:
  * Does the tech stack run?  espeak-ng -> phonemes -> Kokoro (ONNX Runtime) -> audio.
  * How fast, and how does it scale across execution providers / CPU threads?

The window is ui/stack_check.slint -- the same toolkit, Slint version and Fluent style as the
production settings panel (kokoro-panel/ui/panel.slint). The synthesis core is kokoro_min.py.

Threading: Slint objects may only be touched on the UI thread, and the Python bindings have no
invoke_from_event_loop. So heavy work runs on worker threads that post closures to a queue, and
a repeating slint.Timer drains that queue on the UI thread.

Garbage collection: Slint's struct values (slint_python's PyStruct) are pyo3 `unsendable`
objects. If Python's cyclic GC happens to run on a worker thread -- and it runs on whichever
thread allocates past the threshold, which the synthesis workers do constantly -- it clears a
PyStruct made on the UI thread, pyo3 panics, and the whole process aborts. So automatic GC is
off, and a timer runs gc.collect() on the UI thread instead. Reference counting still frees
everything acyclic immediately; only cycles wait for the next collection.
"""

from __future__ import annotations

import gc
import os
import platform
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import replace
from datetime import timedelta
from importlib.metadata import version as pkg_version
from pathlib import Path

import numpy as np
import slint

import kokoro_min as km

UI_FILE = Path(__file__).resolve().parent / "ui" / "stack_check.slint"
OK, WARN, FAIL, INFO = "ok", "warn", "fail", "info"
DEFAULT_TEXT = ("The quick brown fox jumps over the lazy dog. "
                "Kokoro runs on the ONNX Runtime, natively on this machine.")
MAX_LOG_LINES = 500
GC_INTERVAL = timedelta(seconds=5)     # UI-thread collection period (see module docstring)


def _tilde(p) -> str:
    """A path with the home dir folded to ~ -- full paths otherwise dominate the table."""
    s, home = str(p), str(Path.home())
    return "~" + s[len(home):] if s.startswith(home) else s


def _repo_rel(p) -> str:
    """A path inside the repo shown relative to the repo root; anything else via _tilde."""
    try:
        return str(Path(p).resolve().relative_to(km.REPO_ROOT))
    except ValueError:
        return _tilde(p)


class App:
    def __init__(self):
        gc.disable()                                        # see "Garbage collection" above
        ui = slint.load_file(UI_FILE, style="fluent")      # the panel's style
        self.w = ui.AppWindow()
        self.q: queue.Queue = queue.Queue()
        self.busy = False
        self.player: subprocess.Popen | None = None
        self.synth: km.Synth | None = None                  # built lazily on a worker
        self.last_pcm: np.ndarray | None = None
        self.specs: list[km.ProviderSpec] = []              # available EPs, from the env check
        self.cfg_specs: list[km.ProviderSpec] = []          # parallel to the bench-configs model
        self.cfg_checked: list[bool] = []
        self.cfg_model = slint.ListModel([])
        self.log_lines: list[str] = []

        w = self.w
        w.synth_text = DEFAULT_TEXT
        w.save_path = str(Path.home() / "kokoro_stack_check.wav")
        w.recheck = self.on_recheck
        w.synthesize = self.on_synth
        w.stop = self.on_stop
        w.save_wav = self.on_save
        w.run_bench = self.on_bench
        w.bench_config_toggled = self.on_config_toggled

        self._pump_timer = slint.Timer()                    # must stay referenced to keep firing
        self._pump_timer.start(slint.TimerMode.Repeated, timedelta(milliseconds=50), self._pump)
        self._gc_timer = slint.Timer()
        self._gc_timer.start(slint.TimerMode.Repeated, GC_INTERVAL, _collect_garbage)
        _collect_garbage()        # whatever loading the UI left in cycles, before any worker runs
        self.set_busy(True, "Checking environment…")
        threading.Thread(target=self._check_environment, daemon=True).start()

    def run(self):
        """Show the window; returns when it is closed."""
        try:
            self.w.run()
        finally:
            self._stop_player()

    # ---- UI-thread plumbing -------------------------------------------------
    def post(self, fn):
        """Run `fn` on the UI thread -- the only thread allowed to touch self.w."""
        self.q.put(fn)

    def _pump(self):
        while True:
            try:
                fn = self.q.get_nowait()
            except queue.Empty:
                return
            try:
                fn()
            except Exception as e:                          # one bad update must not kill the pump
                self.logline(f"[ui error] {e!r}")

    def logline(self, msg: str):
        self.log_lines.insert(0, f"{time.strftime('%H:%M:%S')}  {msg}")
        del self.log_lines[MAX_LOG_LINES:]
        self.w.log_text = "\n".join(self.log_lines)

    def set_busy(self, busy: bool, status: str = ""):
        self.busy = busy
        self.w.busy = busy
        if status:
            self.w.status = status

    def _ensure_synth(self) -> km.Synth:
        """Build the espeak + Synth core once, on a worker thread."""
        if self.synth is None:
            self.synth = km.Synth(km.DEFAULT_MODEL_DIR, km.Espeak(km.DEFAULT_ESPEAK_RUNTIME))
        return self.synth

    def _spec_by_label(self, label: str) -> km.ProviderSpec:
        for s in self.specs:
            if s.label == label:
                return s
        return km.ProviderSpec("CPU", ["CPUExecutionProvider"])

    # ---- environment check --------------------------------------------------
    def on_recheck(self):
        if self.busy:
            return
        self.set_busy(True, "Checking environment…")
        threading.Thread(target=self._check_environment, daemon=True).start()

    def _check_environment(self):
        rows: list[tuple[str, str, str]] = []
        rows.append((OK, "Python", f"{platform.python_version()}  ({_tilde(os.path.realpath(sys.executable))})"))
        rows.append((OK, "slint", f"{pkg_version('slint')}  ·  Fluent style, as in kokoro-panel"))
        rows.append((OK, "numpy", np.__version__))

        specs: list[km.ProviderSpec] = []
        try:
            import onnxruntime as ort
            avail = ort.get_available_providers()
            rows.append((OK, "onnxruntime", ort.__version__))
            has_gpu_ep = any(p in avail for p in
                             ("WebGpuExecutionProvider", "CUDAExecutionProvider", "DmlExecutionProvider"))
            note = "" if "WebGpuExecutionProvider" in avail else \
                   "  — no WebGPU EP: the venv has plain onnxruntime, not onnxruntime-webgpu (uv sync)"
            rows.append((OK if has_gpu_ep else WARN, "Execution providers", ", ".join(avail) + note))
            specs = km.available_provider_specs()
        except Exception as e:
            rows.append((FAIL, "onnxruntime", f"import failed: {e!r}"))

        # onnx -- only needed to patch the graph for the sine-phase wrap toggle
        wrap_ok = km.wrap_available()
        if wrap_ok:
            import onnx
            rows.append((OK, "onnx", f"{onnx.__version__}  ·  enables the sine-phase wrap toggle"))
        else:
            rows.append((WARN, "onnx", "not installed — sine-phase wrap toggle disabled (uv sync)"))

        rows.append((INFO, "CPU", f"{_cpu_name()}  ·  {os.cpu_count()} logical cores"))
        rows.append((INFO, "GPU (hardware)", _gpu_name() or "unknown"))

        try:
            esp = km.Espeak(km.DEFAULT_ESPEAK_RUNTIME)
            rows.append((OK, "espeak-ng", f"{esp.version()}  ·  {_repo_rel(esp.lib_path)}"))
        except Exception as e:
            rows.append((FAIL, "espeak-ng", f"{e}"))

        # The model dir gets its own row so the file rows below stay short enough for one line.
        md = km.DEFAULT_MODEL_DIR
        rows.append((OK if md.is_dir() else FAIL, "Model dir",
                     _tilde(md) if md.is_dir() else f"missing: {_tilde(md)}"))
        mp = md / "onnx" / "model.onnx"
        if mp.exists():
            rows.append((OK, "Kokoro model", f"{mp.stat().st_size/1e6:.0f} MB  ·  onnx/model.onnx"))
        else:
            rows.append((FAIL, "Kokoro model", "missing: onnx/model.onnx"))
        vp = md / "voices" / "af_heart.bin"
        rows.append((OK if vp.exists() else FAIL, "Voice (af_heart)",
                     "voices/af_heart.bin" if vp.exists() else "missing: voices/af_heart.bin"))

        player = _find_player()
        rows.append((OK if player else WARN, "Audio player",
                     player or "none found (pw-play / aplay) — playback disabled"))

        self.post(lambda: self._render_env(rows, specs, wrap_ok))

    def _render_env(self, rows, specs, wrap_ok):
        w = self.w
        w.env_rows = slint.ListModel([{"status": s, "component": c, "detail": d} for s, c, d in rows])
        self.specs = specs
        w.providers = slint.ListModel([s.label for s in specs])
        w.provider_index = 0 if specs else -1
        w.wrap_available = wrap_ok
        if not wrap_ok:
            w.wrap_phase = False
        self._populate_bench_configs(specs)
        self.set_busy(False, "Ready.")
        self.logline("Environment check complete: " +
                     ", ".join(f"{c}={'ok' if s in (OK, INFO) else s}"
                               for s, c, _ in rows if c in ("onnxruntime", "espeak-ng", "Kokoro model")))

    def _populate_bench_configs(self, specs):
        # Every available EP, then CPU at 1 / 2 / half / all logical cores (x-half and x-all
        # bracket the physical-core count, which is where the hyperthreading penalty shows).
        ncpu = os.cpu_count() or 8
        half = max(1, ncpu // 2)
        entries = [(s, True) for s in specs]
        for n in sorted({1, 2, half, ncpu}):
            entries.append((km.ProviderSpec(f"CPU x{n}", ["CPUExecutionProvider"], intra_op=n),
                            n in (1, half, ncpu)))
        self.cfg_specs = [s for s, _ in entries]
        self.cfg_checked = [c for _, c in entries]
        self.cfg_model = slint.ListModel([{"label": s.label, "checked": c} for s, c in entries])
        self.w.bench_configs = self.cfg_model

    def on_config_toggled(self, i: int, checked: bool):
        if 0 <= i < len(self.cfg_checked):
            self.cfg_checked[i] = checked
            self.cfg_model[i] = {"label": self.cfg_specs[i].label, "checked": checked}

    # ---- synthesize ---------------------------------------------------------
    def on_synth(self):
        if self.busy:
            return
        w = self.w
        text = w.synth_text.strip()
        if not text:
            self.logline("Nothing to synthesize.")
            return
        idx = w.provider_index
        label = self.specs[idx].label if 0 <= idx < len(self.specs) else "CPU"
        speed = round(float(w.speed), 2)
        wrap = bool(w.wrap_phase)
        self.set_busy(True, f"Synthesizing on {label}{' +wrap' if wrap else ''}…")
        w.has_audio = False
        threading.Thread(target=self._do_synth, args=(text, label, speed, wrap), daemon=True).start()

    def _announce_patch(self, synth: km.Synth, wrap: bool):
        """Patching reloads and re-serializes the 325 MB model: seconds, once per run."""
        if wrap and not synth.wrap_ready:
            def show():
                self.w.status = "Patching model (wrap sine phase)…"
                self.logline("Patching model: wrapping the sine phase before " + km.SINE_SIN_NODE)
            self.post(show)

    def _do_synth(self, text, label, speed, wrap):
        try:
            synth = self._ensure_synth()
            spec = replace(self._spec_by_label(label), wrap_phase=wrap)
            self._announce_patch(synth, wrap)
            r = synth.synth(text, spec, speed=speed)
            self.last_pcm = r.pcm
            self.post(lambda: self._show_synth(r))
            self._play(r.pcm, r.sample_rate)
        except Exception as e:
            err = e
            self.post(lambda: (self.logline(f"[synth error] {err!r}"),
                               self.set_busy(False, "Error — see log.")))

    def _show_synth(self, r: km.SynthResult):
        peak = float(np.max(np.abs(r.pcm))) if r.pcm.size else 0.0
        w = self.w
        w.synth_stats = (f"{r.provider}:  {r.n_tokens} tokens ({r.n_windows} window"
                         f"{'s' if r.n_windows != 1 else ''})  ·  {r.audio_seconds:.2f}s audio  ·  "
                         f"synth {r.synth_seconds:.3f}s  ·  RTF {r.realtime_factor:.2f}x  ·  peak {peak:.2f}")
        w.phonemes = r.phonemes
        w.has_audio = True
        self.logline(f"Synth {r.provider}: {r.audio_seconds:.2f}s audio in "
                     f"{r.synth_seconds:.3f}s (RTF {r.realtime_factor:.2f}x)")
        self.set_busy(False, "Ready.")

    # ---- benchmark ----------------------------------------------------------
    def on_bench(self):
        if self.busy:
            return
        w = self.w
        chosen = [s for s, c in zip(self.cfg_specs, self.cfg_checked) if c]
        if not chosen:
            self.logline("Select at least one configuration.")
            return
        text = w.synth_text.strip() or DEFAULT_TEXT
        runs, warmup = int(w.runs), int(w.warmup)
        wrap = bool(w.wrap_phase)
        chosen = [replace(s, wrap_phase=wrap) for s in chosen]
        w.bench_rows = slint.ListModel([])
        self.set_busy(True, "Benchmarking…")
        threading.Thread(target=self._do_bench, args=(text, chosen, runs, warmup), daemon=True).start()

    def _do_bench(self, text, chosen, runs, warmup):
        try:
            synth = self._ensure_synth()
            phon = synth.phonemes_for(text)
            n_tok = len(km.tokenize(phon, synth.vocab))
            self.post(lambda: self.logline(
                f"Benchmark: {len(chosen)} configs × {runs} runs (+{warmup} warm-up), {n_tok} tokens"))
            self._announce_patch(synth, any(s.wrap_phase for s in chosen))
            results = []
            total = len(chosen)
            for idx, spec in enumerate(chosen):
                self.post(lambda i=idx, l=spec.display:
                          setattr(self.w, "status", f"Benchmarking {l}  ({i+1}/{total})…"))
                b = synth.bench(spec, phon, runs=runs, warmup=warmup)
                results.append(b)
                self.post(lambda l=spec.display, bb=b: self.logline(
                    f"  {l}: median {bb['median']*1000:.0f} ms, RTF {bb['rtf_median']:.2f}x"))
            self.post(lambda: self._show_bench(results))
        except Exception as e:
            err = e
            self.post(lambda: (self.logline(f"[bench error] {err!r}"),
                               self.set_busy(False, "Error — see log.")))

    def _show_bench(self, results):
        baseline = max(b["median"] for b in results)        # slowest = 1.00x
        self.w.bench_rows = slint.ListModel([{
            "config": b["label"], "runs": str(b["runs"]),
            "median": f"{b['median']*1000:.0f}", "p90": f"{b['p90']*1000:.0f}",
            "min": f"{b['min']*1000:.0f}", "rtf": f"{b['rtf_median']:.2f}x",
            "rel": f"{baseline/b['median']:.2f}x",
        } for b in sorted(results, key=lambda r: r["median"])])
        best = min(results, key=lambda r: r["median"])
        self.set_busy(False, f"Done. Fastest: {best['label']} "
                             f"({best['median']*1000:.0f} ms, RTF {best['rtf_median']:.2f}x).")

    # ---- audio playback ------------------------------------------------------
    def _play(self, pcm, sr):
        """Called on the synth worker: write a temp WAV and hand it to the system player."""
        player = _find_player()
        if not player:
            self.post(lambda: setattr(self.w, "status", "Ready (no audio player)."))
            return
        path = os.path.join(tempfile.gettempdir(), "kokoro_stack_check_play.wav")
        km.write_wav(path, pcm, sr)
        self._stop_player()
        proc = subprocess.Popen([player, path], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.player = proc

        def started():
            self.w.playing = True
            self.w.status = "Playing…"
        self.post(started)
        threading.Thread(target=lambda: (proc.wait(), self.post(lambda: self._player_done(proc))),
                         daemon=True).start()

    def _player_done(self, proc):
        if self.player is proc:          # a newer clip may have replaced this one
            self.player = None
            self.w.playing = False
            if not self.busy:
                self.w.status = "Ready."

    def _stop_player(self):
        p = self.player
        if p and p.poll() is None:
            try:
                p.terminate()
            except Exception:
                pass
        self.player = None

    def on_stop(self):
        self._stop_player()
        self.w.playing = False
        self.w.status = "Stopped."

    def on_save(self):
        # Slint has no file dialog, so the path comes from the text field beside the button.
        if self.last_pcm is None:
            return
        p = Path(self.w.save_path.strip()).expanduser()
        if not p.parent.is_dir():
            self.logline(f"Save failed: no such folder {p.parent}")
            return
        try:
            km.write_wav(p, self.last_pcm, km.SAMPLE_RATE)
        except OSError as e:
            self.logline(f"Save failed: {e}")
            return
        self.w.save_path = str(p)
        self.logline(f"Saved {p}")


def _collect_garbage():
    """Cyclic GC, on the UI thread only -- the one thread allowed to free Slint structs."""
    gc.collect()


# ---- environment helpers ----------------------------------------------------
def _cpu_name() -> str:
    try:
        for line in open("/proc/cpuinfo"):
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except Exception:
        pass
    return platform.processor() or platform.machine()


def _gpu_name() -> str | None:
    lspci = shutil.which("lspci")
    if not lspci:
        return None
    try:
        out = subprocess.run([lspci], capture_output=True, text=True, timeout=4).stdout
        for line in out.splitlines():
            if any(k in line.lower() for k in ("vga", "3d", "display")):
                return line.split(":", 2)[-1].strip()
    except Exception:
        pass
    return None


def _find_player() -> str | None:
    for p in ("pw-play", "paplay", "aplay", "ffplay"):
        path = shutil.which(p)
        if path:
            return path
    return None


def main():
    App().run()


if __name__ == "__main__":
    main()
