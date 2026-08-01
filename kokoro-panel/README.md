# kokoro-panel — the settings panel (Slint, on demand)

The native settings panel (Slint/Fluent), **spawned on demand** from the tray "Settings"
item — there's **zero resident UI at idle**. Pick a narrator, tune speed/volume/chunk,
**Preview** a voice (synthesizes a fixed per-voice intro via the host pipe + rodio =
WYSIWYG, the same engine Kindle uses), download/verify the model, and toggle whether
Kindle narrates with Kokoro (the host's watcher acts on the flag).

There is **no free-text reading box** by design: the app's job is choosing and hosting
the voice, not reading pasted text.

**The panel does not touch Kindle.** `kokoro-host` is the sole authority for Kindle reading
and health; Play/Stop/Pause/Resume and "close Kindle" all go over the named pipe as intent
(`CMD_KINDLE`, in `src/hostlink.rs`) and the panel draws the state the host reports back.
What the panel still owns is its own persisted settings and its own audio (Preview).

## Build

```powershell
cargo run   # or launch it from the host's tray → Settings
```

## Layout

| File | What |
|---|---|
| `ui/panel.slint` | The Fluent UI (sliders, narrator dropdown, Preview + transport buttons, "Narrate Kindle with Kokoro" checkbox, Read Aloud switch, "Synthesize on GPU" checkbox + its help "?" + the sprint-icon speed test beside it). `IconButton` is the shared glyph-button chrome (Material Symbols SVG + accent + hover + a `Tooltip` child for the label); `HelpIcon` is the drawn "?" that carries a control's explanation in a `Tooltip` instead of a permanent paragraph — same size as an `IconButton` but deliberately not one, since there's nothing to click (cursor is `help`, weight stays at body text); `ModalCard` is the shared dialog chrome (scrim + centred card). |
| `src/main.rs` | Wires the Slint UI to the modules below; background work runs on threads and pushes results back via `upgrade_in_event_loop`. Owns the 1 Hz host heartbeat (`start_heartbeat`) and the two functions that paint its result (`apply_report` / `apply_offline`). The Kindle-narration checkbox raises a Yes/No confirm dialog; Yes persists `kindle_kokoro` and asks the host to close Kindle (the flag only lands on Kindle's next launch), No reverts the checkbox. Also drives the speed test (`run_speed_test`) and applies its verdict to `gpu_synth`. |
| `src/hostlink.rs` | The `CMD_KINDLE` client and the shared pipe `connect()`: Play/Stop/Pause/Resume/Close as intent, plus the query the heartbeat rides on. The panel's entire relationship with Kindle. |
| `src/download.rs` | Model download/verify (framework-agnostic). |
| `src/preview.rs` | Synth via the host pipe (`CMD_PREVIEW`) + rodio playback. |
| `src/benchmark.rs` | The `CMD_BENCH` client: times one execution provider on the host and returns its realtime factor. |

## Contract (do not rediscover)

- The panel **writes `controls.json`**; the host reads it live. The synth keys (`voice`,
  `speed`, `gain`, `chunk`, `gpu_synth`) must match what
  `kokoro-host/src/native_synth.rs::read_controls` reads — a slider move lands on
  Kindle's next page with no IPC or restart (`gpu_synth` additionally costs a session
  rebuild, since the execution provider is fixed at session-build time). `kindle_kokoro`
  is read by `kokoro-host/src/kindle_watch.rs` (gates Kindle auto-injection), and gates the
  panel's own lower half as well — see `controls-active` below.
- **`controls.json` carries settings only.** Pause used to live there, which made a live
  *command* travel as a file the host polled; it is host-owned state now, reached over the
  pipe. Don't put another command in that file — a setting is something the user chose and
  the host reads when it next needs it; a command has to arrive.
- **Host health is reachability, proved every second by real I/O.** `start_heartbeat` sends
  a `CMD_KINDLE` query at ~1 Hz; a pipe that won't open is an offline host, and the whole
  panel below the Kindle-narration checkbox goes dark within about a second — settings
  included, not just the transport. The narrator and sliders *would* still persist to
  `controls.json` with the host down, but a live-looking slider nothing can read is a
  control claiming an effect it can't have, and it invites fiddling instead of reading the
  one red line that says what's wrong. Model download stays enabled: it doesn't go through
  the host, and it's the one thing worth doing while the host is off. `controls-active` in
  `panel.slint` is that one gate: model present, not mid download/verify, host answering,
  **and** "Narrate Kindle with Kokoro" ticked — the engine picker and speed test included,
  since with narration off there's nothing left down there for either to affect. Only the
  Read Aloud switch adds a condition (`kindle-running`). It never infers health from a process
  name, the tray icon, a cached voice list, a `controls.json` timestamp, or a request that
  worked a moment ago — each of those keeps saying "ready" after the host has gone, which is
  exactly the bug this replaced. Exactly one request is in flight at a time: it runs on a
  short-lived thread so the wait can be bounded (a blocking pipe read has no timeout of its
  own), and a request that overruns its bound is *waited out* before another starts, so a
  wedged host leaves one stuck thread rather than a new one every second.
- **The Read Aloud switch is locked from a flip until the host DEMONSTRATES it landed**
  (`Settling`). A fixed timer was tried first and can only ever guess at a duration the host
  actually knows — 5 s expired squarely inside the silent synthesis gap, which is the least
  informative moment there is. The rule is symmetric and reads the host's own report: turning
  on settles when the host is visibly engaged with Kindle's audio (`STATE_KINDLE_SYNTH`, a
  page in flight, or `kindle_speaking`, audio out); turning off settles when it is visibly
  neither. Both additionally require the host's `busy` to have cleared, or a Stop would settle
  on silence its own Ctrl+A had not yet caused.
  - `STATE_KINDLE_SYNTH` is what makes the "on" direction possible. Between Ctrl+A landing and
    the first sample existing there are seconds of synthesis in which every audio clock reads
    idle — a clock cannot report work that has produced no audio yet. That bit can, so the
    host now sends it.
  - The heartbeat tightens from 1 Hz to `SETTLE_POLL` (250 ms) while a flip is pending, since
    that is the one stretch where the answer changes something on screen, and `KINDLE_QUERY`
    is answered inline off atomics.
  - The caps differ by direction because the two wait on different things. `SETTLE_CAP_ON`
    (15 s) waits on synthesis, which can take double digits of seconds on a machine slower
    than real time. `SETTLE_CAP_OFF` (8 s) waits only on audio draining (~2.5 s), and the
    host's belief is already correct the moment the command returns — its real job is to stop
    Kindle *playing out the rest of a page* from holding the switch for the length of it.
    Without caps at all, a flip that can't take effect would disable the switch for the rest
    of the session, which is worse than the race it replaced.
  - A report with `kindle_running` false clears the wait outright: nothing can land if Kindle
    has gone, so the lock shouldn't survive to the cap and greet a reopened Kindle.
- **Commands leave in click order, through ONE queue (`Intents`).** One long-lived thread —
  never a thread per click, and never a lane per kind of command. Both alternatives give each
  command its own pipe connection, connections are served in arrival order, and the host
  serializes by arrival, so clicks invert. These commands aren't idempotent, so every
  inversion sticks: a Stop that overtakes its own Play finds reading already off, no-ops, and
  leaves the delayed Play to start reading with the switch saying stopped; a Pause that lands
  after the Stop it preceded parks a stream the panel offers no Resume for. Against a blind
  Ctrl+A toggle both are unrecoverable until the user notices. Splitting Pause/Resume onto
  their own lane was tried and reverted — it cures the latency below and reintroduces exactly
  those two races *across* the lanes. One queue; the last click wins.
- **A queued Pause can wait out a Play/Stop/Close, and that does not breach "a pause must
  never queue behind Kindle work".** That invariant is about the *host's* control thread, and
  its case is a pause landing mid-page while Kindle narrates — when this queue is empty. The
  queue is non-empty only while a Play, Stop or Close is in flight, and in that window there
  is either nothing narrating yet (Play) or narration the queued command is about to end
  anyway (Stop, Close). The heartbeat's query never enters the queue at all; it has its own
  thread.
- **The panel never repaints the transport from state older than its own command.** Two
  guards, both read on the UI thread at paint time. `Intents::busy()` counts commands queued
  but not yet *painted* (released inside the UI closure, not when the reply is merely posted),
  and the host reports its own `STATE_BUSY` alongside. `Intents::epoch()` covers what a
  counter can't: the heartbeat thread can be descheduled between receiving its reply and
  posting the closure that draws it, and a whole command can queue, run and drain to zero
  inside that gap — the closure would then wake to `busy() == false` and faithfully paint a
  report from before the click. The epoch is sampled before the query goes out and compared
  when painting, so any command at all, still running or long finished, marks the report too
  old to draw the transport from.
- **The speed test measures, it doesn't guess.** GPU-vs-CPU can't be decided from the
  hardware name (an integrated GPU may run at half the CPU's rate or several times it),
  so the sprint button beside "Synthesize on GPU" times both via `CMD_BENCH` and ticks
  the winner. It goes through
  `benchmark.rs`, **not** Preview: the `CMD_SYNTH` stream is paced to ~real time, so timing a
  preview would measure the pacing. It refuses to start while the host reports **any** audio
  in flight — Kindle, the browser extension, or this panel's own Preview all queue on the one
  serialized synth worker, and a test holds it for tens of seconds. Deliberately the *broad*
  clock, not the Kindle-only one the "Speaking" indicator uses: the costs aren't symmetric, a
  false "busy" costs a retry while a false "idle" drops a ~40 s measurement in front of live
  narration. Best-effort regardless, since nothing reserves that worker. A result inside
  `BENCH_TIE_RATIO` is a tie and changes nothing.
- The narrator list is derived from the embedded `model-manifest.json` (accent from
  `id[0]` a/b, gender from `id[1]` f/m).
- Slint `step` on a `Slider` only affects keyboard/scroll, not mouse drag — the dragged
  value is snapped manually (see `SliderRow` in `panel.slint`).
- `KINDLE_CLOSE` only closes Kindle — nothing relaunches it. MSIX/Desktop-Bridge packaged
  apps (Kindle) aren't reliably relaunched via a raw `CreateProcess` on their exe path, so
  the user reopens Kindle by hand; the confirm dialog's body text says so.

See the repo-root [`ARCHITECTURE.md`](../ARCHITECTURE.md) for how the panel fits the
overall topology.
