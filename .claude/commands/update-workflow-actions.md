---
description: Check every action pin in .github/workflows for a newer release and bump it, with a compatibility check.
argument-hint: "[optional: a specific action, e.g. actions/upload-artifact, to limit the scope]"
---

Update the **third-party GitHub Actions** pinned in this repo's workflows to their
latest releases, checking each bump for breaking changes before applying it. This is a
*targeted dependency bump*, not a workflow rewrite — only change version pins (and any
usage a bump forces), never the job logic. If `$ARGUMENTS` names a specific action,
limit the whole pass to that one.

## This repo's pinning convention
- Pins are **moving major tags** (`@v5`, `@v7`), not full `@vX.Y.Z` and not SHA pins.
  So the target for a bump is *the latest major's moving tag* — e.g. if the newest
  release is `v7.0.1`, pin `@v7` (not `@v7.0.1`).
- **Leave non-version moving tags alone.** `dtolnay/rust-toolchain@stable` is a channel,
  not a version — don't "bump" it. Note it and move on. Same for any `@main`/`@master`.
- **First-party `actions/*` and well-known third-party actions only.** Don't repin
  something to a tag that doesn't exist.

## Steps

1. **Inventory.** List every `uses:` line across `.github/workflows/*.yml` with its file,
   line, and current pin (`git grep -n 'uses:' -- .github/workflows`). Group by action so
   the same action used in multiple workflows is bumped consistently. Today that set is:
   `actions/checkout`, `actions/setup-python`, `Swatinem/rust-cache`,
   `actions/upload-artifact`, `softprops/action-gh-release`, and the channel-pinned
   `dtolnay/rust-toolchain@stable` (skip that one).

2. **Find the latest release for each.** Fetch `https://github.com/<owner>/<repo>/releases`
   (e.g. `https://github.com/actions/upload-artifact/releases`) and read the newest
   **stable** release tag — ignore pre-releases/RCs and `-nodeNN` variants. Derive the
   moving major tag from it. If the current pin already equals the latest major, mark it
   **OK** and don't touch it.

3. **Compatibility check (do this before editing).** For every action whose *major*
   version would change, read that action's release notes / migration guide for the
   breaking changes between the current and target major, then check whether this repo's
   usage actually trips them. Known sharp edges to check explicitly:
   - **`actions/upload-artifact` v4+** — artifact names must be **unique per run**
     (no re-uploading the same name); output is not mergeable across jobs; needs a
     matching `download-artifact` major if one is used. Verify each workflow uploads a
     single uniquely-named artifact and has no `download-artifact` consumer.
   - **`actions/checkout` / `setup-python`** — a new major usually bumps the Node runtime
     and may drop old GHES/runner support; confirm the runners here are `windows-latest`
     on github.com (fine) and that `lfs: true` / other inputs still exist.
   - **`action-gh-release` / `rust-cache`** — check input renames and default changes
     (e.g. release draft/latest behavior, cache key scheme) against how they're invoked.
   If a bump would require a **behavioral** change to the workflow (not just the pin),
   **stop and surface it** with the specific breaking note rather than silently rewriting
   the job.

4. **Apply the pin bumps.** Edit only the `@vN` on the `uses:` lines. Keep YAML valid and
   indentation intact. If an action is used in more than one workflow, bump all of them to
   the same tag.

5. **Sweep doc references.** If a bumped action's version is named in `CLAUDE.md`,
   `ARCHITECTURE.md`, a README, or another command, update it too (rare, but the
   `/sync-docs` contract is that docs don't drift).

6. **Report — then stop before committing.** Print a table: action · old pin → new pin ·
   latest release · breaking-change verdict (OK / notes). Do **not** commit or push as
   part of this command — hand off to `/commit-push` or wait for an explicit go, per this
   repo's gate discipline. A workflow file changing under `.github/` never justifies an
   auto-push.

## Hard limits (do not cross)
- **Never** pin-bump the release-triggering behavior or touch tags — editing
  `installer.yml`'s action pins is fine, but this command must not create/move tags or
  publish releases.
- **Never** switch the pinning style (don't convert `@vN` to a SHA or to `@vX.Y.Z`)
  unless explicitly asked — consistency with the existing convention matters.
- **Don't bump across a major with a real breaking change silently.** Flag it and let me
  decide.
- If the latest tag is ambiguous, a pre-release, or the action looks renamed/deprecated,
  **ask** rather than guessing.
