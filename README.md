# kokoro-reader

Local, offline text-to-speech for Windows, powered by
[Kokoro-82M](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX) running
on your GPU. Nothing is sent to the cloud — the model runs entirely on your
machine. kokoro-reader is two things in one app:

1. **A reader app** — paste or type text, pick a narrator, and listen.
2. **A natural voice for Kindle for PC** — "Kokoro (SAPI5)" shows up in Windows'
   voice list, so **Kindle's Read Aloud** narrates your books in Kokoro's voice
   instead of the robotic system one.

> **One thing to know up front:** the kokoro-reader app does the actual speaking,
> so **it must be running** whenever you want Kindle to read aloud. Think of it as
> the engine Kindle plugs into.

## Install

1. Download the latest installer from the
   [**Releases**](https://github.com/phc260/kokoro-reader/releases) page (the
   `.exe` / `.msi` under the newest version).
2. Run it. Installation needs administrator rights — it registers the Kokoro
   voice with Windows and (if Kindle is installed) sets Kokoro as Kindle's Read
   Aloud voice automatically.
3. Launch **kokoro-reader**. On first run it downloads the voice model
   (~430 MB) — a one-time setup wizard walks you through it. After that it works
   fully offline.

A modern GPU is recommended (the app uses WebGPU). On machines without it, it
falls back to a slower CPU mode automatically.

## Using the reader

1. Paste or type text into the box.
2. Pick a **narrator** from the dropdown (different accents and voices).
3. Adjust **Speed** and **Volume** to taste. Click the **volume icon** to
   mute/unmute instantly.
4. Press **Play**. Press again to stop.

Your narrator, speed, and volume choices are remembered between sessions.

## Reading Kindle books with Kokoro

1. Make sure **kokoro-reader is running** (it's the voice engine — no app, no
   sound).
2. In the app, use the **Microsoft / Kokoro** toggle to choose which voice Kindle
   uses. Switching to **Kokoro** prompts for administrator rights (Windows
   requires this to change Kindle's voice). Switch back to **Microsoft** anytime.
3. **Reopen Kindle** after switching so it picks up the new voice.
4. In Kindle, start **Read Aloud** as usual — it now speaks with Kokoro.

The installer sets this up for you the first time; the in-app toggle is for
switching back and forth later.

### Tuning Kindle playback

The app exposes a few sliders that affect how Kindle narration streams. Sensible
defaults are set, but if you want to tune:

- **Sentences per chunk** — higher is smoother but takes slightly longer to start
  each chunk.
- **Pacing lead** — how much audio stays buffered ahead. **Lower = volume/mute
  changes take effect faster**, but set it too low and you may hear gaps or
  stutter. Lower it until you hear gaps, then back off a notch. The right value
  depends on how fast your machine synthesizes.
- **Sub-frame size** — how finely volume is re-checked. Smaller = slightly snappier
  volume response, but going much smaller than the pacing lead just adds overhead
  for no real benefit.

## Troubleshooting

- **Kindle is silent / no Read Aloud sound** — the kokoro-reader app isn't
  running. Start it and try again. (There's no fallback voice by design.)
- **Kindle reverted to the old robotic voice** — a Kindle update can reset its
  voice. Open kokoro-reader and flip the Microsoft/Kokoro toggle back to Kokoro,
  then reopen Kindle.
- **A switch didn't take effect** — fully close and reopen Kindle after changing
  the voice.
- **First launch is slow** — that's the one-time model download (~430 MB).
  Subsequent launches are fast and offline.

## How it works

The interesting part is letting 32-bit Kindle narrate with GPU TTS that lives in
a separate 64-bit process: a thin x86 COM voice plugin loads inside Kindle and
forwards each utterance over a named pipe to the kokoro-reader app, which
synthesizes on WebGPU and streams the audio back.

If you're curious about the engine chain, the wire protocol, the Kindle voice
registry/hive details, or want to **build from source**, see
[**ARCHITECTURE.md**](ARCHITECTURE.md).
