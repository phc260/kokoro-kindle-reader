# kokoro-panel — the settings panel (Slint, on demand)

The native settings panel (Slint/Fluent), **spawned on demand** from the tray "Settings"
item — there's **zero resident UI at idle**. Pick a narrator, tune speed/volume/chunk,
**Preview** a voice (synthesizes a fixed per-voice intro via the host pipe + rodio =
WYSIWYG, the same engine Kindle uses), download/verify the model, and toggle Kindle's
default voice between Kokoro and Microsoft David.

There is **no free-text reading box** by design: the app's job is choosing and hosting
the voice, not reading pasted text.

## Build

```powershell
cargo run   # or launch it from the host's tray → Settings
```

## Layout

| File | What |
|---|---|
| `ui/panel.slint` | The Fluent UI (sliders, narrator dropdown, Preview, Kindle-voice checkbox). |
| `src/main.rs` | Wires the Slint UI to the modules below; background work runs on threads and pushes results back via `upgrade_in_event_loop`. |
| `src/download.rs` | Model download/verify (framework-agnostic). |
| `src/kindle.rs` | The elevated Kindle-voice guard (UAC → `kindle-voice-guard.ps1`). |
| `src/preview.rs` | Synth via the host pipe + rodio playback. |

## Contract (do not rediscover)

- The panel **writes `controls.json`**; the host reads it live. The keys written here
  (`voice`, `speed`, `gain`, `chunk`, `kindle_kokoro`) must match what
  `kokoro-host/src/native_synth.rs::read_controls` reads — a slider move lands on
  Kindle's next page with no IPC or restart.
- The narrator list is derived from the embedded `model-manifest.json` (accent from
  `id[0]` a/b, gender from `id[1]` f/m).
- Slint `step` on a `Slider` only affects keyboard/scroll, not mouse drag — the dragged
  value is snapped manually (see `SliderRow` in `panel.slint`).

See the repo-root [`ARCHITECTURE.md`](../ARCHITECTURE.md) for how the panel fits the
overall topology.
