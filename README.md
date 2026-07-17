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
   Windows UAC prompt to register the Kokoro voice. Kindle narration with Kokoro is
   on by default — the app enables it automatically the next time Kindle runs.
3. Let the installer start the app (or launch **kokoro-kindle-reader** yourself) — it
   runs quietly in the **system tray**, no window. Right-click the tray icon, choose
   **Settings**, and click **Download** to fetch the voice model (~430 MB, one time).
   After that it works fully offline.

The app synthesizes on your GPU via WebGPU, so a **discrete GPU** (e.g. NVIDIA/AMD)
gives smooth, faster-than-realtime narration — tested smooth on an NVIDIA GTX 1060.
Laptops with only an **integrated GPU and no dedicated one** can fall well behind
realtime — narration will work, but may lag noticeably behind Kindle's pages. If so,
try unticking **Synthesize on GPU** in Settings (see Troubleshooting).

## Using the app

kokoro-kindle-reader runs in the **system tray**. Right-click the tray icon and choose
**Settings** to open the control panel — it's where you choose and audition the
voice, not a place to paste text. Whatever you set here is exactly what Kindle (and
the SAPI voice) uses when it reads.

The card at the top is the engine's status: it shows the model download, a quick
**file check** on each launch, then **Voice Engine Ready** — and flips to **Speaking**
live whenever Kokoro is narrating. The narrator and slider controls below stay greyed
out until the engine is ready and **Narrate Kindle with Kokoro** is ticked.

1. Pick a **Narrator** with the three dropdowns (accent, gender, and name).
2. Adjust **Speed** and **Volume**, and **Sentences per chunk** if you want.
3. Tick **Narrate Kindle with Kokoro** to make Kindle read with Kokoro; untick it
   to hand Kindle back its built-in voice. No admin prompt. A Yes/No prompt confirms
   the change and closes Kindle for you — reopen it afterward to pick up the new voice.
4. Click **Preview** to hear the selected narrator read a short sample line.

Your choices are saved and applied to Kindle's **next page** automatically — no
restart needed.

## Reading Kindle books with Kokoro

1. Make sure **kokoro-kindle-reader is running** (it's the voice engine — no app, no
   sound). It lives in the system tray and auto-starts at login.
2. Tick **Narrate Kindle with Kokoro** in Settings if it isn't already (it's on by
   default). Untick it anytime to restore Kindle's built-in voice. Either way,
   confirming the prompt closes Kindle for you.
3. **Reopen Kindle** so it picks up the new voice.
4. In Kindle, start **Read Aloud** as usual — it now speaks with Kokoro, using the
   narrator, speed, and volume you set in the app.

The installer sets this up for you the first time; the in-app checkbox is for
switching back and forth later.

### Controlling narration from the panel (optional)

Once narration is underway you can **Pause** and **Resume** it from the panel without
switching windows — playback stalls in place and picks up exactly where it left off, so
you never lose your spot.

The panel also has a **Read Aloud** switch that starts and stops Kindle's narration
directly (it briefly brings Kindle to the front to do so) — no need to open Kindle's
**Aa** menu first. It mirrors Read Aloud's current state so it stays in sync with what
you do inside Kindle, whichever side toggles it.

### Tuning Kindle playback

**Sentences per chunk** controls how Kindle narration streams: higher is smoother but
takes slightly longer to start each chunk. Sensible defaults are set, so you usually
don't need to touch it.

## Troubleshooting

- **Kindle is silent / no Read Aloud sound** — the kokoro-kindle-reader app isn't
  running. Start it and try again. (There's no fallback voice by design.)
- **Kindle reverted to the old robotic voice** — make sure **Narrate Kindle with
  Kokoro** is ticked in Settings, confirm the app is running, then reopen Kindle.
- **A switch didn't take effect** — fully close and reopen Kindle after changing
  the voice.
- **The narrator and sliders are greyed out** — the engine isn't ready yet (model
  still downloading, or the launch-time "Checking model files" pass is running) or
  **Narrate Kindle with Kokoro** is unticked. They light up when the status card
  says **Voice Engine Ready** and the box is ticked.
- **Settings shows "Checking model files" for a while after opening** — that's a
  quick integrity check of the downloaded model, normal on every launch. If it finds
  a corrupt file it asks you to click Download to repair it.
- **First run needs a download** — the voice model (~430 MB) fetches once, via the
  Download button in Settings. Everything is offline after that.
- **Narration lags behind pages / synthesis feels slow** — synthesis defaults to
  your GPU, and an integrated GPU (no discrete card) can run slower than realtime.
  Try unticking **Synthesize on GPU** in Settings to synthesize on the CPU instead:
  on one integrated-GPU laptop we tested, plain CPU synthesis was over 2x faster
  than its GPU path. There's no automatic switching yet, so this is a manual
  fallback, not a default.

## How it works

The interesting part is letting 32-bit Kindle narrate with GPU TTS that lives in
a separate 64-bit process: a thin x86 COM voice plugin loads inside Kindle and
forwards each utterance over a named pipe to the kokoro-kindle-reader tray app, which
synthesizes natively on your GPU (Dawn WebGPU) and streams the audio back.

Recent Kindle builds (1.0.18632+) ignore the classic Windows voice setting and pick
their own default, so the app also gives Kindle a nudge to select Kokoro each time it
launches — which is why narration works with no manual voice-switching.

If you're curious about the engine chain, the wire protocol, the Kindle voice
registry/hive details, or want to **build from source**, see
[**ARCHITECTURE.md**](ARCHITECTURE.md). Contributor workflow (getting the source,
CI, releasing) is in [**DEVELOPMENT.md**](DEVELOPMENT.md).
