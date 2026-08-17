# Third-party notices

Kokoro Kindle Reader's own source code is MIT-licensed — see [`LICENSE`](LICENSE) —
apart from a handful of files ported from Apache-2.0 projects, listed below. The
**distributed binaries** (the `-setup.exe` and everything it unpacks) additionally bundle
third-party components, one of which is copyleft. This file is the notice that accompanies
those binaries; the installer places a copy next to the application.

## The short version

`kokoro-host.exe` links **espeak-ng** (GPL-3.0-or-later) and `kokoro-panel.exe` uses
**Slint** under its GPL-3.0-only option. **The installed application as a whole is
therefore conveyed under the GNU General Public License, version 3** — full text in
[`licenses/GPL-3.0.txt`](licenses/GPL-3.0.txt).

This does not restrict the project's own source: its terms are GPL-compatible, so the
repository remains available to you under them. Only the *combined binary* — the project's
own code linked against GPL code — is GPLv3. Those terms are MIT for most of the tree and
Apache-2.0 for the ported files in the next section.

## Source in this repository that is not MIT

Two upstream projects are represented in this repository not as bundled binaries but as
**source**: files here are ports or deliberate simplifications of their code. A derivative
work carries the original's licence, so those files are offered under **Apache-2.0**, not
MIT. Full text: [`licenses/Apache-2.0.txt`](licenses/Apache-2.0.txt).

This changes nothing about how the application may be used or redistributed — Apache-2.0 is
compatible with both MIT and GPLv3 — but attribution and a copy of the licence are
conditions of that permission, and this section is where they are given.

| File(s) | Derived from | License |
|---|---|---|
| `kokoro-host/src/text.rs`, the `phonemize_segment_spans` structure in `kokoro-host/src/espeak.rs`, the style-row rule in `kokoro-host/src/native_synth.rs` | **kokoro-js** | Apache-2.0 |
| `kokoro-ocr/src/detect.rs`, `prep.rs`, `recognize.rs`, `session.rs` | **PaddleOCR** | Apache-2.0 |

### kokoro-js — Apache-2.0

Upstream: <https://github.com/hexgrad/kokoro> (npm `kokoro-js`).

**This library is not a dependency of the project and never was** — it appears in no
`package.json`, lockfile or `Cargo.toml` in any commit. It ran in the WebView2 edition that
was deleted in July 2026, and nothing loads it today. The obligation comes from the *port*,
which is still here and still maintained: Apache-2.0's conditions attach to a derivative
work, and translating upstream's logic into this tree is a stronger form of that than
linking the library would have been. This is an easy notice to lose, because the thing it
covers looks like ordinary project source.

`text.rs` is a behavioural port of that library's text normalization, punctuation
segmentation and phoneme post-processing. It operates on UTF-8 bytes specifically so its
scanning passes reproduce the upstream regexes, and it is verified by token-parity against
the original — it is a translation of that code into Rust, not an independent
implementation. `espeak.rs` mirrors the same library's `PhonemizeSegment` (trace to a file,
fold clause-per-line into one space-joined string), and `native_synth.rs` takes its
style-row selection rule, `clamp(nTokens - 2, 0, 509)`, from its `generate_from_ids`.

### PaddleOCR — Apache-2.0

Upstream: <https://github.com/PaddlePaddle/PaddleOCR>.

`detect.rs` implements a deliberate simplification of PaddleOCR's DBNet post-processing —
connected components and axis-aligned boxes in place of contour fitting and a Vatti polygon
offset — and the threshold, normalization and geometry constants across `kokoro-ocr` are
PaddleOCR's own defaults, which is the configuration the shipped weights were exported and
evaluated under. Each is named against the upstream parameter it comes from in the source
itself. `session.rs`'s dictionary loader follows PaddleOCR's own convention for the
leading empty-sentinel line and the trailing space class. The model files are a separate
matter; see PP-OCR models below.

Each of the four files listed above carries its own header retaining PaddleOCR's copyright
notice (`Copyright (c) 2020 PaddlePaddle Authors. All Rights Reserved.`) and naming what
was ported or modified — this section is the overview, not the notice of record; the
notice of record is in the source.

## Components in the installed application

| Component | Shipped as | License |
|---|---|---|
| **espeak-ng** 1.52.0 (**modified**) | `espeak-ng.dll`, `espeak-ng-data/` | GPL-3.0-or-later |
| **Slint** 1.x | statically linked into `kokoro-panel.exe` | GPL-3.0-only (option chosen) |
| **ONNX Runtime** (WebGPU build) | `onnxruntime.dll`, `onnxruntime_providers_shared.dll` | MIT |
| **Dawn / Tint** | statically linked into `onnxruntime.dll` | BSD-3-Clause |
| **DirectX Shader Compiler** | `dxcompiler.dll`, `dxil.dll` | see below |
| **Rust crates** | statically linked into both `.exe`s and the x86 `.dll`s | mostly MIT OR Apache-2.0 — see below for the full generated closure |
| **Kokoro-82M** model weights | *not shipped* — downloaded on first run | Apache-2.0 |

---

### espeak-ng — GPL-3.0-or-later — **modified**

Upstream: <https://github.com/espeak-ng/espeak-ng>, tag `1.52.0`.
Shipped as `espeak-ng.dll` and the `espeak-ng-data/` directory. Used for phonemization
only (the Kokoro model produces all audio; espeak-ng synthesizes none of it).

**Notice of modification, required by GPLv3 section 5(a):** this project distributes a
*modified* espeak-ng. In July 2026, the phoneme definition `o@` in `phsource/ph_english_us`
was changed from `ɔː` back to `oː`, reverting upstream's "horse-hoarse merger" (upstream
commit `5b01dd86`). The Kokoro-82M model was trained on the output of a pre-merger
espeak-ng, so the revert is required for correct pronunciation of "four", "hoarse",
"shore", and similar words. No other source file is altered.

The modification is applied by
[`native-deps/build-espeak.ps1`](https://github.com/phc260/kokoro-kindle-reader/blob/main/native-deps/build-espeak.ps1)
in this repository, which is both the patch and the build recipe: it checks out tag
`1.52.0`, applies the change above, and builds with
`-DBUILD_SHARED_LIBS=ON -DUSE_ASYNC=OFF -DUSE_MBROLA=OFF -DUSE_LIBSONIC=OFF -DUSE_LIBPCAUDIO=OFF -DESPEAK_BUILD_DOC=OFF`.
Running it against a fresh upstream clone reproduces the modified source and the build
configuration used for the shipped `espeak-ng.dll` and `espeak-ng-data/`. (It does not
promise a byte-identical DLL: the script builds with whatever MSVC toolchain
`vswhere -latest` finds, and the build is not otherwise pinned or hash-verified.)
See "Obtaining corresponding source" below.

Parts of the espeak-ng tree carry additional licenses: the `getopt.c` Windows
compatibility shim is 2-clause BSD
([`licenses/espeak-ng-BSD-2-Clause.txt`](licenses/espeak-ng-BSD-2-Clause.txt)), and the
tree also includes Apache-2.0 and Unicode (UCD) licensed data. See `COPYING*` in the
upstream repository.

### Slint — GPL-3.0-only

Upstream: <https://github.com/slint-ui/slint>. Statically linked into `kokoro-panel.exe`.

Slint is offered under `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR
LicenseRef-Slint-Software-3.0`. **This project uses Slint under the GPL-3.0-only
option**, which the application already satisfies by way of espeak-ng. Slint is used
unmodified.

### ONNX Runtime (WebGPU) — MIT

Upstream: <https://github.com/microsoft/onnxruntime>. Copyright (c) Microsoft
Corporation. Shipped unmodified as `onnxruntime.dll` and
`onnxruntime_providers_shared.dll`, obtained from the official `onnxruntime-webgpu`
Python wheel.

> Permission is hereby granted, free of charge, to any person obtaining a copy of this
> software and associated documentation files (the "Software"), to deal in the Software
> without restriction, including without limitation the rights to use, copy, modify,
> merge, publish, distribute, sublicense, and/or sell copies of the Software, and to
> permit persons to whom the Software is furnished to do so, subject to the following
> conditions:
>
> The above copyright notice and this permission notice shall be included in all copies
> or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
> INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
> PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
> HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
> CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE
> OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

ONNX Runtime itself bundles further third-party code — including **Dawn** and **Tint**
(BSD-3-Clause, <https://dawn.googlesource.com/dawn>), which implement the WebGPU
execution provider this application runs on; Dawn's licence, which also covers Tint, is in
[`licenses/dawn-BSD-3-Clause.txt`](licenses/dawn-BSD-3-Clause.txt).

**ONNX Runtime's own licence and notice files ship in `licenses\onnxruntime\` beside the
installed application**, taken verbatim from the same wheel the DLLs came out of. (They are
provisioned rather than checked in, so that directory exists in an install and not in the
source tree.) That is the authoritative and complete list of what ORT bundles — far
more than this file enumerates — and provisioning it with the binaries is what keeps it
matched to the exact build being shipped. `native-deps/fetch-deps.ps1` retains it and
`packaging/build-installer.ps1` stages it; both fail loudly rather than ship the DLLs with
no notices.

### DirectX Shader Compiler — `dxcompiler.dll`, `dxil.dll`

Upstream: <https://github.com/microsoft/DirectXShaderCompiler>. Copyright (c) Microsoft
Corporation. Redistributed unmodified, exactly as obtained from the official
`onnxruntime-webgpu` wheel; required by the WebGPU execution provider to compile
shaders. The DirectX Shader Compiler is published under the University of
Illinois/NCSA Open Source License — full text, including the licences of the components it
in turn bundles, in [`licenses/dxcompiler-NCSA.txt`](licenses/dxcompiler-NCSA.txt). That
licence requires its notice accompany binary redistributions, which is why the text is here
and not merely named. `dxil.dll` is the DirectX Shader Compiler's validator binary from the
same official redistributable package as `dxcompiler.dll`; Microsoft's terms for that
package (the `Microsoft.Direct3D.DXC` redistributable) state that `LICENSE-LLVM.txt`
applies to all other files in it, which covers `dxil.dll` too. It is therefore covered by
the same `licenses/dxcompiler-NCSA.txt` text above, not a separate licence.

### Google Material Symbols — Apache-2.0

Upstream: <https://github.com/google/material-design-icons>. Copyright Google Inc. The
settings panel's icon buttons use glyphs from the Material Symbols (Rounded) set, checked
into `kokoro-panel/ui/` as SVG and compiled into `kokoro-panel.exe`: `play-arrow`,
`pause`, `stop`, `sprint`, and `resume`. Licensed under the Apache License, Version 2.0
([`licenses/Apache-2.0.txt`](licenses/Apache-2.0.txt)). `resume` is **modified** — its left bar
is lengthened past the play triangle; the rest are used unmodified apart from being
recoloured at runtime.

### PP-OCR models (Cloud Reader OCR) — Apache-2.0

Two ONNX models and a character dictionary, **installed with the application** into
`ocr\` beside the executables, where `kokoro-host` loads them to recognize Kindle Cloud
Reader pages. They are PP-OCR models originating with PaddleOCR
(<https://github.com/PaddlePaddle/PaddleOCR>) and are redistributed here unmodified,
pinned by SHA-256 and fetched at build time by
[`native-deps/fetch-ocr-models.ps1`](https://github.com/phc260/kokoro-kindle-reader/blob/main/native-deps/fetch-ocr-models.ps1)
— no copy of the weights is checked into this repository.

- **`det.onnx`** — the PP-OCRv3 English text *detector*, taken from
  <https://huggingface.co/SWHL/RapidOCR>.
- **`rec.onnx`** and **`en_dict.txt`** — the PP-OCRv5 English mobile text *recognizer* and
  its character dictionary, taken from
  <https://github.com/PT-Perkasa-Pilar-Utama/ppu-paddle-ocr-models>.

All three are licensed under the Apache License, Version 2.0
([`licenses/Apache-2.0.txt`](licenses/Apache-2.0.txt)), as are both redistributing projects
and PaddleOCR itself.

### Rust crates — MIT OR Apache-2.0, plus a generated closure report

The two executables and the three x86 libraries statically link a number of crates from
crates.io — including `ort`, `windows`/`windows-sys`, `serde`, `tray-icon`, `cpal`, and
their transitive dependencies. Most are permissively licensed (typically
`MIT OR Apache-2.0`), but a full lockfile is hundreds of transitive crates, and a
hand-written list of "the unusual ones" is exactly what went stale in an earlier version
of this section: four crates it described as already-covered `OR` alternatives turned out
to be sole-licensed under terms this file shipped no text for. **This section no longer
tries to enumerate the closure by hand.**

Instead, [`packaging/generate-dependency-licenses.ps1`](packaging/generate-dependency-licenses.ps1)
runs [`cargo about`](https://github.com/EmbarkStudios/cargo-about) against the exact
`Cargo.lock` each shipped binary was built from — `kokoro-host` and `kokoro-panel` for
`x86_64-pc-windows-msvc`, `kokoro-sapi`/`kokoro-hook`/`kokoro-inject` for
`i686-pc-windows-msvc` (`kokoro-ocr` and `kokoro-protocol` are path dependencies and so
are covered by whichever binary links them) — and renders every crate's resolved licence
text, grouped by licence, into one HTML notice per binary. `packaging/about.toml` is the
list of licences this project has reviewed and accepts (`MIT`, `Apache-2.0`,
`Unicode-3.0`, `ISC`, `Zlib`, `BSL-1.0`, `BSD-2-Clause`, `BSD-3-Clause`,
`CDLA-Permissive-2.0`, and `GPL-3.0-only` for Slint under the option chosen above); a
dependency whose licence isn't on that list makes generation **fail the build** rather
than ship silently uncovered — that's the mechanism for "a new licence category showed
up," not a person re-reading the whole tree by hand.

`packaging/build-installer.ps1` runs the generator on every build (not once, provisioned —
the Rust closure moves with ordinary `cargo update`s in a way a pinned wheel doesn't) and
stages its output into `licenses\dependencies\` beside the installed application. That
directory, like `licenses\onnxruntime\`, exists in an install and not in this source tree.
To reproduce it yourself: `cargo install cargo-about --locked --features cli`, then
`packaging\generate-dependency-licenses.ps1`.

Two terms in that closure are **not** alternatives you can decline by picking MIT or
Apache-2.0, so their text has to ship on its own:

- `unicode-ident` — a dependency of six of the seven crate lockfiles, and so of nearly
  every binary here — is `(MIT OR Apache-2.0) AND Unicode-3.0`. The `AND` is the point:
  choosing Apache-2.0 does not discharge the Unicode licence, whose text is in
  [`licenses/Unicode-3.0.txt`](licenses/Unicode-3.0.txt).
- Several crates reached through Slint carry a **sole** licence with no MIT/Apache-2.0
  alternative at all — despite an earlier version of this section describing them as
  already-covered `OR` alternatives, which was wrong and has been corrected:
  `untrusted` (**ISC**), `slotmap` and `foldhash` (**Zlib**), `webpki-roots`
  (**CDLA-Permissive-2.0**). Their required notice text is part of the generated
  dependency-licence material described above, not the hand-written text in this file.
  `ryu` (`Apache-2.0 OR BSL-1.0`) is the one crate here that genuinely is a covered `OR`
  alternative.

Slint, listed separately above, is the one dependency in this set that is **not**
permissively licensed.

### Kokoro-82M — Apache-2.0 — *not shipped*

Model: <https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX> (an ONNX conversion
of <https://huggingface.co/hexgrad/Kokoro-82M>). The weights and voice embeddings are
**not** included in the installer — the settings panel downloads them from Hugging Face
when you click **Download**, into your own user profile, per the checksums in
[`model-manifest.json`](https://github.com/phc260/kokoro-kindle-reader/blob/main/model-manifest.json).
They are licensed Apache-2.0 by their authors
([`licenses/Apache-2.0.txt`](licenses/Apache-2.0.txt)), and your use of them is governed by
that license and by Hugging Face's terms.

---

## Obtaining corresponding source

For the GPL-licensed components in any binary release, the complete corresponding source
is available at no charge:

- **This application's source:** <https://github.com/phc260/kokoro-kindle-reader>, at the
  tag matching the release version.
- **espeak-ng:** upstream tag `1.52.0` from <https://github.com/espeak-ng/espeak-ng>,
  plus the modification and build flags in `native-deps/build-espeak.ps1` (see above).
  `native-deps/fetch-deps.ps1` performs the whole provisioning step.
- **Slint:** <https://github.com/slint-ui/slint>, at the version recorded in
  `kokoro-panel/Cargo.lock`. Used unmodified.

If you would prefer these on physical media, open an issue on the repository above.
