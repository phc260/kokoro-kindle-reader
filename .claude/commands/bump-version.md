---
description: Bump the product version across every version string in lockstep (Cargo.toml, build.rs, installer.nsi).
argument-hint: <x.y.z>
allowed-tools: Read, Edit, Grep, Bash(git status:*), Bash(grep:*)
---

Bump the **product version** to `$ARGUMENTS` everywhere it is hard-coded, keeping all
locations in lockstep. This command **only edits version strings** — it does not build,
commit, tag, or push (those are deliberate, separate steps).

## Steps

1. **Validate the argument.** `$ARGUMENTS` must be a plain three-part semver `X.Y.Z`
   (digits only, e.g. `0.4.0`). If it is empty, malformed, or has a `v` prefix, stop and
   explain the expected form — do not guess.

2. **Read the current version** from `packaging/installer.nsi` (the `!define VERSION`
   line) so you can report the old → new transition and match the exact old strings.

3. **Edit these 8 locations** (the version was previously confirmed to live in exactly
   these spots):

   Three-part `version = "X.Y.Z"` — the `[package]` `version` (line ~3) of:
   - `kokoro-host/Cargo.toml`
   - `kokoro-panel/Cargo.toml`
   - `kokoro-sapi/Cargo.toml`
   - `kokoro-protocol/Cargo.toml`
   - `kokoro-hook/Cargo.toml`
   - `kokoro-inject/Cargo.toml`
   - `kokoro-sapi-smoke/Cargo.toml`

   In `packaging/installer.nsi` — **both** the three-part define and the four-part
   product version:
   - `!define VERSION "X.Y.Z"`
   - `VIProductVersion "X.Y.Z.0"`  (Windows version resources are four-part; the `.0` is
     the unused revision field — keep it)

   Match each old string exactly so the edits are unambiguous.

4. **Do NOT touch** these — they pick the version up automatically:
   - `kokoro-host/build.rs` / `kokoro-panel/build.rs` — the exe `FileVersion`/
     `ProductVersion` are derived from `CARGO_PKG_VERSION` (`format!("{}.0", ...)`), so the
     Cargo.toml bump flows through with no edit here.
   - `installer.nsi`'s `VIAddVersionKey "FileVersion" "${VERSION}"` — derived from the define.
   - all `Cargo.lock` files — the `version` entries refresh on the next `cargo build`.

5. **Verify.** Grep the repo for both the old and new version and confirm: no stale
   occurrences of the old version remain outside `Cargo.lock` and `native-deps/` dep
   folders; and all 8 edited locations now show the new version. Report a short table of
   the files changed (old → new) and remind me that building the installer, committing,
   and tagging are separate steps I run when ready.
