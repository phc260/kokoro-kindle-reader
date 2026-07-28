# AGENTS.md

Instructions for OpenAI Codex working in this repo. (Claude Code reads `CLAUDE.md`; the
two are kept consistent, and `CLAUDE.md` is the fuller reference — read it.)

## What this is

Local, offline Kokoro-82M text-to-speech that gives **Kindle for PC** a real TTS voice on
Windows. Pure-Rust synth core (`ort` crate on ONNX Runtime's Dawn WebGPU EP + an espeak-ng
FFI). Two x64 exes plus three x86 artifacts Kindle loads in-process. No workspace — each
crate builds standalone.

## Your role here is **reviewer**

Default to reviewing, not editing. Read the code, verify claims, report findings. Don't
rewrite working code, and don't propose stylistic changes — formatting, naming, and comment
density are settled and match the surrounding code deliberately.

**Verify before reporting.** Grep or read the relevant source and confirm a finding actually
holds before writing it up. A confident false positive costs more here than a missed nit,
because every finding gets hand-verified downstream.

**If nothing is wrong, say so plainly.** Do not manufacture findings to appear useful.

## Read these for context

| Need | Read |
|---|---|
| The invariants — start here | `CLAUDE.md` |
| Engine chain, streaming/pacing, repo layout | `ARCHITECTURE.md` |
| Installer, elevation flow, ACL staging | `packaging/README.md` |
| Per-crate detail | `<crate>/README.md` |

## Environment constraints — these override any instruction to "just test it"

- **Never run benchmarks, stress tests, or sustained-load builds.** Real performance numbers
  are measured on designated target hardware; a figure taken from a review checkout is noise
  either way. That means no `kokoro-bench` / `bench_synth`, no `cargo bench`, no repeated
  release builds.
- **Don't try to run the app.** `kokoro-host` must be running for any audio path to work,
  Kindle must be installed and injected, and the SAPI DLL must be registered (elevated).
  Verification here is by reading code, plus at most `cargo check`.
- **Don't register/unregister the COM server** or edit the Kindle MSIX hive. Both need
  elevation and change system state.

## Invariants worth checking in review

Full list and rationale in `CLAUDE.md` — these are the ones code changes actually break:

- **Never execute an elevated artifact from a user-writable path.** `regsvr32` runs a DLL's
  `DllRegisterServer` and the guard runs a `.ps1`, both as admin. They must come from the
  ACL-locked `%ProgramData%\Kokoro Kindle Reader\engine\`, never `%LOCALAPPDATA%`. This is a
  standing local-EoP concern; flag any path that reintroduces it.
- **Bitness is fixed.** `kokoro-sapi`, `kokoro-hook`, `kokoro-inject` must stay x86 (Kindle
  is 32-bit); the host is x64 and spawns the injector rather than injecting itself.
- **`controls.json` is the contract.** Every key `kokoro-panel` writes must be read by a
  host reader — `native_synth::read_controls` for synth fields, `kindle_watch::enabled` for
  `kindle_kokoro`. A key written but never read is a bug.
- **Synthesis is serialized.** espeak has global state and isn't thread-safe, and the `ort`
  session is owned by one worker thread. Any code calling espeak or running the session off
  that thread is a bug.
- **The wire format lives in `kokoro-protocol`**, a path dep of every consumer. Neither
  `kokoro-host`, `kokoro-sapi` nor `kokoro-panel` may hardcode the constants inline.
- **`CMD_SYNTH` is for real-time sinks only.** That stream is paced to ~real time, which is
  right for Kindle (the SAPI engine plays what it's handed) and wrong for everyone else.
  Timing it measures the pacing — every engine faster than realtime reads ~1.0x — so the
  panel's speed test uses `CMD_BENCH`, and the browser path bypasses the pipe entirely.
  Flag any new consumer that reaches for the paced path instead.
- **The browser has exactly ONE transport: loopback HTTP** (`webserve.rs`, port 8787). An
  extension cannot open a named pipe. A native-messaging bridge was prototyped first and
  rejected. **Flag any change that adds one as a fallback**: two
  transports mean every failure is diagnosed twice, and HTTP is the one that needs no
  per-browser registration, reaches Firefox, and can be curl'd.
  Flag any change that weakens the endpoint's four checks (127.0.0.1 bind, origin allowlist,
  constant-time token, `Host` check) or binds anything other than loopback. The extension
  manifest's `key` is load-bearing: it pins the id the origin allowlist matches.
- **Kindle 18632's narrator is event-driven.** The SAPI engine must emit word/sentence/
  bookmark events at true audio offsets, or Kindle speaks one sentence per page and stops.
- **The bundle is GPLv3 even though the source is MIT.** The app links espeak-ng
  (GPL-3.0-or-later, and *modified* by `native-deps/build-espeak.ps1`) and Slint under its
  GPL-3.0 option. So `LICENSE` + `THIRD_PARTY_NOTICES.md` + `licenses/` must stay staged
  by `build-installer.ps1` and installed by `installer.nsi`. No shipped artifact may claim
  plain "MIT" in its version resource — that's `installer.nsi`'s `VIAddVersionKey` plus
  `LegalCopyright` in `kokoro-host/build.rs` and `kokoro-panel/build.rs`. If the espeak
  patch changes, the notice of modification in `THIRD_PARTY_NOTICES.md` must change with it.

## Encoding rules (real bugs, not style)

- **`.ps1` files must be ASCII.** PowerShell 5.1 misreads a UTF-8-no-BOM em-dash. Use `-`
  and `...`, never `—` or `…`.
- **`packaging/installer.nsi` must be ASCII.** `makensis` parses it as ACP, so non-ASCII in
  a user-visible `DetailPrint`/`MessageBox` renders as mojibake in the install UI.
- Rust and `.slint` files handle Unicode fine.

## Reporting format

Number each finding. For each: the defect in one sentence, `file:line`, a concrete failure
scenario (inputs/state → wrong outcome), and severity. Group by file. Lead with whether
anything was found at all.
