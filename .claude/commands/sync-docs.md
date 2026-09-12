---
description: Cross-check the docs and cross-file invariants against the current code; fix drift.
---

Dispatch this to the **`doc-sync` subagent** — do not run the sweep in the main thread.

```
Agent(subagent_type: "doc-sync", run_in_background: false,
      description: "Sync docs against code",
      prompt: "Run the full documentation + cross-file invariant accuracy pass on this
               repo per your instructions. Fix drift directly; leave changes uncommitted.")
```

**Why a subagent:** the pass reads ~19 doc files plus the source they describe (`pipe.rs`,
`native_synth.rs`, `model_patch.rs`, `main.rs`, `build.rs` ×2, `installer.nsi`,
`fetch-deps.py`, `model-manifest.json`, `ocr-manifest.json`, `kokoro-ocr/src/`, the
workflows). Run inline, all of that lands in the main context and stays there for the rest of
the session; run in the subagent, only the findings come back. The checklist itself lives in
[`.claude/agents/doc-sync.md`](../agents/doc-sync.md) — keep it there, not duplicated here.

If a specific area was named (e.g. `/sync-docs packaging`), pass that scope through in the
prompt so the agent narrows to it.

## After it reports

The agent's report is not shown to the user, so **relay it**:

- List the edits it made (`path:line — what changed`), grouped by file.
- Surface anything it flagged as ambiguous, and give your own read on it — don't just pass
  the question along.
- **Spot-check every edit that touches an invariant in `CLAUDE.md`, `LICENSING.md` or
  `THIRD_PARTY_NOTICES.md` before accepting it** — re-derive the claim from the source
  yourself, don't just read the edit and find it plausible. This is not a hedge against the
  agent's model: it caught a real one on 2026-09-11, where a *correct* claim
  (`unicode-ident` is in 6 of the 7 tracked lockfiles) was "fixed" to 5 of 7 by counting
  `kokoro-protocol` as a lockfile lacking the crate when it has no lockfile at all — landing
  in `CLAUDE.md` and mirrored into `THIRD_PARTY_NOTICES.md`. A count is the easiest thing
  here to change confidently and wrongly, and a licensing count is the worst place to do it.
  If one looks wrong, verify against the code and say so.
- **Act on any doc file it reports as unnamed by its checklist** — that list is how the scope
  stays current. It went stale once by exactly this route: five docs were added to the repo
  and the agent kept reporting "nothing drifted" without having opened them, which is the one
  failure mode of this command that looks like success. Add them to
  `.claude/agents/doc-sync.md` rather than letting the next run rediscover them.
- **Relay, don't perform, any hash refresh it flags** (`packaging/license-texts.sha256` and
  friends). The agent is instructed never to update one; an edit to `THIRD_PARTY_NOTICES.md`
  therefore leaves PR CI red until someone deliberately re-pins it.
- Say plainly if nothing drifted.

Don't commit unless asked.
