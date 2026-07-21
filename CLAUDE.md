# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Local, offline Kokoro-82M text-to-speech synthesized **natively on the Dawn WebGPU**
execution provider of ONNX Runtime (the same Dawn as Chrome/WebView2) — **no WebView2,
no browser, no C++**. The synth core is pure Rust: the `ort` crate drives the model on
the WebGPU EP, and espeak-ng is reached over a thin FFI. Two x64 exes, plus three x86
artifacts Kindle loads in-process:

1. **`kokoro-host.exe`** (x64) — windowless system-tray daemon. Owns the named pipe,
   synthesizes, reads `controls.json` live, runs the Kindle-watcher. The only thing that
   produces audio. Auto-starts hidden at login.
2. **`kokoro-panel.exe`** — native Slint settings panel, spawned on demand from the tray.
   Narrator/speed/volume, Preview, model download/verify, Kindle-narration toggle.
3. **`KokoroSapi.dll`** (x86) — thin connect-only COM shim Kindle loads in-process; forwards
   each `Speak` over the pipe to the host.
4. **`kokoro_hook.dll` + `kokoro-inject.exe`** (x86) — make Kindle for PC 1.0.18632.0+ narrate
   with Kokoro by patching the `ISpVoice::SetVoice` vtable slot.

```
Kindle.exe (x86) ──in-proc COM (LoadLibrary + vtable)──▶ KokoroSapi.dll (x86 shim)
                                                            │ named pipe \\.\pipe\KokoroSapiSynth
                                                            ▼
      kokoro-host.exe (x64, tray): pipe.rs ──▶ native_synth.rs (Rust synth:
                                       ▲          text.rs + espeak.rs + ort/Dawn WebGPU EP)
        reads live ── controls.json ──┘        ▲ spawns "Settings"
                          ▲                     │
      kokoro-panel.exe (Slint) writes ─────────┘
```

All audio comes from the native synth in `kokoro-host`; the SAPI engine synthesizes
nothing. **Consequence: `kokoro-host` must be running for Kindle to speak** — and it also
injects the hook, so one running host both hooks Kindle and serves its audio.

## Where the details live

This file carries only the **invariants** — the things that are expensive to rediscover.
Load the detail on demand:

| Need | Read |
|---|---|
| The engine chain end to end, streaming/pacing model, repo layout table, build-from-source | [`ARCHITECTURE.md`](ARCHITECTURE.md) |
| Contributor workflow, the two-model review split + `Reviewed-by:` convention, CI table, release/tagging steps | [`DEVELOPMENT.md`](DEVELOPMENT.md) |
| Installer internals: NSIS build, elevation flow, ACL staging, uninstall | [`packaging/README.md`](packaging/README.md) |
| Tray host + synth core internals (per-file layout) | [`kokoro-host/README.md`](kokoro-host/README.md) |
| Settings panel internals | [`kokoro-panel/README.md`](kokoro-panel/README.md) |
| SAPI engine: COM exports, interfaces, dev registration, smoke tests | [`kokoro-sapi/README.md`](kokoro-sapi/README.md) · [`kokoro-sapi-smoke/README.md`](kokoro-sapi-smoke/README.md) |
| Pipe wire format (the single source of truth) | [`kokoro-protocol/README.md`](kokoro-protocol/README.md) + the crate itself |
| Kindle 18632 hook + injector | [`kokoro-hook/README.md`](kokoro-hook/README.md) · [`kokoro-inject/README.md`](kokoro-inject/README.md) |
| Dep provisioning (ORT/Dawn DLLs, espeak-ng) | [`native-deps/README.md`](native-deps/README.md) |
| GPU-vs-CPU synth timings + settled perf dead ends | [`kokoro-bench/README.md`](kokoro-bench/README.md) |
| User-facing install/usage | [`README.md`](README.md) |
| Codex's copy of these instructions (reviewer role + constraints) | [`AGENTS.md`](AGENTS.md) — keep its invariant list in sync with this file |

## Commands

```powershell
# One-time: provision the synth runtime deps (Dawn ORT runtime DLLs + espeak-ng x64
# import lib/DLL + espeak-ng-data). Must run before building kokoro-host.
native-deps\fetch-deps.ps1

# Build + run (Rust, x64). Right-click the tray → Settings to open the panel.
cargo run --manifest-path kokoro-host\Cargo.toml     # windowless tray daemon
cargo run --manifest-path kokoro-panel\Cargo.toml    # settings panel (or via the tray)

# SAPI engine — x86 Rust cdylib, no deps (thin COM shim + pipe client).
cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-sapi\Cargo.toml

# Kindle 18632 hook + injector — both x86 (Kindle is 32-bit; the host spawns the injector).
cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-hook\Cargo.toml
cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-inject\Cargo.toml
# Kindle-free check that the SetVoice vtable index is still 18 (needs Kokoro registered):
cargo run --release --target i686-pc-windows-msvc --manifest-path kokoro-hook\Cargo.toml --bin selftest

# Register the voice — DEV path (elevated; MUST be the 32-bit regsvr32). Same DLL path =
# registration survives rebuilds. The packaged installer does this automatically.
C:\Windows\SysWOW64\regsvr32.exe "kokoro-sapi\target\i686-pc-windows-msvc\release\KokoroSapi.dll"

# Packaged installer — builds the x86 DLL + release-builds both crates, stages everything,
# then runs makensis. NSIS. See packaging/README.md.
packaging\build-installer.ps1
# CI does this on a v* tag (.github/workflows/installer.yml); sapi.yml
# builds the x86 DLL + runs the COM smoke test on kokoro-sapi/** changes; hook.yml
# compile-checks the x86 hook + injector on kokoro-hook/** / kokoro-inject/** changes.

# SAPI smoke test — no Kindle, no elevation: LoadLibrary the DLL + drive the COM object
# model + Speak path (needs the host running for audio). See kokoro-sapi-smoke/.
cargo run --release --target i686-pc-windows-msvc --manifest-path kokoro-sapi-smoke\Cargo.toml
# Or the SAPI-registered path (32-BIT PowerShell, host running, DLL registered):
C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe -File kokoro-sapi\test-speak.ps1
```

No Rust test suites except `text.rs`'s golden normalization tests; "testing" is Preview in
the panel and Read Aloud in Kindle (or `test-speak.ps1`).

## Gotchas / invariants (do not rediscover these)

### Runtime / synth
- **`kokoro-host` must be running** or Kindle gets no audio (the engine's `Speak` returns
  `E_FAIL` when the pipe is absent — no fallback). It's a windowless tray daemon that
  **auto-starts hidden at login** (`auto-launch`, `--hidden`); Quit is only via the tray
  menu. Closing the settings panel does **not** stop the host. This also fixes Kindle
  **fast-scrolling** when the host is gone mid-Read-Aloud: a mid-session pipe disconnect
  makes each per-page `Speak` fail instantly, which Kindle reads as "page done" and races
  through the book — so keep the host alive.
- **Native synth is serialized.** espeak has global state + isn't thread-safe (and the
  `ort` session is owned by the worker), so ONE dedicated thread owns the synth; never
  call espeak / run the session from multiple threads.
- **The model fails the BERT `Expand` node past ~510 tokens.** A chunk's tokens are
  sub-split into `MAX_CONTENT_TOKENS`(=500) windows, each wrapped in its own BOS/EOS, and
  the PCM concatenated (`native_synth.rs`). A run also retries a couple of times —
  rebuilding the session on the last try — to ride out a transient Dawn device error.
- **Kindle 18632's narrator is event-driven.** `engine.rs` must report
  `SPEI_WORD_BOUNDARY` / `SPEI_SENTENCE_BOUNDARY` / `SPEI_TTS_BOOKMARK` at true
  audio-stream offsets, which is what the `CHUNK_INFO` frame (`0xFFFF_FFFD` + the chunk's
  UTF-16 span + sample count) exists to make possible. Without those events Kindle speaks
  the first sentence of a page and never advances.
- **`fetch-deps.ps1` must run before building `kokoro-host`.** `build.rs` panics if the
  provisioned dep folders under `native-deps/` (ORT + Dawn DLLs + espeak) are missing.
  It also stages the 5 runtime DLLs next to the exe.

### `controls.json` — single source of truth, read live
- Lives at `%APPDATA%\com.phc260.kokoro-kindle-reader\controls.json`. The panel writes
  `voice`/`speed`/`gain`/`chunk`/`kindle_kokoro`/`paused`/`gpu_synth`; the host re-reads
  `voice`/`speed`/`chunk`/`gpu_synth` per utterance and `gain`/`paused` per sub-frame (via
  `native_synth::read_controls`), so a slider move lands on the next chunk/page — not
  frozen into prefetched samples (`paused` stalls the stream live with the pipe held open;
  `gpu_synth` triggers a session rebuild on the next chunk, since the EP is fixed at
  session-build time). `kindle_kokoro` is read separately, per watcher tick, by
  `kindle_watch::enabled` (default `true`).
- **Invariant: every key the panel writes must be read by whichever host reader consumes it**
  — `read_controls` for the synth fields (`paused` among them, consumed in `pipe.rs`),
  `kindle_watch` for `kindle_kokoro`. Keep them in sync.
- The pacing lead (500 ms) / sub-frame size (250 ms) are **not** user-tunable; fixed
  constants in `pipe.rs` (`DEFAULT_LEAD_MS` / `DEFAULT_SUBFRAME_MS`).

### Bitness, registration, file placement
- **The engine must stay x86** — Kindle is a 32-bit process and loads the COM DLL
  in-process by registry path. It **cannot** be merged into the x64 host.
- **Registration → `WOW6432Node`.** The 32-bit `regsvr32` writes `HKLM\SOFTWARE\Classes\…`
  into the WOW64 view — exactly what 32-bit Kindle reads.
- **Register from a stable path, never a git worktree.** The token's `InprocServer32`
  stores the absolute DLL path it was registered from; if that path goes away (e.g. an
  auto-cleaned worktree), Kindle's `LoadLibrary` fails silently and Read Aloud plays
  **nothing**. For a **dev** build, register the main checkout's
  `kokoro-sapi\target\i686-pc-windows-msvc\release\KokoroSapi.dll`.
- **Don't move `kokoro-sapi/`** — the registered token points at the DLL by path;
  relocating means re-`regsvr32`.
- **Never run an elevated artifact from a user-writable path (local EoP).** `regsvr32` runs
  a DLL's `DllRegisterServer` and the guard runs a `.ps1` — both **as admin**. So
  `voice-setup.ps1 -Action register` stages both into an `icacls`-locked
  `%ProgramData%\Kokoro Kindle Reader\` and registers *those* copies, never the
  user-writable `%LOCALAPPDATA%` ones, and **fails closed** if the lock can't be set.
  `-Action unregister` executes only those locked copies too — with the keys deleted
  directly when they're absent, never a fallback to `resources\`.
  **Never point the installer's registration back at a user-writable path.** Full rationale
  and the residual (unsigned-installer) gap: [`packaging/README.md`](packaging/README.md).
- **Kindle (MSIX) shadows HKCU.** Its SAPI default voice (`DefaultTokenId`) comes from the
  package hive (`…\Packages\AMZNKindle…\SystemAppData\Helium\User.dat`), not real HKCU.
  Patch it via `reg load`/`unload` with Kindle stopped — `kindle-voice-guard.ps1 -Set
  kokoro|david`. **On Kindle 1.0.18632.0+ this `DefaultTokenId` is ignored by the narrator**
  (it uses the WinRT default — see the hook), so the guard is a harmless no-op there, kept
  only for older builds. The panel's checkbox no longer runs it; it just persists
  `kindle_kokoro`, which the host's watcher acts on (no UAC).

### The Kindle-18632 hook
- **Selection-only, in-memory, x86, slot-18.** Kindle 18632's narrator (`SpVoiceEngine` in
  `xrm120.dll`) resolves its voice from the WinRT `SpeechSynthesizer` default and applies it
  via `ISpVoice::SetVoice`, ignoring `DefaultTokenId`. The fix injects `kokoro_hook.dll` to
  patch the shared `ISpVoice` vtable **slot 18** (`SetVoice`) → Kokoro token.
- Invariants: the hook + `kokoro-inject.exe` **must stay x86** (Kindle is 32-bit; the
  injector reuses its own `LoadLibraryW` address in the target); the host (x64) **spawns**
  the injector, never injects itself; injection needs host and Kindle at the **same
  integrity** (both normal user — the watcher retries the injector a few times per Kindle
  PID, then logs and gives up when `OpenProcess` keeps failing); the
  patch is **in-memory** (gone when Kindle exits — no persistence/unhook, so disabling
  applies on Kindle's next launch). `kokoro-hook`'s `selftest` guards the slot-18 ABI.

### Where shared files live
- The synth core (`native_synth.rs` + `text.rs` + `espeak.rs` + `split_text.rs`) is in
  `kokoro-host/src/` — **not** in the engine crate. `text.rs`/`espeak.rs` must stay
  pure/self-contained (no `kokoro-host`-specific state) because `kokoro-bench` reuses them
  via `#[path]` includes (`kokoro-host` is bin-only, no lib target).
- `model-manifest.json` + `icons/` are at the repo root (the panel embeds the manifest; the
  exes + installer use the icons). `icons/*` are in Git LFS.
- The pipe wire constants live in the `kokoro-protocol` crate — a `path` dep of **both**
  `kokoro-host` and `kokoro-sapi`, so the two ends can't drift. Neither may hardcode them.
- The Kindle-18632 hook + injector are standalone root crates (`kokoro-hook/`,
  `kokoro-inject/`), built x86 and staged into the installer's `resources\`.
- There is **no root workspace**; each crate builds standalone with its own target dir.

## Environment quirks

- **PowerShell 5.1:** don't redirect native stderr (`2>&1` + `$ErrorActionPreference=Stop`
  turns a harmless cmd-autorun line into a terminating error). `Select-Object -First`
  truncates upstream pipelines. Writing `.ps1` files: keep them **ASCII** — PS 5.1 misreads
  a UTF-8-no-BOM em-dash "—", so use "-" in scripts (Rust/`.slint` handle "—" fine).
- **Keep `installer.nsi` ASCII too.** `makensis` parses the script as **ACP** (see its
  `(ACP)` log line) since the file has no BOM — `Unicode true` only makes the *output*
  installer's strings Unicode. So a UTF-8 `…`/`—` in a user-visible `DetailPrint`/
  `MessageBox` renders as mojibake (`â€¦`) in the install UI. Use plain ASCII (`...`, `-`).
- **File locks:** rebuilds hit LNK1104 / "Access is denied" while Kindle holds
  `KokoroSapi.dll` or a running `kokoro-panel.exe`/`kokoro-host.exe` holds its exe — stop
  them first. Port lingers after a crashed session.
- **Slint `step`** on a `Slider` only affects keyboard/scroll, **not** mouse drag — snap
  the dragged value manually (see `SliderRow` in `panel.slint`).
- Registering/unregistering the voice and editing the MSIX hive need elevation
  (`Start-Process -Verb RunAs`).
