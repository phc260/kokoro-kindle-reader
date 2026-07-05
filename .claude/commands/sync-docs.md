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

- `README.md` — user-facing: install steps, the "app must be running" caveat, tuning
  advice, badges/links resolve.
- `CLAUDE.md` — the gotchas/invariants and the architecture descriptions.
- `ARCHITECTURE.md` — the engine chain, streaming/pacing model, Layout table, build steps.

Drift-prone claim types to check explicitly:
- **File/path references** — every file named in prose or the Layout table still exists
  at that path (e.g. `kokoro-sapi/*.ps1`, the DLL path, `src-tauri/src/*`).
- **Event / wire-protocol names** — the Tauri event names and `WorkerProtocol.h` markers
  named in docs match the code (`synth-request`/`synth_result`, `gain-request`,
  `stream-config-request`/`stream_config_result`, `kStreamEnd`/`kSynthError`, the
  `[nSamples][gain][f32…]` frame format).
- **localStorage keys** — the keys the docs list are the ones actually written/read
  (`tts-voice`, `tts-speed`, `tts-gain`, `tts-chunk`, `tts-lead`, `tts-subframe`,
  `kindle-agency`).
- **Dependency pins / versions** — pinned deps in the docs match `package.json` /
  `Cargo.toml` (notably `@huggingface/transformers` stays `3.8.1` to match kokoro-js's
  ORT 1.22; `bundle.targets` is `["nsis"]`).
- **Command snippets** — the PowerShell/bun commands in fenced blocks still run as written.

## 2. Cross-file invariants (the only comments in scope)
These are facts asserted in one place that must agree with another. Verify each pair and
fix whichever side is wrong (code is the source of truth; update the comment/doc):

- **Wire format** — `kokoro-sapi/src/WorkerProtocol.h` ⇆ `src-tauri/src/pipe_server.rs`
  ("change it in both places").
- **localStorage contract** — the keys `src/App.tsx` writes ⇆ the keys `src/bridge.ts`
  reads (and what `CLAUDE.md` lists).
- **Manifest ⇆ voices** — voice entries in `src-tauri/model-manifest.json` ⇆ `VOICES`
  in `src/voices.ts`.
- **Version sync** — the version string in `package.json`, `src-tauri/Cargo.toml`,
  `src-tauri/tauri.conf.json`, and `src-tauri/Cargo.lock` all match.
- **Build ordering** — comments/docs that assert `kokoro-sapi\build.ps1` must run before
  `tauri build` (the DLL must exist for `bundle.resources`) still hold.
- **ORT wasm serving** — `tts.worker.ts` `wasmPaths = "/"` ⇆ `vite.config.ts` serving/
  copying the ORT wasm to root, and `ortDropDeadWasmPlugin` registered in both `plugins`
  and `worker.plugins`.

## Output
Report concisely: for each item, **OK** or **the fix applied** (file + what changed).
Make edits directly; don't ask before fixing a clear staleness. Flag anything ambiguous
(where code and docs disagree and the intended behavior is unclear) for me to decide
rather than guessing. End with whether anything still needs a human call.
