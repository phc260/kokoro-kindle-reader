<div align="center">

<img src="icons/128x128@2x.png" alt="Kokoro Kindle Reader" width="120" height="120">

# Kokoro Kindle Reader

**Give Kindle for PC a natural voice — local, offline Kokoro-82M text-to-speech, running on your own GPU.**

[![Platform: Windows](https://img.shields.io/badge/platform-Windows-0078D6?logo=windows&logoColor=white)](#install)
[![TTS: Kokoro-82M](https://img.shields.io/badge/TTS-Kokoro--82M-ff69b4)](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX)
[![100% Offline](https://img.shields.io/badge/100%25-offline-2ea44f)](#)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Latest release](https://img.shields.io/github/v/release/phc260/kokoro-kindle-reader?include_prereleases&label=release)](https://github.com/phc260/kokoro-kindle-reader/releases)

</div>

Nothing is sent to the cloud — [Kokoro-82M](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX)
runs entirely on your machine. kokoro-kindle-reader is two things in one app:

1. **A voice control panel** — choose your narrator and tune speed and volume,
   with a **Preview** button to hear how it sounds.
2. **A natural voice for Kindle for PC** — "Kokoro (SAPI5)" shows up in Windows'
   voice list, so **Kindle's Read Aloud** narrates your books in Kokoro's voice
   instead of the robotic system one.

> **One thing to know up front:** the kokoro-kindle-reader app does the actual speaking,
> so **it must be running** whenever you want Kindle to read aloud. Think of it as
> the engine Kindle plugs into.

## Install

1. Download the latest installer from the
   [**Releases**](https://github.com/phc260/kokoro-kindle-reader/releases) page (the
   `-setup.exe` under the newest version).
2. Run it. It installs just for you (no machine-wide changes), then raises a single
   Windows UAC prompt to register the Kokoro voice and — if Kindle is installed —
   set Kokoro as Kindle's Read Aloud voice automatically.
3. Launch **kokoro-kindle-reader**. On first run it downloads the voice model
   (~430 MB) — a one-time setup wizard walks you through it. After that it works
   fully offline.

The app synthesizes on your GPU via WebGPU, so a reasonably modern GPU gives the
best results.

## Using the app

kokoro-kindle-reader runs in the **system tray**. Right-click the tray icon and choose
**Settings** to open the control panel — it's where you choose and audition the
voice, not a place to paste text. Whatever you set here is exactly what Kindle (and
the SAPI voice) uses when it reads.

1. Pick a **Narrator** with the three dropdowns (accent, gender, and name).
2. Adjust **Speed** and **Volume**, and **Sentences per chunk** if you want.
3. Tick **Use Kokoro as Kindle's default voice** to make Kindle read with Kokoro
   (this asks for administrator rights — Windows requires that to change Kindle's
   voice); untick it to hand Kindle back its built-in Microsoft voice.
4. Click **Preview** to hear the selected narrator read a short sample line.

Your choices are saved and applied to Kindle's **next page** automatically — no
restart needed.

## Reading Kindle books with Kokoro

1. Make sure **kokoro-kindle-reader is running** (it's the voice engine — no app, no
   sound). It lives in the system tray and auto-starts at login.
2. Tick **Use Kokoro as Kindle's default voice** in Settings if it isn't already
   (see above). Untick it anytime to restore Kindle's built-in voice.
3. **Reopen Kindle** after switching so it picks up the new voice.
4. In Kindle, start **Read Aloud** as usual — it now speaks with Kokoro, using the
   narrator, speed, and volume you set in the app.

The installer sets this up for you the first time; the in-app checkbox is for
switching back and forth later.

### Tuning Kindle playback

**Sentences per chunk** controls how Kindle narration streams: higher is smoother but
takes slightly longer to start each chunk. Sensible defaults are set, so you usually
don't need to touch it.

## Troubleshooting

- **Kindle is silent / no Read Aloud sound** — the kokoro-kindle-reader app isn't
  running. Start it and try again. (There's no fallback voice by design.)
- **Kindle reverted to the old robotic voice** — a Kindle update can reset its
  voice. Open kokoro-kindle-reader and flip the Microsoft/Kokoro toggle back to Kokoro,
  then reopen Kindle.
- **A switch didn't take effect** — fully close and reopen Kindle after changing
  the voice.
- **First launch is slow** — that's the one-time model download (~430 MB).
  Subsequent launches are fast and offline.

## How it works

The interesting part is letting 32-bit Kindle narrate with GPU TTS that lives in
a separate 64-bit process: a thin x86 COM voice plugin loads inside Kindle and
forwards each utterance over a named pipe to the kokoro-kindle-reader tray app, which
synthesizes natively on your GPU (Dawn WebGPU) and streams the audio back.

If you're curious about the engine chain, the wire protocol, the Kindle voice
registry/hive details, or want to **build from source**, see
[**ARCHITECTURE.md**](ARCHITECTURE.md).
