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

**Why a subagent:** the pass reads ~13 doc files plus the source they describe (`pipe.rs`,
`native_synth.rs`, `main.rs`, `build.rs` ×2, `installer.nsi`, `fetch-deps.ps1`,
`model-manifest.json`, the workflows). Run inline, all of that lands in the main context and
stays there for the rest of the session; run in the subagent, only the findings come back.
The checklist itself lives in [`.claude/agents/doc-sync.md`](../agents/doc-sync.md) — keep it
there, not duplicated here.

If a specific area was named (e.g. `/sync-docs packaging`), pass that scope through in the
prompt so the agent narrows to it.

## After it reports

The agent's report is not shown to the user, so **relay it**:

- List the edits it made (`path:line — what changed`), grouped by file.
- Surface anything it flagged as ambiguous, and give your own read on it — don't just pass
  the question along.
- Spot-check any edit that touches an invariant in `CLAUDE.md` before accepting it. The agent
  runs on a smaller model; a plausible-sounding "fix" to a load-bearing claim is exactly the
  failure mode worth catching. If one looks wrong, verify against the code and say so.
- Say plainly if nothing drifted.

Don't commit unless asked.
