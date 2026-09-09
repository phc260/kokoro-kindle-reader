# Licensing — the authoritative map

This is the single source of truth for how Kokoro Kindle Reader is licensed: the
per-artifact map, the one judgment call it rests on, and how to obtain corresponding
source. [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) is the notice that ships with the
binaries (the human-readable prose + the licence texts it points at);
[`packaging/components.toml`](packaging/components.toml) is the machine-checkable inventory
of components outside Cargo's package graph; the Cargo closure is generated per build by
`cargo-about`. This file ties them together.

## The one-line version

- **The repository source is MIT**, except for a handful of files ported from Apache-2.0
  projects (which retain Apache-2.0 — see below).
- **The distributed binaries are conveyed under GPL-3.0-only**, because they link modified
  espeak-ng (GPL-3.0-or-later) and Slint under its GPL-3.0-only option.
- MIT source linking into a GPL binary is the standard, valid arrangement: copyleft governs
  distribution of the *combined binary*, not the licensing of the source files fed into it.
  Taking the source, you get MIT (+ the Apache islands); taking the binary, you get GPLv3.

The combined binary is **GPL-3.0-only**, not "or-later": Slint's GPL option is
`GPL-3.0-only`, so the aggregate cannot be offered "or-later". Keep documentation off
"or-later" for the app.

## Per-artifact map (authoritative)

| Artifact | Conveyed under | Why |
|---|---|---|
| `kokoro-host.exe` | **GPL-3.0-only** combined work | Links modified espeak-ng (GPL-3.0-or-later); retains MIT/Apache-2.0 dependency terms within. |
| `kokoro-panel.exe` | **GPL-3.0-only** combined work | Statically links Slint under its GPL-3.0-only option. |
| `espeak-ng.dll` + `espeak-ng-data/` | **GPL-3.0-or-later**, modified | Modification noticed per GPLv3 §5(a); ships its own `COPYING*` incl. UCD/Apache/BSD2. |
| `KokoroSapi.dll`, `kokoro_hook.dll`, `kokoro-inject.exe` (x86) | **MIT** (see §"the boundary") | Connect-only IPC clients; link no GPL code. |
| `onnxruntime.dll`, `onnxruntime_providers_shared.dll` | **MIT** | ONNX Runtime, unmodified. |
| Dawn / Tint (inside ORT) | **BSD-3-Clause** | Statically linked by ORT. |
| `dxcompiler.dll`, `dxil.dll` | **NCSA + bundled third-party terms** | From ORT-webgpu wheel; the complete upstream `LICENSE.TXT` is shipped. |
| OCR models + `en_dict.txt` | **Apache-2.0**, *not shipped* | Downloaded at first run (like Kokoro-82M); not in the installer. |
| Material Symbols SVGs (compiled into panel) | **Apache-2.0**, `resume.svg` modified | In-file change notices; see `components.toml`. |
| `icon.ico` | **MIT** | This project's own art. |
| Cargo crate closure | **MIT OR Apache-2.0** + ISC/Zlib/BSL-1.0/BSD/W3C-20150513/Unicode-3.0/CDLA-Permissive-2.0 | Enumerated per build by `cargo-about`, with exact embedded-source notices appended. |
| Rust Standard Library | Primarily **MIT OR Apache-2.0**, with bundled code under additional terms | Statically linked into every Rust output; exact toolchain report staged from `rustc` as `licenses/rust/COPYRIGHT-library.html`. |
| Installer / uninstaller stub | **NSIS license** (Zlib + bzip2 + CPL-1.0-w/-exception) | LZMA-compressed NSIS stub. |
| Kokoro-82M weights | **Apache-2.0**, *not shipped* | Runtime download. |
| Repository source | **MIT**, except Apache-2.0 ported files | Alan's grant; ports retain upstream licence. |

### Source in this repository that is not MIT (Apache-2.0 islands)

- `kokoro-host/src/text.rs` — a behavioural port of **kokoro-js** (Apache-2.0); whole file.
- `kokoro-host/src/espeak.rs` — the `phonemize_segment_spans` structure is from kokoro-js;
  the rest is original MIT (file marked `MIT AND Apache-2.0`).
- `kokoro-host/src/native_synth.rs` — the style-row rule `clamp(nTokens-2, 0, 509)` is from
  kokoro-js; the rest is original MIT (file marked `MIT AND Apache-2.0`).
- `kokoro-ocr/src/detect.rs`, `prep.rs`, `recognize.rs`, `session.rs` — simplifications of
  **PaddleOCR**'s DBNet post-processing / conventions (Apache-2.0).
- `kokoro-panel/ui/*.svg` — five **Google Material Symbols** (Apache-2.0), compiled into
  `kokoro-panel.exe` by `slint-build`. `resume.svg` is modified (left bar lengthened) and
  carries an in-file change notice; the other four are unmodified. Per-glyph provenance +
  SHA-256 in [`packaging/components.toml`](packaging/components.toml).

The ported/derived Rust files and the **modified** `resume.svg` each carry an in-file
provenance + change notice (Apache-2.0 §4(b), which applies to *modified* files). The four
**unmodified** Material Symbols SVGs need no §4(b) header; their attribution is
`packaging/components.toml` (per-glyph origin + SHA-256) plus `THIRD_PARTY_NOTICES.md` and
the shipped `licenses/Apache-2.0.txt`. The crate `Cargo.toml`s declare `MIT AND Apache-2.0`
for `kokoro-host`/`kokoro-ocr`/`kokoro-panel` (the first two embed the ported Rust files, the
last the Material Symbols SVGs) and `MIT` for the rest.

**Apache-2.0 §4(d) (NOTICE reproduction):** checked 2026-08-26 — none of the upstream
Apache-2.0 projects (`hexgrad/kokoro`, `PaddlePaddle/PaddleOCR`, `google/material-design-icons`)
ships a separate `NOTICE` file; each has only a `LICENSE`. So there is no upstream NOTICE
text to reproduce, and the per-file change notices plus the shipped `licenses/Apache-2.0.txt`
discharge §4. Re-check only if a new port is added from a project that *does* carry a NOTICE.

## The boundary (the one judgment call)

The x86 components (`KokoroSapi.dll`, `kokoro_hook.dll`, `kokoro-inject.exe`) are
**connect-only**: they forward `Speak` to `kokoro-host` over a named pipe and link neither
espeak-ng nor Slint. Separate processes communicating at arm's length are, under the FSF's
long-standing reading, *mere aggregation* — not a single combined work — so these
components are **not derivative works of any GPL code** and remain **MIT** inside the
aggregate installer (GPLv3 §§5–6 preserve independent works placed in an aggregate).

This project takes option **(b)**: the precise per-binary map above, which preserves MIT
reuse of the x86 clients. The conservative fallback **(a)** — declaring the whole bundle
GPLv3 — costs nothing and is never under-inclusive, and is what to fall back to if a
reviewer contests the boundary. This is the only line a reviewer could reasonably contest;
it is not required to ship, but it is the one place where a lawyer's confirmation would be
worth having.

## Obtaining corresponding source (GPLv3 §6)

The GPL-covered binaries (`kokoro-host.exe`, `kokoro-panel.exe`, `espeak-ng.dll` +
`espeak-ng-data/`) are conveyed with complete corresponding source. Each binary release
carries a `corresponding-source-X.Y.Z.zip` beside the installer, built by
[`packaging/build-corresponding-source.ps1`](packaging/build-corresponding-source.ps1) and
linked from the release body (GPLv3 §6(d): equivalent access from the same place, with clear
directions). Its contents are listed in that script and in `THIRD_PARTY_NOTICES.md`. The
archive contains the project source at the matching tag, the modified espeak-ng tree, and
the exact `rust-src` Standard Library `library/` tree from the toolchain that built all five
Rust outputs, plus the exact official NSIS 3.12 source archive, lockfiles/build scripts, and
immutable toolchain commit. A §6 recipient
therefore does not depend on a mutable upstream tag for either modified espeak-ng or the
statically linked standard library.

## How this is enforced

- **`cargo-about --fail`** (`packaging/generate-dependency-licenses.ps1`, run by
  `build-installer.ps1` and in CI) refuses any Cargo dependency whose licence is not in
  `packaging/about.toml`'s `accepted` list. `GPL-3.0-only` is accepted **only** for the
  Slint crates, so a GPL dependency arriving through anything else fails the build. The
  generator also appends exact packaged licence/notice files and leading source copyright
  comments, with SHA-256s. `about.toml` clarifications recover upstream notices omitted
  from published packages, including Taffy's Visly attribution and AccessKit's additional
  Chromium BSD terms. `source-notices.json` pins the complete W3C notices in Tao,
  Winit and cursor-icon, and Intel's ISC notice in Ring's native P-384 implementation;
  it rejects source drift or a new package version until reviewed.
  `verify-dependency-licenses.ps1` requires every configured text's
  hash in each affected crate/version's rendered report. It also checks every ordinary
  appendix text and its recorded block count, catching changed or removed packaged
  notices. These checks run before staging and after installer
  extraction. This is separate from `--fail`: cargo-about 0.9.1 only warns when a
  clarification cannot be retrieved or validated, then falls back to generic text.
- **The Rust Standard Library is provisioned from the build toolchain**, not inferred from
  Cargo metadata. `build-installer.ps1` requires and stages that rustc sysroot's generated
  `COPYRIGHT-library.html` plus its release/commit; `build-corresponding-source.ps1` requires
  `rust-src`, requires the current toolchain to equal the installer's staged `TOOLCHAIN.txt`,
  and includes the exact `library/` source tree. CI installs `rust-src` explicitly.
- **Native cache provenance is fail-closed.** ORT's marker identifies the exact CPython 3.12
  Windows wheel and reviewed PyPI SHA-256 (the release-number-only wheels have different native
  DLL bytes); espeak carries an exact recipe marker, and installer staging reads the marked
  cache directly. The modified espeak build
  marker includes `build-espeak.py`'s normalized SHA-256, so a patch/build-recipe change
  forces a rebuild. It also records the source tree's SHA-256 manifest; corresponding-source
  packaging refuses to pair the binary with a source tree that has changed since the build.
- **Project source is paired with the binaries, including under `-SkipBuild`.** A successful
  full installer build records SHA-256s for every tracked source file and both x64 executables
  plus the Rust toolchain. Reuse refuses a mismatch even if a standalone build overwrote an
  executable. The installer freezes these records and espeak's source manifest in its staging
  tree, so corresponding-source packaging cannot use a later build or provision's records.
- **The NSIS pin is checked locally as well as installed by CI.** `build-installer.ps1`
  requires `makensis /VERSION` to report 3.12 before it packages the stub and stages that
  toolchain's `COPYING`; a different local NSIS cannot be mislabeled as the inventoried one.
  The matching source package downloads the exact official NSIS 3.12 source archive and
  rejects it unless its SHA-256 matches the reviewed value, satisfying the CPL LZMA module's
  source-availability condition without depending only on a mutable web page.
- **Appropriate legal notices are accessible from both interactive interfaces.**
  **About & licenses** in the tray and Settings opens the installed `legal.html`,
  including while the host/model is unavailable. It states the copyright, warranty
  exclusion and GPL redistribution rights, links the local GPL text and component
  notices, and directs users to the corresponding-source release downloads.
- **The installer-extraction test** in CI unpacks the produced `-setup.exe` and asserts the
  whole notice tree is present and non-empty (LICENSE, THIRD_PARTY_NOTICES.md, the licence
  texts, ORT/espeak/NSIS/Rust-toolchain notices, and all five generated per-binary Cargo
  reports with their exact packaged-licence appendices). The fixed checked-in licence texts
  and `THIRD_PARTY_NOTICES.md` plus the UI's `legal.html` are also content-checked against
  `packaging/license-texts.sha256`, both before the build and inside the extracted installer;
  presence alone would not catch truncation or a copy from the wrong upstream revision.
  Every local link from `legal.html` must resolve to a non-empty installed file.
- **Every binary-bearing CI artifact is complete.** `installer.yml` pairs the installer with
  corresponding source even on a manual, non-release run; `sapi.yml` build-tests the SAPI DLL
  but does not upload that intermediate binary without its notices.
- **`components.toml`** is the checked-in inventory of every shipped component the Cargo
  gate cannot see.

## Audit log

### 2026-08-26 — v0.3.3 assessed (GPLv3 §6 / Apache §4 shortfall) — cure pending

The published **v0.3.3** installer (SHA-256
`b0411df768801857c52107f5c5341a2ba8469df3fe5eef2a9d799a7cb03173fa`) was downloaded and
extracted. It predates most of the notice machinery and **is short**:

- Its `THIRD_PARTY_NOTICES.md` links `licenses/LICENSE-2.0` (an Apache-2.0 text) that **was
  never shipped** — a dangling reference to a required licence copy.
- It ships only `licenses/GPL-3.0.txt` and `licenses/espeak-ng-BSD-2-Clause.txt`. Missing:
  the **Apache-2.0** text (for the ported files and the OCR/model attribution), the **NCSA**
  text (for the redistributed `dxcompiler.dll`/`dxil.dll`), **Dawn/Tint BSD-3-Clause**,
  **Unicode-3.0** (`unicode-ident`'s `AND` clause), espeak-ng's `COPYING.APACHE`/`COPYING.UCD`,
  the ORT wheel's own notice set, and the Rust dependency-closure reports.
- No corresponding-source archive accompanies it.

**Required maintainer follow-up — still pending:** a corrected new release does not supply
the source or missing notices for this older binary. Before continuing to offer v0.3.3,
remediate that exact distribution: supply its complete matching corresponding source and
all applicable notices, with clear access beside the binary. If that cannot be established,
stop offering the deficient binary while remediation is worked out. Merely marking it
"superseded" and linking to a different version's source is not sufficient. This follows
[GPLv3 §6(d)](https://www.gnu.org/licenses/gpl-3.0.en.html#section6) and the
[GNU FAQ's source/binary correspondence requirement](https://www.gnu.org/licenses/gpl-faq.html#SourceAndBinaryOnDifferentSites).

Publish future binaries only through the hardened pipeline, paired with their own source
archives. Withdrawal prevents further deficient downloads; it does not itself resolve
obligations to prior recipients or establish reinstatement under GPLv3 §8. Assess that
history and any rights-holder notices with qualified counsel rather than declaring the
violation cured by a newer release. Do **not** rewrite source tags. Review stale draft
binary assets too (`installer.yml` publishes as `draft: true`). No release assets have been
changed by this repository audit.
