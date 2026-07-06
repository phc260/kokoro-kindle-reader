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

Drift-prone claim types to check explicitly:
- **File/path references** — every file named in prose or the Layout table still exists
  at that path (e.g. `kokoro-sapi/*.ps1`, the DLL path, `kokoro-host/src/*`,
  `kokoro-panel/src/*`, `kokoro-worker/src/*`, `model-manifest.json`, `icons/`).
- **Wire-protocol names** — the `WorkerProtocol.h` markers named in docs match the code
  (`kStreamEnd`/`kSynthError` ⇆ `STREAM_END`/`SYNTH_ERROR` = `0xFFFF_FFFE`/`0xFFFF_FFFF`,
  the `'S'` request `[rate][textBytes][text]`, the `[nSamples][gain][f32…]` frame format).
- **`controls.json` keys** — the keys the docs list are the ones actually written/read
  (`voice`, `speed`, `gain`, `chunk`, `kindle_kokoro`). Note the pacing lead / sub-frame
  are *not* in the file — they're fixed constants in `pipe.rs`, so docs must not describe
  them as user-tunable.
- **Dependency pins / versions** — the ORT / `onnxruntime-webgpu` pin in
  `kokoro-worker/tools/fetch-deps.ps1` matches what the docs claim; the product version
  agrees across `packaging/installer.nsi` (`VERSION`) and the `FileVersion` in
  `kokoro-host/build.rs` + `kokoro-panel/build.rs`.
- **Command snippets** — the PowerShell/cargo commands in fenced blocks still run as
  written (`fetch-deps.ps1`, `cargo run --manifest-path …`, `build-installer.ps1`).

## 2. Cross-file invariants (the only comments in scope)
These are facts asserted in one place that must agree with another. Verify each pair and
fix whichever side is wrong (code is the source of truth; update the comment/doc):

- **Wire format** — `kokoro-sapi/src/WorkerProtocol.h` (C++) ⇆ the `kokoro-protocol`
  crate (Rust, used by `kokoro-host/src/pipe.rs`). Two copies; change the format in both.
- **`controls.json` contract** — the keys `kokoro-panel/src/main.rs` writes ⇆ the keys
  `kokoro-host/src/native_synth.rs` (`read_controls`) reads (and what `CLAUDE.md` lists).
- **Chunker parity** — `split_text` in `kokoro-host/src/split_text.rs` ⇆ its 1:1 C++ port
  `SplitText` in `kokoro-worker/src/KokoroSynth.cpp`.
- **Manifest ⇆ narrator list** — voice entries in `model-manifest.json` (repo root) are
  what `kokoro-panel` embeds and derives its narrator dropdowns from.
- **Version sync** — the product version in `packaging/installer.nsi` (`VERSION`) ⇆ the
  `FileVersion`/`ProductVersion` set in `kokoro-host/build.rs` + `kokoro-panel/build.rs`.
- **Build ordering** — `kokoro-worker/tools/fetch-deps.ps1` must run before building
  `kokoro-host` (its `build.rs` panics without `third_party/`); `build-installer.ps1`
  runs `kokoro-sapi/build.ps1` if the x86 DLL is missing.
- **Icons in LFS** — `icons/*` are tracked via Git LFS (`.gitattributes`); CI checks out
  with `lfs: true` so `icon.ico` bundles.

## Output
Report concisely: for each item, **OK** or **the fix applied** (file + what changed).
Make edits directly; don't ask before fixing a clear staleness. Flag anything ambiguous
(where code and docs disagree and the intended behavior is unclear) for me to decide
rather than guessing. End with whether anything still needs a human call.
