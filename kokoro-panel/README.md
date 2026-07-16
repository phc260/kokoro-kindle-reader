# kokoro-panel — the settings panel (Slint, on demand)

The native settings panel (Slint/Fluent), **spawned on demand** from the tray "Settings"
item — there's **zero resident UI at idle**. Pick a narrator, tune speed/volume/chunk,
**Preview** a voice (synthesizes a fixed per-voice intro via the host pipe + rodio =
WYSIWYG, the same engine Kindle uses), download/verify the model, and toggle whether
Kindle narrates with Kokoro (the host's watcher acts on the flag).

There is **no free-text reading box** by design: the app's job is choosing and hosting
the voice, not reading pasted text.

## Build

```powershell
cargo run   # or launch it from the host's tray → Settings
```

## Layout

| File | What |
|---|---|
| `ui/panel.slint` | The Fluent UI (sliders, narrator dropdown, Preview + transport buttons, "Narrate Kindle with Kokoro" checkbox, Read Aloud switch). |
| `src/main.rs` | Wires the Slint UI to the modules below; background work runs on threads and pushes results back via `upgrade_in_event_loop`. The Kindle-narration checkbox raises a Yes/No confirm dialog; Yes persists `kindle_kokoro` and closes Kindle (the flag only lands on Kindle's next launch), No reverts the checkbox. |
| `src/download.rs` | Model download/verify (framework-agnostic). |
| `src/preview.rs` | Synth via the host pipe + rodio playback. |
| `src/kindle_reader.rs` | Drives Kindle's "Assistive reader" (Read Aloud) hands-free — foregrounds Kindle and sends its Ctrl+A shortcut via raw `SendInput` (works whether or not the Aa menu is open); UI Automation is used only for best-effort state readback and to dismiss an open Aa/ToC flyout first. Also `close()`s Kindle (found by process name via Toolhelp32, `WM_CLOSE`'d — not relaunched) for the narration-voice confirm dialog. |

## Contract (do not rediscover)

- The panel **writes `controls.json`**; the host reads it live. The synth keys (`voice`,
  `speed`, `gain`, `chunk`) must match what `kokoro-host/src/native_synth.rs::read_controls`
  reads — a slider move lands on Kindle's next page with no IPC or restart. `kindle_kokoro`
  is read by `kokoro-host/src/kindle_watch.rs` (gates Kindle auto-injection); `paused` is read
  by `read_controls` and consumed in `pipe.rs` (a live pause command that stalls the stream).
- The narrator list is derived from the embedded `model-manifest.json` (accent from
  `id[0]` a/b, gender from `id[1]` f/m).
- Slint `step` on a `Slider` only affects keyboard/scroll, not mouse drag — the dragged
  value is snapped manually (see `SliderRow` in `panel.slint`).
- `kindle_reader::close()` only closes Kindle — it never relaunches it. MSIX/Desktop-
  Bridge packaged apps (Kindle) aren't reliably relaunched via a raw `CreateProcess` on
  their exe path, so the user reopens Kindle by hand; the confirm dialog's body text
  says so.

See the repo-root [`ARCHITECTURE.md`](../ARCHITECTURE.md) for how the panel fits the
overall topology.
