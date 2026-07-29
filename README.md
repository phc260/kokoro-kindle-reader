<div align="center">

<img src="icons/128x128@2x.png" alt="Kokoro Kindle Reader" width="120" height="120">

# Kokoro Kindle Reader

**Give Kindle for PC a natural voice — local, offline Kokoro-82M text-to-speech, running on your own GPU.**

[![Platform: Windows](https://img.shields.io/badge/platform-Windows-1976D2?logo=windows&logoColor=white)](#install)
[![TTS: Kokoro-82M](https://img.shields.io/badge/TTS-Kokoro--82M-E91E63)](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX)
[![Language: English](https://img.shields.io/badge/language-English-673AB7)](#)
[![100% Offline](https://img.shields.io/badge/100%25-offline-43A047)](#)
[![Source: MIT](https://img.shields.io/badge/source-MIT-009688)](LICENSE)
[![Binaries: GPLv3](https://img.shields.io/badge/binaries-GPLv3-00695C)](THIRD_PARTY_NOTICES.md)
[![Latest release](https://img.shields.io/github/v/release/phc260/kokoro-kindle-reader?include_prereleases&label=release&color=F4511E)](https://github.com/phc260/kokoro-kindle-reader/releases)

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
   **Settings**, and click **Download** to fetch the voice model (~340 MB, one time).
   After that it works fully offline.

The app synthesizes on your GPU via WebGPU, so a **discrete GPU** (e.g. NVIDIA/AMD)
gives smooth, faster-than-realtime narration — tested smooth on an NVIDIA GTX 1060.
Laptops with only an **integrated GPU and no dedicated one** can fall well behind
realtime — narration will work, but may lag noticeably behind Kindle's pages. If so,
click the **runner** button beside **Synthesize on GPU** in Settings: it times your GPU
and your CPU and picks the faster one for you (see [GPU or CPU?](#gpu-or-cpu)).

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
5. Not sure whether to leave **Synthesize on GPU** ticked? Click the **runner** button
   next to it — it times both and picks the faster one for your PC. See
   [GPU or CPU?](#gpu-or-cpu).

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

### GPU or CPU?

Kokoro can synthesize on your graphics card or on your processor, and **which one is
faster depends entirely on your PC** — on a laptop with only an integrated GPU, the
processor can be twice as fast; with a discrete graphics card it's usually the other way
round. You can't tell from the hardware name, so don't guess: click the **runner** button
next to **Synthesize on GPU** ("Test which is faster" when you hover it).

The test speaks the same short sentence on each engine, times them, and ticks the faster
one. Nothing is played aloud. It usually takes under a minute — longer on a slow PC, since
how long synthesis takes is the thing being measured — and it needs the synthesizer to
itself, so **stop Read Aloud first**. The results are shown as a *real time* figure: 2.0x
means Kokoro produces two seconds of speech per second, so it comfortably keeps ahead of
your reading; below 1.0x it can't keep up and narration will pause to catch up.

You can still tick or untick **Synthesize on GPU** by hand at any time; the test only
sets it for you.

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
- **First run needs a download** — the voice model (~340 MB) fetches once, via the
  Download button in Settings. Everything is offline after that.
- **Narration lags behind pages / synthesis feels slow** — synthesis defaults to
  your GPU, and an integrated GPU (no discrete card) can run slower than realtime.
  Click the **runner** button beside **Synthesize on GPU** in Settings: it times both
  engines on your machine and ticks the faster one (on one integrated-GPU laptop we tested, plain CPU synthesis was over
  2x faster than its GPU path). Stop Read Aloud before running it. See
  [GPU or CPU?](#gpu-or-cpu).

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

## Licensing

This project's own source code is **MIT** — see [LICENSE](LICENSE). Reuse any of it on
MIT terms.

The **installed app** is a different question, because it bundles third-party pieces:
it links [espeak-ng](https://github.com/espeak-ng/espeak-ng) (GPL-3.0-or-later, and
patched here for phoneme parity with the model) and [Slint](https://slint.dev) under its
GPL-3.0 option. **The binaries in a release are therefore conveyed under the GNU GPL
version 3.** That's the normal outcome of MIT code linking a GPL library — it doesn't
restrict the source in this repository, only the combined binary.

Full component list, the required notice of modification to espeak-ng, and where to get
corresponding source: [**THIRD_PARTY_NOTICES.md**](THIRD_PARTY_NOTICES.md). The GPLv3
text is in [licenses/](licenses/), and the installer places both next to the app.

The **Kokoro-82M voice model** is Apache-2.0 and is *not* bundled — the app fetches it
from Hugging Face when you click **Download** in Settings.
