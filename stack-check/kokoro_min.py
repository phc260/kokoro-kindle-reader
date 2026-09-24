"""Minimal, faithful-enough Kokoro synthesis core for the stack check.

This is NOT a port of the production kokoro-kindle-reader pipeline. It exercises the
same *tech stack* end to end so we can answer two questions on this machine:

  1. Does it work?  espeak-ng (FFI) -> phonemes -> Kokoro vocab tokens -> ONNX Runtime
     -> 24 kHz waveform.
  2. How fast is it, CPU vs GPU/WebGPU?

Deliberately skipped vs production (not needed for a stack/speed check): number &
currency normalization (espeak reads digits on its own), punctuation-segment
reassembly, the word-timing / span machinery. Kept: the handful of phoneme
substitutions that decide whether a phoneme lands in Kokoro's vocab at all -- without
`r -> ɹ` etc. those tokens are silently dropped and the audio degrades.

Model I/O matches kokoro-host/src/native_synth.rs:
  inputs  input_ids|tokens : int64 [1, N]  (BOS=0 ... EOS=0)
          style|ref_s      : f32   [1, 256] (voice row = clamp(n_content, 0, 509))
          speed            : f32   [1]
  output  waveform         : f32   @ 24000 Hz
Voice .bin is 510 x 256 float32 (VOICE_ROWS x STYLE_DIM).
"""

from __future__ import annotations

import ctypes
import json
import os
import tempfile
import time
import wave
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np

# --- constants (mirrors native_synth.rs) ------------------------------------
SAMPLE_RATE = 24000
STYLE_DIM = 256
VOICE_ROWS = 510
MAX_CONTENT_TOKENS = 500  # Kokoro's BERT Expand node fails past ~510 tokens
BOS = 0
EOS = 0

WINDOWS = os.name == "nt"


def app_data_dir() -> Path:
    """Mirrors native-deps/fetch-model.py's app_data_dir() (and so kokoro-host's): Windows
    keeps the reverse-DNS identifier under %APPDATA%, Linux a plainly-named XDG data dir."""
    if WINDOWS:
        return Path(os.environ.get("APPDATA", "")) / "com.phc260.kokoro-kindle-reader"
    xdg = os.environ.get("XDG_DATA_HOME")
    return (Path(xdg) if xdg else Path.home() / ".local" / "share") / "kokoro-kindle-reader"


DEFAULT_MODEL_DIR = app_data_dir() / "onnx-community" / "Kokoro-82M-v1.0-ONNX"
# repo root = parent of this file's directory (stack-check/)
REPO_ROOT = Path(__file__).resolve().parent.parent
# fetch-deps.py keeps one runtime tree per platform: native-deps/windows/runtime/ holds
# espeak-ng.dll, native-deps/linux/runtime/ libespeak-ng.so*; espeak-ng-data/ sits beside
# the library in both.
DEFAULT_ESPEAK_RUNTIME = REPO_ROOT / "native-deps" / ("windows" if WINDOWS else "linux") / "runtime"


# --- espeak-ng over ctypes ---------------------------------------------------
# speak_lib.h / espeak_ng.h constants
_ENOUTPUT_MODE_SYNCHRONOUS = 0x0001
_ESPEAK_CHARS_UTF8 = 1
_ENS_OK = 0
_EE_OK = 0
# espeak_TextToPhonemes phoneme_mode: bit 1 (value 2) = IPA (UTF-8), no separator char.
_PHONEMEMODE_IPA = 0x02


class Espeak:
    """One-time espeak-ng init + phonemization. espeak keeps global state and is not
    thread-safe -- construct exactly one and phonemize from a single thread."""

    def __init__(self, runtime_dir: Path):
        runtime_dir = Path(runtime_dir)
        lib_path = _find_espeak_lib(runtime_dir)
        if lib_path is None:
            raise FileNotFoundError(f"{ESPEAK_LIB_NAME} not found under {runtime_dir}")
        self.lib_path = lib_path
        self.data_parent = str(runtime_dir)  # dir that CONTAINS espeak-ng-data/

        if WINDOWS:
            # A full path loads the DLL itself; this lets the loader find anything it
            # depends on in the same folder, as it would beside kokoro-host.exe.
            self._dll_dir = os.add_dll_directory(str(lib_path.parent))
        lib = ctypes.CDLL(str(lib_path))
        lib.espeak_ng_InitializePath.argtypes = [ctypes.c_char_p]
        lib.espeak_ng_InitializePath.restype = None
        lib.espeak_ng_Initialize.argtypes = [ctypes.POINTER(ctypes.c_void_p)]
        lib.espeak_ng_Initialize.restype = ctypes.c_int
        lib.espeak_ng_InitializeOutput.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_char_p]
        lib.espeak_ng_InitializeOutput.restype = ctypes.c_int
        lib.espeak_SetVoiceByName.argtypes = [ctypes.c_char_p]
        lib.espeak_SetVoiceByName.restype = ctypes.c_int
        lib.espeak_TextToPhonemes.argtypes = [
            ctypes.POINTER(ctypes.c_void_p), ctypes.c_int, ctypes.c_int
        ]
        lib.espeak_TextToPhonemes.restype = ctypes.c_char_p
        lib.espeak_Info.argtypes = [ctypes.c_void_p]
        lib.espeak_Info.restype = ctypes.c_char_p
        self.lib = lib

        lib.espeak_ng_InitializePath(self.data_parent.encode("utf-8"))
        ctx = ctypes.c_void_p()
        if lib.espeak_ng_Initialize(ctypes.byref(ctx)) != _ENS_OK:
            raise RuntimeError("espeak_ng_Initialize failed")
        if lib.espeak_ng_InitializeOutput(_ENOUTPUT_MODE_SYNCHRONOUS, 0, None) != _ENS_OK:
            raise RuntimeError("espeak_ng_InitializeOutput failed")
        if lib.espeak_SetVoiceByName(b"en-us") != _EE_OK:
            raise RuntimeError("espeak_SetVoiceByName(en-us) failed")

    def version(self) -> str:
        """The library's own version string. Asked of espeak rather than read off the file
        name, which carries it on Linux (libespeak-ng.so.1.52.0) but not on Windows."""
        v = self.lib.espeak_Info(None)
        return v.decode("utf-8", "replace") if v else "unknown"

    def phonemize(self, text: str) -> str:
        """espeak IPA phonemes for `text`, clauses joined with a single space.

        Mirrors the trace fold in espeak.rs: espeak_TextToPhonemes returns one clause
        per call and advances the text pointer; we join the clauses with spaces."""
        buf = ctypes.create_string_buffer(text.encode("utf-8") + b"\x00")
        text_ptr = ctypes.cast(buf, ctypes.c_void_p)
        clauses: list[str] = []
        # guard against a pathological non-advancing call
        for _ in range(100000):
            if not text_ptr.value:
                break
            out = self.lib.espeak_TextToPhonemes(
                ctypes.byref(text_ptr), _ESPEAK_CHARS_UTF8, _PHONEMEMODE_IPA
            )
            if out:
                clauses.append(out.decode("utf-8", "replace"))
        return " ".join(c.strip() for c in clauses if c.strip())


ESPEAK_LIB_NAME = "espeak-ng.dll" if WINDOWS else "libespeak-ng"


def _find_espeak_lib(runtime_dir: Path) -> Path | None:
    if WINDOWS:
        p = runtime_dir / "espeak-ng.dll"
        return p if p.is_file() else None
    # Prefer the concrete versioned file, fall back to the symlink/soname.
    cands = sorted(runtime_dir.glob("libespeak-ng.so.*.*.*"))
    for c in cands:
        if c.is_file() and not c.is_symlink():
            return c
    for name in ("libespeak-ng.so.1", "libespeak-ng.so"):
        p = runtime_dir / name
        if p.exists():
            return p
    return None


# --- phoneme post-processing (essential subset of text.rs::post_process) -----
# Order matters. The two "kokoro" fixes come first, then the vocab-critical
# single-symbol substitutions. The contextual rules (hundred spacing, ninety
# ti->di, terminal " z"->"z") are omitted -- minor and lookbehind-only.
_PHONEME_SUBS: list[tuple[str, str]] = [
    ("kəkˈoːɹoʊ", "kˈoʊkəɹoʊ"),
    ("kəkˈɔːɹəʊ", "kˈəʊkəɹəʊ"),
    ("ʲ", "j"),
    ("r", "ɹ"),
    ("x", "k"),
    ("ɬ", "l"),
]


def post_process_phonemes(phon: str) -> str:
    for frm, to in _PHONEME_SUBS:
        phon = phon.replace(frm, to)
    return phon


# --- tokenizer ---------------------------------------------------------------
def load_vocab(tokenizer_json: Path) -> dict[str, int]:
    """tokenizer.json model.vocab: phoneme-char -> id."""
    d = json.loads(Path(tokenizer_json).read_text(encoding="utf-8"))
    model = d.get("model")
    vocab = model.get("vocab") if isinstance(model, dict) else None
    if not vocab:
        vocab = d.get("vocab")
    if not vocab:
        raise ValueError("no model.vocab in tokenizer.json")
    return {k: int(v) for k, v in vocab.items()}


def tokenize(phonemes: str, vocab: dict[str, int]) -> list[int]:
    """Per-codepoint vocab lookup; a phoneme char not in the vocab produces no token
    (matches native_synth.rs::tokenize_spans). No BOS/EOS here -- added per window."""
    return [vocab[ch] for ch in phonemes if ch in vocab]


# --- voice / style -----------------------------------------------------------
def load_voice(path: Path) -> np.ndarray:
    raw = Path(path).read_bytes()
    if len(raw) != VOICE_ROWS * STYLE_DIM * 4:
        raise ValueError(
            f"voice {path} is {len(raw)} bytes, expected {VOICE_ROWS * STYLE_DIM * 4}"
        )
    return np.frombuffer(raw, dtype="<f4").reshape(VOICE_ROWS, STYLE_DIM).copy()


def style_row(voice: np.ndarray, n_content_tokens: int) -> np.ndarray:
    # style row = clamp(nContentTokens, 0, 509)  (kokoro-js generate_from_ids)
    row = max(0, min(n_content_tokens, VOICE_ROWS - 1))
    return voice[row]


# --- synthesis ---------------------------------------------------------------
@dataclass
class ProviderSpec:
    """One execution-provider configuration to build a session from."""
    label: str
    providers: list[str]
    options: list | None = None
    intra_op: int | None = None   # CPU intra-op thread count, None = ORT default
    wrap_phase: bool = False      # run the model with wrap_sine_phase() applied

    @property
    def key(self) -> str:
        """Session-cache key: the same EP with and without the wrap is a different graph."""
        return f"{self.label}|wrap" if self.wrap_phase else self.label

    @property
    def display(self) -> str:
        return f"{self.label} +wrap" if self.wrap_phase else self.label


# --- the WebGPU Sin-range fix ---------------------------------------------------
# Kokoro's harmonic source (SineGen) feeds this Sin an UNWRAPPED cumulative phase: ~2e5 rad
# after ~6 s of audio. WebGPU's Sin is only required to be accurate in [-pi, pi] (WGSL), and on
# the reference Iris Xe it returns garbage past |x| ~ 102,839 (~ 32768*pi) -- so the tail of any
# model run longer than ~3.6 s loses its harmonic excitation and sounds muffled. CPU Sin is
# accurate at any magnitude, which is why the two EPs disagree only on longer utterances.
SINE_SIN_NODE = "/decoder/decoder/generator/m_source/l_sin_gen/Sin"


def wrap_available() -> bool:
    """The wrap edits the graph with the `onnx` package; without it the toggle is unavailable."""
    try:
        import onnx  # noqa: F401
        return True
    except Exception:
        return False


def wrap_sine_phase(model_path: Path) -> bytes:
    """Model bytes with the harmonic source's phase wrapped into [0, 2*pi) before its Sin:
    x - 2*pi*floor(x / 2*pi), four elementwise nodes. Every Sin implementation is accurate
    in that range, so WebGPU then matches CPU; on CPU the change is ~fp32 rounding (measured
    corr 0.9995 against the unwrapped graph). An off-by-one floor near a period boundary only
    shifts the argument by exactly 2*pi, which Sin cannot see."""
    import onnx
    from onnx import helper, numpy_helper

    m = onnx.load(str(model_path))
    g = m.graph
    idx = next((i for i, n in enumerate(g.node) if n.name == SINE_SIN_NODE), None)
    if idx is None:
        raise ValueError(f"{SINE_SIN_NODE} not found -- not the expected Kokoro graph")
    src = g.node[idx].input[0]
    g.initializer.extend([
        numpy_helper.from_array(np.array(1.0 / (2.0 * np.pi), np.float32), "sine_wrap/inv_2pi"),
        numpy_helper.from_array(np.array(2.0 * np.pi, np.float32), "sine_wrap/two_pi"),
    ])
    wrap = [
        helper.make_node("Mul", [src, "sine_wrap/inv_2pi"], ["sine_wrap/cycles"], name="sine_wrap/Mul"),
        helper.make_node("Floor", ["sine_wrap/cycles"], ["sine_wrap/whole"], name="sine_wrap/Floor"),
        helper.make_node("Mul", ["sine_wrap/whole", "sine_wrap/two_pi"], ["sine_wrap/offset"],
                         name="sine_wrap/Mul_1"),
        helper.make_node("Sub", [src, "sine_wrap/offset"], ["sine_wrap/phase"], name="sine_wrap/Sub"),
    ]
    for k, node in enumerate(wrap):          # keep the node list topologically sorted
        g.node.insert(idx + k, node)
    g.node[idx + len(wrap)].input[0] = "sine_wrap/phase"
    return m.SerializeToString()


@dataclass
class SynthResult:
    pcm: np.ndarray                # float32, mono
    sample_rate: int
    phonemes: str
    n_tokens: int                  # content tokens (excl. BOS/EOS), summed over windows
    n_windows: int
    synth_seconds: float           # wall time of session.run() calls only
    audio_seconds: float
    realtime_factor: float         # audio_seconds / synth_seconds (>1 = faster than RT)
    provider: str


@dataclass
class Synth:
    """Holds one ONNX Runtime session per provider label, plus the vocab and voice."""

    model_dir: Path
    espeak: Espeak
    vocab: dict[str, int] = field(init=False)
    voice: np.ndarray = field(init=False)
    _sessions: dict[str, object] = field(default_factory=dict, init=False)
    _io: dict[str, tuple[list[str], str]] = field(default_factory=dict, init=False)
    _wrapped_bytes: bytes | None = field(default=None, init=False)   # patched model, built once

    def __post_init__(self):
        self.model_dir = Path(self.model_dir)
        self.vocab = load_vocab(self.model_dir / "tokenizer.json")
        self.voice = load_voice(self.model_dir / "voices" / "af_heart.bin")

    @property
    def model_path(self) -> Path:
        return self.model_dir / "onnx" / "model.onnx"

    @property
    def wrap_ready(self) -> bool:
        """True once the patched model has been built (so a wrapped session starts fast)."""
        return self._wrapped_bytes is not None

    def session(self, spec: "ProviderSpec"):
        """Build (or return cached) session for `spec`, cached by spec.key. Sessions of the
        other wrap state are dropped first: each holds a full copy of the weights, so keeping
        both sets alive would double memory for no benefit."""
        import onnxruntime as ort

        if spec.key in self._sessions:
            return self._sessions[spec.key]
        for k in [k for k in self._sessions if k.endswith("|wrap") != spec.wrap_phase]:
            del self._sessions[k]
            self._io.pop(k, None)
        model: str | bytes = str(self.model_path)
        if spec.wrap_phase:
            if self._wrapped_bytes is None:
                self._wrapped_bytes = wrap_sine_phase(self.model_path)
            model = self._wrapped_bytes
        so = ort.SessionOptions()
        so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        if spec.intra_op is not None:
            so.intra_op_num_threads = spec.intra_op
        sess = ort.InferenceSession(
            model,
            sess_options=so,
            providers=spec.providers,
            provider_options=spec.options,
        )
        in_names = [i.name for i in sess.get_inputs()]
        out_names = [o.name for o in sess.get_outputs()]
        wf = next((n for n in out_names if n == "waveform"), out_names[0])
        self._sessions[spec.key] = sess
        self._io[spec.key] = (in_names, wf)
        return sess

    def phonemes_for(self, text: str) -> str:
        return post_process_phonemes(self.espeak.phonemize(text))

    def _windows(self, content: list[int]) -> list[list[int]]:
        if not content:
            return []
        return [content[i:i + MAX_CONTENT_TOKENS]
                for i in range(0, len(content), MAX_CONTENT_TOKENS)]

    def _run_window(self, sess, in_names, wf, window: list[int], speed: float) -> np.ndarray:
        ids = np.array([[BOS] + window + [EOS]], dtype=np.int64)
        style = style_row(self.voice, len(window)).reshape(1, STYLE_DIM).astype(np.float32)
        feeds = {}
        for name in in_names:
            if name in ("input_ids", "tokens"):
                feeds[name] = ids
            elif name in ("style", "ref_s"):
                feeds[name] = style
            else:
                feeds[name] = np.array([speed], dtype=np.float32)
        out = sess.run([wf], feeds)[0]
        return np.asarray(out, dtype=np.float32).reshape(-1)

    def synth(self, text: str, spec: "ProviderSpec", speed: float = 1.0,
              phonemes: str | None = None) -> SynthResult:
        sess = self.session(spec)
        in_names, wf = self._io[spec.key]
        if phonemes is None:
            phonemes = self.phonemes_for(text)
        content = tokenize(phonemes, self.vocab)
        windows = self._windows(content)
        parts: list[np.ndarray] = []
        t0 = time.perf_counter()
        for w in windows:
            parts.append(self._run_window(sess, in_names, wf, w, speed))
        synth_seconds = time.perf_counter() - t0
        pcm = np.concatenate(parts) if parts else np.zeros(0, dtype=np.float32)
        audio_seconds = len(pcm) / SAMPLE_RATE
        return SynthResult(
            pcm=pcm, sample_rate=SAMPLE_RATE, phonemes=phonemes,
            n_tokens=len(content), n_windows=len(windows),
            synth_seconds=synth_seconds, audio_seconds=audio_seconds,
            realtime_factor=(audio_seconds / synth_seconds if synth_seconds > 0 else 0.0),
            provider=spec.display,
        )

    def bench(self, spec: "ProviderSpec", phonemes: str, speed: float = 1.0,
              runs: int = 5, warmup: int = 1, progress=None):
        """Time `runs` full-page syntheses of the same phonemes on `spec`. Tokenization
        and windowing happen once; only session.run() is timed. `progress(done, total)`
        is called after each timed run. Returns a dict of stats + one PCM sample."""
        sess = self.session(spec)
        in_names, wf = self._io[spec.key]
        content = tokenize(phonemes, self.vocab)
        windows = self._windows(content)

        def one():
            parts = [self._run_window(sess, in_names, wf, w, speed) for w in windows]
            return np.concatenate(parts) if parts else np.zeros(0, dtype=np.float32)

        for _ in range(max(0, warmup)):
            one()
        times: list[float] = []
        pcm = np.zeros(0, dtype=np.float32)
        for i in range(max(1, runs)):
            t0 = time.perf_counter()
            pcm = one()
            times.append(time.perf_counter() - t0)
            if progress:
                progress(i + 1, max(1, runs))
        times.sort()
        audio_seconds = len(pcm) / SAMPLE_RATE

        def pct(p):
            k = min(len(times) - 1, int(round((p / 100.0) * (len(times) - 1))))
            return times[k]

        median = times[len(times) // 2]
        return {
            "label": spec.display,
            "n_tokens": len(content),
            "n_windows": len(windows),
            "audio_seconds": audio_seconds,
            "runs": len(times),
            "min": times[0],
            "median": median,
            "p90": pct(90),
            "max": times[-1],
            "rtf_median": (audio_seconds / median if median > 0 else 0.0),
            "pcm": pcm,
        }


# --- WAV out -----------------------------------------------------------------
def write_wav(path: Path, pcm: np.ndarray, sample_rate: int = SAMPLE_RATE):
    clipped = np.clip(pcm, -1.0, 1.0)
    ints = (clipped * 32767.0).astype("<i2")
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sample_rate)
        w.writeframes(ints.tobytes())


# --- provider discovery ------------------------------------------------------
def available_provider_specs() -> list[ProviderSpec]:
    """Every EP we can use on this machine. CPU is always present; WebGPU / CUDA /
    DirectML appear only if the installed onnxruntime wheel built them in."""
    import onnxruntime as ort

    avail = ort.get_available_providers()
    specs: list[ProviderSpec] = []
    # Production's GPU path is the WebGPU EP (Dawn; D3D12 on Windows, Vulkan on Linux). It
    # shows up only with the onnxruntime-webgpu wheel -- the plain onnxruntime wheel does not
    # ship it. GPU specs list CPU second so any node the GPU EP can't run falls back instead of failing the build.
    if "WebGpuExecutionProvider" in avail:
        specs.append(ProviderSpec("WebGPU", ["WebGpuExecutionProvider", "CPUExecutionProvider"]))
    if "CUDAExecutionProvider" in avail:
        specs.append(ProviderSpec("CUDA", ["CUDAExecutionProvider", "CPUExecutionProvider"]))
    if "DmlExecutionProvider" in avail:
        specs.append(ProviderSpec("DirectML", ["DmlExecutionProvider", "CPUExecutionProvider"]))
    specs.append(ProviderSpec("CPU", ["CPUExecutionProvider"]))
    return specs


def cpu_thread_specs(counts: list[int]) -> list[ProviderSpec]:
    """CPU sessions pinned to specific intra-op thread counts, for a scaling test."""
    return [ProviderSpec(f"CPU x{n}", ["CPUExecutionProvider"], intra_op=n) for n in counts]


if __name__ == "__main__":
    import sys

    text = sys.argv[1] if len(sys.argv) > 1 else "The quick brown fox jumps over the lazy dog."
    esp = Espeak(DEFAULT_ESPEAK_RUNTIME)
    print("espeak lib :", esp.lib_path, "version", esp.version())
    synth = Synth(DEFAULT_MODEL_DIR, esp)
    print("model      :", synth.model_path)
    print("providers  :", [s.label for s in available_provider_specs()])
    ph = synth.phonemes_for(text)
    print("text       :", text)
    print("phonemes   :", ph)
    r = synth.synth(text, ProviderSpec("CPU", ["CPUExecutionProvider"]), phonemes=ph)
    print(f"tokens     : {r.n_tokens} content ({r.n_windows} window(s))")
    print(f"audio      : {r.audio_seconds:.2f}s  synth {r.synth_seconds:.3f}s  "
          f"RTF {r.realtime_factor:.2f}x  peak {float(np.max(np.abs(r.pcm))):.3f}")
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else Path(tempfile.gettempdir()) / "kokoro_stack_check.wav"
    write_wav(out, r.pcm, r.sample_rate)
    print("wav        :", out)
