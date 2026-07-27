# Third-party notices

Kokoro Kindle Reader's own source code is MIT-licensed — see [`LICENSE`](LICENSE).
The **distributed binaries** (the `-setup.exe` and everything it unpacks) additionally
bundle third-party components, one of which is copyleft. This file is the notice that
accompanies those binaries; the installer places a copy next to the application.

## The short version

`kokoro-host.exe` links **espeak-ng** (GPL-3.0-or-later) and `kokoro-panel.exe` uses
**Slint** under its GPL-3.0-only option. **The installed application as a whole is
therefore conveyed under the GNU General Public License, version 3** — full text in
[`licenses/GPL-3.0.txt`](licenses/GPL-3.0.txt).

This does not restrict the project's own source: MIT is GPL-compatible, so every file
in this repository remains available to you under MIT terms. Only the *combined binary*
— MIT code linked against GPL code — is GPLv3.

## Components in the installed application

| Component | Shipped as | License |
|---|---|---|
| **espeak-ng** 1.52.0 (**modified**) | `espeak-ng.dll`, `espeak-ng-data/` | GPL-3.0-or-later |
| **Slint** 1.x | statically linked into `kokoro-panel.exe` | GPL-3.0-only (option chosen) |
| **ONNX Runtime** (WebGPU build) | `onnxruntime.dll`, `onnxruntime_providers_shared.dll` | MIT |
| **Dawn / Tint** | statically linked into `onnxruntime.dll` | BSD-3-Clause |
| **DirectX Shader Compiler** | `dxcompiler.dll`, `dxil.dll` | see below |
| **Rust crates** | statically linked into both `.exe`s and the x86 `.dll`s | MIT OR Apache-2.0 |
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
execution provider this application runs on. See ONNX Runtime's own
`ThirdPartyNotices.txt` in the upstream repository for the complete list.

### DirectX Shader Compiler — `dxcompiler.dll`, `dxil.dll`

Upstream: <https://github.com/microsoft/DirectXShaderCompiler>. Copyright (c) Microsoft
Corporation. Redistributed unmodified, exactly as obtained from the official
`onnxruntime-webgpu` wheel; required by the WebGPU execution provider to compile
shaders. The DirectX Shader Compiler is published under the University of
Illinois/NCSA Open Source License; `dxil.dll` is a Microsoft-signed validator component
redistributed under the terms accompanying its official binary release.

### Rust crates — MIT OR Apache-2.0

The two executables and the three x86 libraries statically link a number of crates from
crates.io — including `ort`, `windows`/`windows-sys`, `serde`, `tray-icon`, `cpal`, and
their transitive dependencies. All are used unmodified, and those checked individually
are permissively licensed (typically `MIT OR Apache-2.0`); the tree as a whole has not
been audited crate by crate, and this notice does not assert a license for every
transitive dependency. Each crate's `Cargo.lock` records the authoritative *list* of
crates and versions — it does not record their licenses. To enumerate the licenses
themselves, read the crates' own manifests, or run `cargo license` / `cargo about` in a
crate directory.

Slint, listed separately above, is the one dependency in this set that is **not**
permissively licensed.

### Kokoro-82M — Apache-2.0 — *not shipped*

Model: <https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX> (an ONNX conversion
of <https://huggingface.co/hexgrad/Kokoro-82M>). The weights and voice embeddings are
**not** included in the installer — the settings panel downloads them from Hugging Face
when you click **Download**, into your own user profile, per the checksums in
[`model-manifest.json`](https://github.com/phc260/kokoro-kindle-reader/blob/main/model-manifest.json).
They are licensed Apache-2.0 by their authors, and your use of them is governed by that
license and by Hugging Face's terms.

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
