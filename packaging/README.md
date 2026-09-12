# packaging — the standalone NSIS installer

Builds `kokoro-kindle-reader-X.Y.Z-setup.exe`. **Standalone NSIS** via `makensis` — *not*
a Tauri bundler.

```powershell
.\packaging\build-installer.ps1
```

CI runs this on a `v*` tag or manual dispatch (`.github/workflows/installer.yml`). Its Actions
artifact always carries the setup.exe and corresponding-source archive together; a tag also
drafts a GitHub Release with both attached — see [`../DEVELOPMENT.md`](../DEVELOPMENT.md).

## Layout

| File | What |
|---|---|
| `build_installer.py` | Release-builds both x64 crates, builds the three x86 artifacts, records source/toolchain provenance, stages everything into `staging\`, then runs `makensis`. `-SkipBuild` accepts only matching recorded outputs. |
| `installer.nsi` | The NSIS script: install/uninstall sections, the elevation hooks, the Run value, the model-deletion prompt. Carries the product `VERSION`. |
| `generate_dependency_licenses.py` | Runs `cargo about --locked` against each shipped crate's `Cargo.lock` and appends exact packaged licence files and source copyright headers; called automatically by `build-installer.ps1`. Needs `cargo install cargo-about --locked --features cli` once. |
| `about.toml`, `about.hbs` | `cargo-about`'s config (the accepted-licence list; `GPL-3.0-only` granted per-crate to Slint only) and output template. |
| `verify_dependency_licenses.py`, `test_dependency_licenses.py` | Verify every appendix text hash and block count, plus clarification/embedded-source hashes per crate/version (also inside the extracted installer); 26 offline regression fixtures, driven in CI through the PS 5.1 harness. |

**These four are Python now, and the coupling that made them one unit is gone.**
PowerShell cannot import a script without executing it, so `test-dependency-licenses.ps1`
used to **parse `generate-dependency-licenses.ps1`'s AST**, find two functions by name and
dot-source their extents -- which meant renaming a function in the generator broke the
tests somewhere that never mentioned it, and none of the four could be moved or reorganized
without the other three. `test_dependency_licenses.py` imports them instead. What remains
is ordinary: `source_notices.py` is a library, `generate_` calls `verify_` after each
crate, and `dotnet_compat.py` holds the .NET behaviours the output hashes depend on. The
`.ps1` files beside them are harnesses so `build-installer.ps1`,
`verify-installer-notices.ps1` and `license-check.yml` call them exactly as before.

Their output contract is still byte-exact -- the generated HTML is staged into the
installer and hashed by `verify-installer-notices.ps1` -- so "it still runs" is not
evidence a change was safe: regenerate and compare digests. **Two things changed
deliberately in the port**, both because the PowerShell was wrong rather than merely
different, and neither alters a notice, a hash or a count:

- **Ordering is ordinal, not `Sort-Object`.** That cmdlet compares with the *current
  culture*: it treats `-` as ignorable (so `autocfg` sorted before `auto-launch`), and
  under `da-DK` it sorts `aa` after `z`. The same lockfile therefore produced different
  bytes for a Danish developer than an American one -- a poor property for an artifact CI
  compares by hash between the build tree and the extracted installer. See
  `dotnet_compat.ordinal_key`.
- **A `licenses/` subdirectory is labelled with its real on-disk name.** PowerShell echoed
  back the lowercase path it had constructed, so `const-field-offset`'s `LICENSES/` was
  reported as `licenses/` -- a path that resolves only on a case-insensitive filesystem.

The port was accepted on evidence, not on the suites passing: **1,656 notice blocks across
594 package articles in all five reports** were compared against the PowerShell output, and
every per-article multiset is equal once labels are normalized for those two changes.
`about.generated.toml` and the three x86 reports are byte-identical; cargo-about's own
output is byte-identical everywhere; and two independent runs produce identical bytes.
| `source-notices.json`, `source_notices.py` | Reviewed versions, source paths, line ranges and complete-notice hashes for additional embedded terms; generation rejects changed source or unreviewed versions. |
| `components.toml` | Checked-in inventory of every **non-Cargo** component (the toolchain-supplied Rust Standard Library, shipped native DLLs, compiled-in SVGs, NSIS stub, plus the not-shipped downloaded assets — OCR + voice models — for attribution): origin, version/revision, SHA-256, SPDX, notice files, modification status. |
| `verify_component_hashes.py` | Re-hashes the in-repo assets `components.toml` pins by SHA-256 (the five Material Symbols SVGs compiled into `kokoro-panel.exe`) and fails if any drifted — the in-repo half of provenance; `verify_installer_notices.py` covers the shipped side. Run by `license-check.yml`. |
| `license-texts.sha256`, `verify-license-texts.ps1` | Reviewed SHA-256s for the shipped checked-in notice/licence files, normalized across CRLF/LF, plus the fail-closed verifier used by CI, the installer build, and the extraction test. |
| `verify_installer_notices.py` | Extracts the built `-setup.exe`, derives exact component notice paths from `components.toml`, and fails if any is missing or empty. A CI step in `installer.yml` runs it after the build; run manually anytime (needs 7-Zip). |
| `build_corresponding_source.py` | Builds `corresponding-source-<version>.zip` (GPLv3 §6 plus NSIS/LZMA CPL source) for attaching to a release. |
| `staging\` | Build output — everything that goes into the installer. Regenerated by `build-installer.ps1`. |
| `dependency-licenses\` | Output of `generate_dependency_licenses.py` — provisioned, not tracked (like `native-deps\runtime\notices\`). |

## One language in packaging/

Everything here is Python now; the `.ps1` files beside
each script are harnesses, so `installer.yml`, `license-check.yml` and a local
`packaginguild-installer.ps1` all call them exactly as before. `python3.ps1` is the shared
interpreter resolver (it resolves `py`/`python`/`python3` by RUNNING them - a Windows App
Execution Alias is a 0-byte reparse point and a size check rejects a working install).

**Standard library only.** No third-party packages: the corresponding-source README
promises that any Python 3 will do, and a release path that needs `pip install` first is a
release path that breaks on a clean runner. That is why the `components.toml` and
`about.toml` readers are narrow fail-closed regexes rather than a TOML parser, and why the
TLS trust-store correction in `provision_util.os_root_context` uses `ssl.enum_certificates`
instead of `certifi` or `truststore`.

`target_platform.py` holds what genuinely differs by platform - OS and architecture
detection, the Rust target triple, the executable suffix, the release package format - and
`build_installer.py`'s `PROFILES` holds the rest: which runtime libraries ship, which extra
client artifacts exist, which packager runs. Only the Windows rows are populated, and an
unknown platform raises rather than falling through to them. Adding the Linux release is
adding rows, not a second script.

**The port was accepted on evidence, not on the scripts running.** A full PowerShell
installer build and a Python `--skip-build` over the same tree produced **byte-identical
staging trees: 408 files, no additions, no removals, no differing content.** The notice
verifier was run against a real built installer under both implementations and returned the
same verdict. That was only possible because provenance records are now compared as a SET of
lines rather than as joined text - see `build_installer.records_match`.

It also found a live bug. `build-corresponding-source.ps1` re-hashed the espeak-ng tree with
`Sort-Object` and compared it byte for byte against the manifest `native-deps/fetch-deps.py`
writes in ordinal byte order. On the real 2579-file tree those orderings diverge at line 8,
so once provisioning became Python that check was guaranteed to throw - on a tagged release
only, in the GPLv3 section 6 path. The Python packager sorts the way the provisioner does,
and the two manifests now agree by construction.

## What gets staged

- `kokoro-host.exe` + `kokoro-panel.exe` (x64, release)
- The 5 native runtime DLLs + `espeak-ng-data` (from `native-deps/`)
- `icons/icon.ico`
- Under `resources\`: the three x86 artifacts — `KokoroSapi.dll`, `kokoro_hook.dll`,
  `kokoro-inject.exe` — plus `voice-setup.ps1` and `kindle-voice-guard.ps1`
- `legal.html` — the local **About & licenses** page opened by the tray and Settings,
  with copyright, warranty and GPL rights, local notice links and source-download access.
- `LICENSE`, `THIRD_PARTY_NOTICES.md`, and `licenses\` — the checked-in texts (GPLv3,
  Apache-2.0, Unicode-3.0, BSD-2-Clause, BSD-3-Clause, NCSA), plus five provisioned/
  generated subdirectories staged in at build time: `licenses\rust\` (the active toolchain's
  generated Standard Library copyright report + exact release/commit),
  `licenses\onnxruntime\` (that wheel's
  own notices), `licenses\espeak-ng\` (espeak-ng's `COPYING*`, incl. `COPYING.UCD` for the
  UCD data in `espeak-ng-data\`), `licenses\nsis\` (NSIS's own `COPYING` for the LZMA stub),
  and `licenses\dependencies\` (the Cargo closure's, one HTML file per shipped binary — see
  below)

The TTS model is **not** bundled; the panel downloads it on first run (~340 MB).

### The license files are an obligation, not a courtesy

The bundle links **espeak-ng** (GPL-3.0-or-later — and *modified* by
`native-deps/build-espeak.py`, which reverts the horse-hoarse merger) and **Slint** under
its GPL-3.0-only option. The repository source stays permissive (MIT, plus Apache-2.0 for the
files ported from `kokoro-js` and PaddleOCR and the Google Material Symbols SVGs), but **the
installed combination is conveyed
under GPLv3**, so the license text and notices have to ship *with* the binaries. Don't drop
them from the staging list or the `File` directives. `licenses/` is staged and installed
**recursively**, so a new licence text there ships with no edit here — which is what keeps
`Apache-2.0.txt` alongside `GPL-3.0.txt` without a second list to forget.

ONNX Runtime's own licence and notice set is the first exception: it is **provisioned, not
tracked**. `fetch-deps.ps1` keeps it from the wheel into `native-deps\runtime\notices\` and
this script stages it into `licenses\onnxruntime\`, so it stays matched to the exact build
the DLLs came from instead of being a hand copy that goes stale at the next version bump.
The runtime cache marker records the exact CPython 3.12 Windows wheel filename and its
reviewed PyPI SHA-256; changing either forces a fresh provision. Selecting by the local Python
version is not sufficient because 1.27.0's cp311-cp314 wheels contain different native DLL
bytes. Installer staging copies the DLLs from that marked cache rather than a possibly older
host build directory.
Both ends **throw** when it is absent rather than proceed: four of the installed binaries
come out of that wheel, and `dxcompiler.dll`'s NCSA licence requires its notice accompany
them. An installer missing licence text looks complete and is not — the same "looks
complete but isn't" failure every fail-loud check in this script guards against.

The fixed texts tracked in `licenses\`, plus `LICENSE`, `THIRD_PARTY_NOTICES.md` and
`legal.html`, have a
different drift risk: a stale, truncated, or wrong-revision copy still passes a presence
check. `license-texts.sha256` pins their reviewed content after newline normalization.
`verify-license-texts.ps1` also requires every tracked licence text to appear exactly once in
that manifest, and runs in pull requests and before the installer build.

The Cargo dependency closure is the second, and for the same reason: hundreds of transitive
crates across two target triples cannot be kept accurate by a checked-in list (one drifted
and shipped four sole-licensed crates as if MIT/Apache-2.0 already covered them — see
`THIRD_PARTY_NOTICES.md`'s "Cargo crates" section). `generate-dependency-licenses.ps1` runs
[`cargo about`](https://github.com/EmbarkStudios/cargo-about) against each shipped crate's
own `Cargo.lock` and target triple and this script stages the result into
`licenses\dependencies\`. It **fails the build** if a dependency's licence isn't on
`about.toml`'s accepted list, rather than shipping it uncovered. Each report also appends
the exact packaged `LICENSE*`/`NOTICE*`/`COPYRIGHT*` files and their SHA-256s: cargo-about's
normalized SPDX fallback can contain a copyright placeholder, which is not a substitute
for the real notice in a crate's package. Leading source copyright comments are retained
as labelled excerpts too. For packages that omit their upstream notices, `about.toml`
pins the exact texts and fetches them at the package's own source commit. The one local
RustAudio copy uses an `@project/` path expanded in a generated config; only the HTML
reports, not that machine-specific config, are staged. `verify-dependency-licenses.ps1`
then checks every appendix text hash and its expected block count, plus the separately
pinned clarification hashes: cargo-about 0.9.1 itself only warns on failed
clarifications and can still return success with canonical fallback text.
The generator also reads `source-notices.json` to retain complete W3C terms embedded in
Tao/Winit/cursor-icon and Intel's ISC notice in Ring's native P-384 source. These excerpts
include their attribution and short notices, not just a canonical licence template.
A version or text mismatch fails generation; review the full notice in the new published
package before updating the pinned version, line range or hash. The rendered-text verifier
requires these hashes for each affected crate/version in both generated and extracted reports.
Installer extraction also verifies every local link from `legal.html`.

espeak-ng's and NSIS's own notices are the third and fourth exceptions, provisioned the
same way. We ship a **modified** espeak-ng.dll + `espeak-ng-data\`, so `fetch-deps.ps1`
provisions espeak-ng's `COPYING`/`COPYING.APACHE`/`COPYING.BSD2`/`COPYING.UCD` from the exact
1.52.0 commit into `native-deps\espeak-ng-notices\` and this script stages them to
`licenses\espeak-ng\`. `COPYING.UCD` is **not** `licenses\Unicode-3.0.txt` — it covers the
Unicode Character Database data in `espeak-ng-data\`, a different document from the
`unicode-ident` crate's licence. The installer/uninstaller stub is NSIS compressed with LZMA
(`SetCompressor /SOLID lzma`), so this script stages NSIS's own `COPYING` (zlib + bzip2 +
CPL-1.0-with-linking-exception) from the installed toolchain to `licenses\nsis\`; NSIS is
**pinned** in `installer.yml` so that text matches the version used. Both stagers **throw**
when the text is absent. The espeak cache marker includes `build-espeak.py`'s normalized
SHA-256, so any patch/build-recipe change forces a rebuild; its build-time source manifest
must match the tree placed in the corresponding-source archive. `build-installer.ps1` also
rejects an installed `makensis` other than 3.12;
otherwise a local build could ship a different stub while the inventory still claimed 3.12.

The Rust Standard Library is a fifth provisioned notice set. It is statically linked into
every Rust output but is supplied by the rustc sysroot rather than any `Cargo.lock`, so
`cargo-about` cannot see it. `build-installer.ps1` copies the toolchain-generated
`COPYRIGHT-library.html` into `licenses\rust\` and records `rustc`'s release plus immutable
commit in `TOOLCHAIN.txt`. `build-corresponding-source.ps1` also requires the matching
`rust-src` component, checks the active toolchain against the installer's staged
`TOOLCHAIN.txt`, and places its full `library/` tree in the release source archive.

After `makensis`, `verify-installer-notices.ps1` extracts the produced `-setup.exe` and
**fails the build** if any required notice is missing or empty — and if any fixed checked-in
text differs from its reviewed SHA-256 — proving the right notice tree is *in the installer*,
not just in `staging\`. Component notice paths come directly from
`components.toml`; an unparseable declaration, a directory path, or a component without
exactly one `notice` field also fails closed. Every workflow run then assembles
`corresponding-source-<version>.zip` (GPLv3 §6: LFS-resolved project source,
lockfiles/scripts, the modified espeak-ng tree, the exact Rust Standard Library source,
SHA-256 manifests, the hash-verified official NSIS 3.12 source for its CPL-covered LZMA
module, and a rebuild README). It verifies the tracked project tree against the
manifest recorded when the binaries were built; `-SkipBuild` also checks both executable
hashes, since a standalone build can overwrite them without updating the records. The
source packager uses the records frozen under `staging/provenance/`, including espeak's
source manifest, rather than a later provision or build's records. A tag requires a clean matching source tag; a
manual run marks the archive non-release. `installer.yml` pairs it with the installer in the
Actions artifact, and a tag also attaches both to the release. `components.toml` is the
checked-in inventory of every non-Cargo shipped component; [`../LICENSING.md`](../LICENSING.md)
is the authoritative per-artifact map.

To rebuild from the corresponding-source archive, follow its root `README.txt`: install
the recorded Rust release, then initialize a local Git index in its project directory
(`git init`, `git add --all`) before provisioning and building. The archive omits `.git`,
but the installer records tracked build inputs; the index supplies that inventory without
requiring a commit or remote.

Nor may any shipped artifact claim plain "MIT" in its version resource — that's
`VIAddVersionKey` here, **and** the `LegalCopyright` set in `kokoro-host/build.rs` and
`kokoro-panel/build.rs`, which is what Windows shows in each exe's Properties dialog. See
[`../THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md).

## Install mode and elevation

**Per-user (`currentUser`, unelevated)**, installing to
`$LOCALAPPDATA\kokoro-kindle-reader` — the same path the original app used. That keeps the
app out of `C:\Program Files` and the installer itself out of UAC.

But `DllRegisterServer` writes HKLM and the Kindle guard does `reg load`, both of which
need admin. So the install/uninstall sections call **`voice-setup.ps1`**, which relaunches
*itself* through UAC (`Start-Process -Verb RunAs`). **One UAC prompt per install.**

The installer also sets the HKCU Run value to `kokoro-host.exe --hidden` (login autostart).

The uninstaller reverts Kindle to Microsoft David **before** unregistering, drops the Run
value, removes the ACL-locked ProgramData dir, and **offers** (default: keep, `/SD IDNO`)
to delete the downloaded model — so a silent upgrade run doesn't force a multi-hundred-MB
re-download.

`voice-setup.ps1` propagates its elevated half's exit code, and **both** NSIS sections `Pop`
it and warn on failure (`/SD IDOK`, non-fatal — the app is still usable, only Kindle
narration isn't). That matters most on uninstall: the file deletions that follow remove
`resources\` and the ProgramData copy, so a silently-failed unregistration would strand the
SAPI token with nothing left to retry it with.

> **Caveat:** if UAC is satisfied with a *different* admin account, the guard's
> `$env:LOCALAPPDATA` points at that admin's profile and it won't find the installing
> user's Kindle hive (logs "hive not found", skips).

## Why registration doesn't point at the staged DLL (local EoP)

`regsvr32` runs a DLL's `DllRegisterServer` and the guard runs a `.ps1` — **both as
admin**. If those files sat in `%LOCALAPPDATA%` (writable by the possibly-lower-integrity
user), a same-user process could swap them and get code run as admin on the next
install/uninstall.

So `voice-setup.ps1 -Action register` first copies `KokoroSapi.dll` and
`kindle-voice-guard.ps1` into `%ProgramData%\Kokoro Kindle Reader\engine\` with an
`icacls`-locked ACL, and registers/runs **those** copies:

- SYSTEM + Administrators: Full
- Users: read/execute
- Owner: the Administrators **group** — so an admin user's medium-integrity process can't
  reopen the ACL via owner-`WRITE_DAC`

It **fails closed** if the lock can't be set. Unregister reverts the voice, unregisters,
then removes that directory — using **only** those locked copies. If they're missing (an
install that predates them), it deletes the CLSID + token keys directly rather than falling
back to `regsvr32 /u` on the `resources\` DLL: uninstall-time fallback to a user-writable
artifact reopens the very same EoP, and by then `resources\` has been writable for the whole
life of the install. The Kindle-hive half of that path is inlined for the same reason
(`Clear-KindleDefaultToken`) — it *deletes* `DefaultTokenId` instead of setting David, since
with Kokoro gone Kindle's own default is the right end state, and it only touches the value
when it still points at `KokoroTTS`.

A failed register rolls back (drops any keys `DllRegisterServer` half-wrote, removes the
locked dir) before it throws, so a declined UAC or a broken DLL can't leave a CLSID pointing
at a file the installer is about to delete.

Two rules follow, and they are load-bearing:

1. **Never point the installer's registration back at a user-writable path** — on
   *either* action. Unregister is elevated too.
2. Do the privileged file placement from the **elevated** context (inside
   `voice-setup.ps1`), not from NSIS — NSIS runs unelevated here.

**Residual, only closable by signing the installer:** the `resources\` *source* files and
the entry-point `.ps1` are still user-writable, so first-install / entry-point tampering
needs a signed installer to fully close.

## Editing `installer.nsi`

**Keep it ASCII.** `makensis` parses the script as **ACP** (see its `(ACP)` log line)
because the file has no BOM; `Unicode true` only affects the *output* installer's strings.
A UTF-8 `…` or `—` in a user-visible `DetailPrint`/`MessageBox` renders as mojibake
(`â€¦`) in the install UI. Use `...` and `-`.

The product `VERSION` here must stay in lockstep with the `FileVersion`/`ProductVersion` in
`kokoro-host/build.rs` + `kokoro-panel/build.rs` and the 8 `Cargo.toml`s — the
`/bump-version` command does all of them.

See the repo-root [`CLAUDE.md`](../CLAUDE.md) for the cross-cutting invariants and
[`ARCHITECTURE.md`](../ARCHITECTURE.md) for how the pieces fit together.
