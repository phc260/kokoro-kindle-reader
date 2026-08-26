# Licensing — the authoritative map

This is the single source of truth for how Kokoro Kindle Reader is licensed: the
per-artifact map, the one judgment call it rests on, and how to obtain corresponding
source. [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) is the notice that ships with the
binaries (the human-readable prose + the licence texts it points at);
[`packaging/components.toml`](packaging/components.toml) is the machine-checkable inventory
of the non-Rust payload; the Rust closure is generated per build by `cargo-about`. This
file ties them together.

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
| `dxcompiler.dll`, `dxil.dll` | **NCSA** | From ORT-webgpu wheel. |
| OCR models + `en_dict.txt` | **Apache-2.0** | Data, aggregated (not linked). |
| Material Symbols SVGs (compiled into panel) | **Apache-2.0**, `resume.svg` modified | In-file change notices; see `components.toml`. |
| `icon.ico` | **MIT** | This project's own art. |
| Rust crate closure | **MIT OR Apache-2.0** + ISC/Zlib/BSL-1.0/BSD/Unicode-3.0/CDLA-Permissive-2.0 | Enumerated per build by `cargo-about`. |
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

Each file carries an in-file provenance + change notice (Apache-2.0 §4(b)); the crate
`Cargo.toml`s declare `MIT AND Apache-2.0` for `kokoro-host`/`kokoro-ocr` and `MIT` for the
rest.

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
carries a `corresponding-source-vX.Y.Z.zip` beside the installer, built by
[`packaging/build-corresponding-source.ps1`](packaging/build-corresponding-source.ps1) and
linked from the release body (GPLv3 §6(d): equivalent access from the same place, with clear
directions). Its contents are listed in that script and in `THIRD_PARTY_NOTICES.md`. The
project source at the matching tag on GitHub is itself corresponding source for the Rust
portion; the archive additionally pins the modified espeak-ng tree and the exact
lockfiles/build scripts so a §6 recipient does not depend on an upstream tag remaining
reachable.

## How this is enforced

- **`cargo-about --fail`** (`packaging/generate-dependency-licenses.ps1`, run by
  `build-installer.ps1` and in CI) refuses any Rust dependency whose licence is not in
  `packaging/about.toml`'s `accepted` list. `GPL-3.0-only` is accepted **only** for the
  Slint crates, so a GPL dependency arriving through anything else fails the build.
- **The installer-extraction test** in CI unpacks the produced `-setup.exe` and asserts the
  whole notice tree is present and non-empty (LICENSE, THIRD_PARTY_NOTICES.md, the licence
  texts, ORT/espeak/NSIS notices, and the generated per-binary Rust reports).
- **`components.toml`** is the checked-in inventory of every non-Rust shipped file the Rust
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

**Recommended cure (GPLv3 §8 path — a maintainer action, not automatable here):** build one
compliant release through the hardened pipeline above (the licence gate now runs, the
notice tree is verified, and `corresponding-source-*.zip` is attached), publish it, then add
a prominent "superseded by vX.Y.Z — complete notices/source there" note to the v0.3.3
release. Withdrawing v0.3.3 is optional once a compliant release exists and points to it. Do
**not** rewrite tags — source tags need no binary remediation. Also clear any stale **draft**
releases (CI publishes as `draft: true`).
