---
description: Review changes, commit with this repo's conventions, and push the current branch (never tags/releases).
argument-hint: "[optional: subject hint or scope]"
---

Commit the working changes and push the **current branch**. Stay the gate at every
step — this command must never push tags, create releases, or do anything
hard-to-reverse without an explicit confirm. `$ARGUMENTS` (if given) is a hint for the
commit subject/scope, not a literal message.

## Steps

1. **Survey.** Run `git status` and `git diff --stat`, and read the actual diff of the
   staged/unstaged changes (`git diff` / `git diff --cached`). Understand what changed
   before writing anything. If there are **no** changes, say so and stop.

2. **Branch check.** Report the current branch. This repo's release flow commits version
   bumps **directly on `main`** and tags afterward, so committing on `main` is normal
   here — do **not** auto-create a branch. Only suggest branching if the change is
   clearly unrelated to a release/main-line edit and the user hasn't signaled otherwise;
   otherwise commit on the current branch.

3. **Stage deliberately.** Stage the files that belong in this commit. Don't blindly
   `git add -A` if there are unrelated untracked files (scratch output, local configs) —
   call those out and leave them unless they're clearly part of the change. Note that
   `.gitattributes` may normalize CRLF/LF; that's expected.

4. **Compose the message** in this repo's style:
   - Imperative subject, ~50 chars, no trailing period (match recent log:
     `git log --oneline -8`).
   - A body explaining the **why** when the change isn't self-evident, wrapped ~72 cols.
   - End with the trailer exactly:
     `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`
   - **If Codex reviewed this change** (`/codex-review` ran and its findings shaped what
     landed), add a `Reviewed-by: OpenAI Codex (<model>)` line *above* the
     `Co-Authored-By:` trailer, naming the model actually used — check `model` in
     `~/.codex/config.toml`, or whatever `-m` was passed. Do not add it for changes Codex
     never saw, and do not guess the model. See "Code review" in `DEVELOPMENT.md`.
   - Incorporate `$ARGUMENTS` as a hint if provided.

5. **Commit.** Never use `--no-verify` and never bypass signing. If a pre-commit hook
   fails, **stop and surface the failure** — fix the underlying issue, don't skip it.

6. **Push — confirm first.** Show the commit (`git show --stat HEAD`) and the exact push
   command you intend to run (`git push origin <branch>`), then **ask for confirmation**
   before pushing. Push only the current branch to `origin`.

## Hard limits (do not cross)
- **Never** create, move, or push a tag (`git tag`, `git push --tags`, `git push origin v*`).
  Tags trigger the release CI (`installer.yml`) — releases stay a manual, deliberate act.
- **Never** force-push (`--force` / `-f`) without an explicit, separate request.
- **Never** amend or rewrite already-pushed commits unless asked.
- If anything is ambiguous (what to stage, whether to branch, whether to push), **ask**
  rather than guessing.

## Output
End by reporting: branch, commit hash + subject, and whether it was pushed (or is
awaiting your confirmation). If a release is the actual goal, remind me that tagging is
separate and must be requested explicitly.
