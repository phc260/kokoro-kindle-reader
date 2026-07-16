---
description: Cross-check the docs and cross-file invariants against the current code; fix drift.
---

Verify that the project's **documentation and cross-file invariants** still match the
current code, and fix anything stale. This is a *targeted accuracy pass*, not a rewrite
and not a repo-wide comment audit — only change what is actually wrong or newly missing.

Scope is deliberately bounded to two things: the three doc files, and a short list of
facts that are duplicated across files (so editing one file can silently invalidate
another). Do **not** scan every inline comment — comments adjacent to code are fixed at
edit time and caught by `/code-review` on the diff.

## 1. Documentation files
Cross-check each claim against the code and correct any that drifted. Add a note only
when a genuinely load-bearing, non-obvious mechanism is undocumented ("do not rediscover
this" material) — don't pad.

- `README.md` — user-facing: install steps, the "host must be running" caveat, the panel
  controls it describes, tuning advice, badges/links resolve.
- `CLAUDE.md` — the gotchas/invariants and the architecture descriptions.
- `ARCHITECTURE.md` — the engine chain, streaming/pacing model, Layout table, build steps.
- **Per-crate READMEs** — `kokoro-host/`, `kokoro-panel/`, `kokoro-protocol/`,
  `kokoro-sapi/`, `kokoro-sapi-smoke/`, `native-deps/`. These are deliberately *thin
  pointers* (orient + the load-bearing gotcha + a link to `CLAUDE.md`/`ARCHITECTURE.md`),
  so keep them thin: check their Layout tables list the files that actually exist, their
  build/run snippets still run, and any invariant they restate agrees with `CLAUDE.md`.
  Don't let them grow into a third full copy of the architecture.

Drift-prone claim types to check explicitly:
- **File/path references** — every file named in prose or the Layout table still exists
  at that path (e.g. `kokoro-sapi/*.ps1`, the DLL path, `kokoro-host/src/*`,
  `kokoro-panel/src/*`, `native-deps/*.ps1`, `model-manifest.json`, `icons/`).
- **Wire-protocol names** — the markers named in docs match the `kokoro-protocol` crate
  (`STREAM_END`/`SYNTH_ERROR` = `0xFFFF_FFFE`/`0xFFFF_FFFF`, the `'S'` request
  `[rate][textBytes][text]`, the `[nSamples][gain][f32…]` frame format).
- **`controls.json` keys** — the keys the docs list are the ones actually written/read
  (`voice`, `speed`, `gain`, `chunk`, `kindle_kokoro`, `paused`). `paused` is a live pause
  command (not a persisted setting): the panel writes it and `pipe.rs` consumes it per
  sub-frame to stall the stream. Note the pacing lead / sub-frame are *not* in the file —
  they're fixed constants in `pipe.rs`, so docs must not describe them as user-tunable.
- **Dependency pins / versions** — the ORT / `onnxruntime-webgpu` pin in
  `native-deps/fetch-deps.ps1` matches what the docs claim; the product version
  agrees across `packaging/installer.nsi` (`VERSION`) and the `FileVersion` in
  `kokoro-host/build.rs` + `kokoro-panel/build.rs`.
- **Command snippets** — the PowerShell/cargo commands in fenced blocks still run as
  written (`fetch-deps.ps1`, `cargo run --manifest-path …`, `build-installer.ps1`).

## 2. Cross-file invariants (the only comments in scope)
These are facts asserted in one place that must agree with another. Verify each pair and
fix whichever side is wrong (code is the source of truth; update the comment/doc):

- **Wire format** — the `kokoro-protocol` crate is the single source, used by **both**
  `kokoro-host/src/pipe.rs` and the SAPI engine `kokoro-sapi`. Verify neither hardcodes
  the constants inline instead.
- **`controls.json` contract** — the keys `kokoro-panel/src/main.rs` writes ⇆ the keys
  `kokoro-host/src/native_synth.rs` (`read_controls`) reads (and what `CLAUDE.md` lists).
- **Phonemizer parity** — `kokoro-host/src/text.rs` (normalization/segmentation) +
  `espeak.rs` must stay token-identical to kokoro-js; the golden tests in `text.rs`
  (`#[cfg(test)] mod tests`) lock the normalization passes. Model I/O (input names,
  style-row = clamp(nTokens-2,0,509), fp32) lives in `native_synth.rs::run_model`.
- **Manifest ⇆ narrator list** — voice entries in `model-manifest.json` (repo root) are
  what `kokoro-panel` embeds and derives its narrator dropdowns from.
- **Version sync** — the product version in `packaging/installer.nsi` (`VERSION`) ⇆ the
  `FileVersion`/`ProductVersion` set in `kokoro-host/build.rs` + `kokoro-panel/build.rs`.
- **Build ordering** — `native-deps/fetch-deps.ps1` must run before building
  `kokoro-host` (its `build.rs` panics without the provisioned dep folders); `build-installer.ps1`
  builds the x86 SAPI DLL (`kokoro-sapi`, needs the `i686-pc-windows-msvc` target).
- **Dep folder names** — the folders `native-deps/fetch-deps.ps1` provisions into
  `native-deps/` (`runtime/`, `espeak-ng-src/`) ⇆ the paths `kokoro-host/build.rs` reads
  (`native-deps/runtime`, `native-deps/espeak-ng-src/build-x64/...`) ⇆ the entries
  `native-deps/.gitignore` lists. These live directly in `native-deps/` (no `third_party/`
  wrapper), so a rename in one place must update all three or the deps get committed / the
  build panics. The `.gitignore` check is one-directional: every provisioned folder must
  be ignored, but extra entries there (`onnxruntime/`, `build/`) are deliberate defensive
  slack for older/other tooling — leave them alone.
- **Icons in LFS** — `icons/*` are tracked via Git LFS (`.gitattributes`); CI checks out
  with `lfs: true` so `icon.ico` bundles.

## Output
Report concisely: for each item, **OK** or **the fix applied** (file + what changed).
Make edits directly; don't ask before fixing a clear staleness. Flag anything ambiguous
(where code and docs disagree and the intended behavior is unclear) for me to decide
rather than guessing. End with whether anything still needs a human call.
