#!/usr/bin/env python3
r"""Build corresponding-source-<version>.zip for a binary release (GPLv3 section 6).

The shipped binaries are conveyed under GPLv3 (the host and panel link modified espeak-ng
and Slint), so whoever conveys them must provide COMPLETE CORRESPONDING SOURCE. Attaching
only the installer does not satisfy that, and pointing at an upstream espeak-ng TAG is
fragile - it can be retagged or deleted through no fault of ours.

"Complete corresponding source" here is: the modified GPL component (espeak-ng) carried IN
FULL, the exact Rust standard-library source statically linked by the active toolchain, and
the official NSIS 3.12 source archive for its CPL-covered LZMA module, plus GPLv3 section
6(d) clear-directions pointers for the pieces whose upstream source is immutable and public
- the Cargo crates (crates.io forbids republishing a version) and the permissively-licensed
native runtime pulled in dynamically. The archive is therefore not fully self-contained by
design; the README spells out where each off-archive piece is obtained, so a section 6
recipient never depends on a mutable tag.

    python3 packaging/build_corresponding_source.py                    # version from installer.nsi
    python3 packaging/build_corresponding_source.py --version 0.4.0    # explicit

Run AFTER the installer build (needs its staged provenance and Rust toolchain record).

RELEASE INTEGRITY: the archive's project source is exactly `git ls-files` at HEAD - tracked
files only. So an untracked build input is silently dropped, and an uncommitted edit ships
source that does not match the released binary. Both defeat section 6. This therefore
REFUSES to build unless the working tree is clean AND HEAD carries the v<version> tag.
`--allow-uncommitted` is the local dry-run escape hatch; it stamps the README as a
NON-release build so such an archive cannot be mistaken for the real thing.

Port of build-corresponding-source.ps1.
"""

import argparse
import hashlib
import re
import shutil
import sys
import urllib.request
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))

import target_platform  # noqa: E402
from build_installer import (  # noqa: E402
    Fail, RECORD_EOL, capture, nonempty, normalized_text_sha256,
    project_source_manifest, read_record, records_match, sha256_file, write_record,
)

ESPEAK_DIR_NAME = "espeak-ng-modified-1.52.0"
ESPEAK_BASE_COMMIT = "4870adfa25b1a32b4361592f1be8a40337c58d6c"
# Excluded from the copied espeak tree. Matches native-deps/fetch-deps.py's own skip set, so
# the tree hashed here is the tree that manifest describes - `build-linux` included, even
# though a Windows build never creates one: the two lists disagreeing is how they drift.
ESPEAK_EXCLUDE = {".git", "build", "build-x64", "build-linux"}

NSIS_SOURCE_URL = "https://downloads.sourceforge.net/nsis/nsis-3.12-src.tar.bz2"
NSIS_SOURCE_SHA256 = "f3ed7a8e4aa2cf4e8cf47d3b563a02559e0cb4934db2662b2f9661b824e2b186"
NSIS_SOURCE_NAME = "nsis-3.12-src.tar.bz2"
USER_AGENT = {"User-Agent": "Kokoro-Kindle-Reader-source-packager/1.0"}


def tree_manifest(root):
    """`<sha256>  <relative/posix/path>` for every file under `root`, in BYTE order.

    Ordinal, and `/`-separated, because `native-deps/fetch-deps.py` writes the espeak
    manifest that way and this one is compared against it byte for byte.

    **That comparison was broken and this is the fix.** The PowerShell original sorted with
    `Sort-Object`, which is culture-aware: it treats `-` as ignorable, so it ordered
    `.github/workflows/windows.yml` before `windows-msbuild.yml` while the provisioner's
    byte order has them the other way round. The two manifests diverged at line 8 of 2579,
    so once provisioning moved to Python this check threw on every tagged release - in the
    GPLv3 section 6 path, which only runs on a tag.
    """
    rows = []
    for p in Path(root).rglob("*"):
        if p.is_file():
            rel = p.relative_to(root).as_posix()
            rows.append((rel.encode("utf-8"), sha256_file(p)))
    rows.sort(key=lambda r: r[0])
    return ["%s  %s" % (digest, rel.decode("utf-8")) for rel, digest in rows]


def write_manifest(path, lines):
    """LF and a trailing newline, matching fetch-deps.py's writer exactly."""
    path.write_bytes(("\n".join(lines) + "\n").encode("utf-8"))


def copy_children(src, dest, exclude=()):
    dest.mkdir(parents=True, exist_ok=True)
    for item in sorted(Path(src).iterdir()):
        if item.name in exclude:
            continue
        target = dest / item.name
        if item.is_dir():
            shutil.copytree(item, target, dirs_exist_ok=True, symlinks=False)
        else:
            shutil.copy2(item, target)


def resolve_version(explicit):
    if explicit:
        return explicit
    # Derive from installer.nsi's !define VERSION, so this matches what the installer ships.
    from dotnet_compat import read_all_text
    m = re.search(r'!define\s+VERSION\s+"([^"]+)"', read_all_text(HERE / "installer.nsi"))
    if not m:
        raise Fail("Could not read VERSION from installer.nsi; pass --version explicitly.")
    return m.group(1)


def check_release_integrity(version, allow_uncommitted):
    commit = capture(["git", "-C", str(ROOT), "rev-parse", "HEAD"],
                     "git rev-parse HEAD failed; not a git checkout?").strip()
    if not commit:
        raise Fail("git rev-parse HEAD failed; not a git checkout?")
    porcelain = [ln for ln in capture(["git", "-C", str(ROOT), "status", "--porcelain"])
                 .replace("\r\n", "\n").split("\n") if ln]
    tags = [ln for ln in capture(["git", "-C", str(ROOT), "tag", "--points-at", "HEAD"])
            .replace("\r\n", "\n").split("\n") if ln]

    expected_tag = "v%s" % version
    is_clean, is_tagged = not porcelain, expected_tag in tags
    release_clean = is_clean and is_tagged
    if not release_clean:
        reasons = []
        if not is_clean:
            reasons.append("working tree is not clean (%d modified/untracked path(s)) - an "
                           "untracked build input would be silently omitted from the archive"
                           % len(porcelain))
        if not is_tagged:
            reasons.append("HEAD does not carry tag %s (tags here: [%s]) - the archive would "
                           "not match the released binary" % (expected_tag, ", ".join(tags)))
        msg = "Refusing to build RELEASE corresponding source:\n  - " + "\n  - ".join(reasons)
        if not allow_uncommitted:
            raise Fail(msg + "\nCheck out the release tag on a clean tree, or pass "
                             "--allow-uncommitted for a local (non-release) dry run.")
        print("WARNING: " + msg + "\n--allow-uncommitted set: continuing as a NON-RELEASE dry "
              "run (README will say so).", file=sys.stderr)
    return commit, release_clean, expected_tag


def check_rust_toolchain():
    """The toolchain whose precompiled standard library is linked into every Rust output.

    cargo-about enumerates Cargo packages only; rust-src is the corresponding source for
    std/core/alloc/compiler-builtins, and the generated COPYRIGHT-library.html is their
    exhaustive notice. The active toolchain must equal the one the installer recorded, or a
    local toolchain switch would pair different standard-library source with the binary.
    """
    info = [ln for ln in capture(["rustc", "--version", "--verbose"],
                                 "rustc --version --verbose failed.")
            .replace("\r\n", "\n").split("\n") if ln]
    if not info:
        raise Fail("rustc --version --verbose failed.")
    text = "\n".join(info)
    m = re.search(r"^release:\s+(\S+)\s*$", text, re.M)
    if not m:
        raise Fail("Could not read the Rust release from rustc --version --verbose.")
    release = m.group(1)
    m = re.search(r"^commit-hash:\s+([0-9a-f]{40})\s*$", text, re.M)
    if not m:
        raise Fail("Could not read the immutable Rust commit from rustc --version --verbose.")
    commit = m.group(1)

    recorded = HERE / "staging" / "licenses" / "rust" / "TOOLCHAIN.txt"
    if not recorded.is_file():
        raise Fail("The installer build toolchain record is missing. Run the installer build "
                   "before creating its corresponding-source archive.")
    if not records_match(read_record(recorded), info):
        raise Fail("The active Rust toolchain differs from the one that built the installer. "
                   "Restore the recorded toolchain before creating corresponding source.")

    sysroot = Path(capture(["rustc", "--print", "sysroot"], "rustc --print sysroot failed.").strip())
    library = sysroot / "lib" / "rustlib" / "src" / "rust" / "library"
    notice = sysroot / "share" / "doc" / "rust" / "COPYRIGHT-library.html"
    if not (library / "std" / "Cargo.toml").is_file():
        raise Fail("Rust standard-library source not found at %s - install the exact "
                   "toolchain's rust-src component before building release corresponding "
                   "source." % library)
    if not notice.is_file():
        raise Fail("Rust standard-library copyright report not found at %s" % notice)
    return info, release, commit, library, notice


def check_installer_provenance():
    """The installer must have been built from this tree, and still be the one staged."""
    staging = HERE / "staging"
    provenance = staging / "provenance"
    project_record = provenance / "kkr-project-source.SHA256SUMS.txt"
    if not project_record.is_file():
        raise Fail("Installer project-source provenance is missing. Run the installer build "
                   "before creating corresponding source.")
    if not records_match(read_record(project_record), project_source_manifest()):
        raise Fail("The tracked project source changed after the installer binaries were "
                   "built. Rebuild the installer before creating corresponding source.")

    output_record = provenance / "kkr-build-outputs.SHA256SUMS.txt"
    if not output_record.is_file():
        raise Fail("Installer output provenance is missing; rebuild the installer before "
                   "creating source.")
    staged = ["%s  %s" % (sha256_file(staging / n), n)
              for n in (target_platform.exe_name("kokoro-host"),
                        target_platform.exe_name("kokoro-panel"))]
    if not records_match(read_record(output_record), staged):
        raise Fail("Staged executables differ from their build records; rebuild the installer.")
    return provenance


def main(argv=None):
    ap = argparse.ArgumentParser(description="Build the GPLv3 corresponding-source archive.")
    ap.add_argument("--version", help="release version (default: from installer.nsi)")
    ap.add_argument("--allow-uncommitted", action="store_true",
                    help="local dry run; stamps the README as NON-RELEASE")
    args = ap.parse_args(argv)

    version = resolve_version(args.version)
    print("==> Corresponding source for v%s" % version)
    commit, release_clean, expected_tag = check_release_integrity(version, args.allow_uncommitted)
    print("==> Source commit: %s%s"
          % (commit, " (clean, tagged %s)" % expected_tag if release_clean else " (DEV/dirty)"))

    rustc_info, rust_release, rust_commit, rust_library, rust_notice = check_rust_toolchain()
    print("==> Rust standard library: %s (%s)" % (rust_release, rust_commit))
    provenance = check_installer_provenance()

    stage = HERE / "corresponding-source-%s" % version
    out = HERE / "corresponding-source-%s.zip" % version
    shutil.rmtree(stage, ignore_errors=True)
    out.unlink(missing_ok=True)
    stage.mkdir(parents=True)

    # 1. The project's own tracked source, with Git-LFS assets RESOLVED. `git ls-files` is
    #    the exact set under version control; copying those paths from the working tree
    #    (which a CI checkout populates with real LFS content, not pointer stubs) is what
    #    guarantees the archive ships resolved bytes. This also naturally excludes target/
    #    and the gitignored native-deps provisioning - espeak source is added deliberately.
    print("==> Copying tracked project source (LFS resolved)")
    tracked = [ln for ln in capture(["git", "-C", str(ROOT), "ls-files"], "git ls-files failed")
               .replace("\r\n", "\n").split("\n") if ln]
    src_dir = stage / "kokoro-kindle-reader"
    for rel in tracked:
        frm = ROOT / rel
        if not frm.is_file():
            continue  # a deleted-but-staged path
        to = src_dir / rel
        to.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(frm, to)

    # Guard against shipping LFS pointer stubs: a resolved binary asset is not a ~130-byte
    # text file that starts with the LFS spec URL.
    icon = src_dir / "icons" / "icon.ico"
    if icon.is_file():
        head = icon.read_bytes()[:64]
        if len(icon.read_bytes()) < 1024 and b"git-lfs" in head:
            raise Fail("icons/icon.ico is a Git-LFS POINTER, not the resolved file. Check out "
                       "with lfs:true (run: git lfs pull) before building the source archive.")

    # 2. The MODIFIED espeak-ng tree actually built, minus .git and build output. This is the
    #    load-bearing part of section 6: the GPL component whose source would otherwise
    #    depend on an upstream tag staying put.
    espeak_src = ROOT / "native-deps" / "espeak-ng-src"
    if not (espeak_src / "phsource" / "ph_english_us").is_file():
        raise Fail("Modified espeak-ng source not found at %s - run native-deps/fetch-deps.py "
                   "first (it fetches the 1.52.0 commit and applies the horse-hoarse revert)."
                   % espeak_src)
    espeak_expected = ("espeak-ng=1.52.0+horse-hoarse-revert;base=%s\nbuild-script-sha256=%s"
                       % (ESPEAK_BASE_COMMIT,
                          normalized_text_sha256(ROOT / "native-deps" / "build-espeak.py")))
    marker = provenance / "ESPEAK-PROVISION.txt"
    built_manifest = provenance / "espeak-ng-source.SHA256SUMS.txt"
    from build_installer import read_marker
    if read_marker(marker) != espeak_expected or not nonempty(built_manifest):
        raise Fail("The installer has no matching espeak-ng provenance/source manifest. Run "
                   "native-deps/fetch-deps.py and rebuild the installer before creating source.")

    print("==> Copying modified espeak-ng source (excluding .git and build output)")
    espeak_dest = stage / ESPEAK_DIR_NAME
    copy_children(espeak_src, espeak_dest, ESPEAK_EXCLUDE)

    print("==> Hashing espeak-ng source tree")
    manifest_path = stage / ("%s.SHA256SUMS.txt" % ESPEAK_DIR_NAME)
    archive_manifest = tree_manifest(espeak_dest)
    write_manifest(manifest_path, archive_manifest)
    if not records_match(read_record(built_manifest), archive_manifest):
        raise Fail("The espeak-ng source tree differs from the one staged in the installer. "
                   "Restore that source, or re-provision and rebuild the installer before "
                   "creating source.")

    # 3. The exact Rust standard-library source linked into all five Rust outputs. rust-src
    #    is target-independent, so one copy covers both the x64 and x86 precompiled standard
    #    libraries from this toolchain. Include the generated copyright report beside it:
    #    unlike Cargo packages, these sources never enter cargo-about's graph.
    rust_path_version = re.sub(r"[^A-Za-z0-9._-]", "-", rust_release)
    rust_dest = stage / ("rust-standard-library-%s" % rust_path_version)
    print("==> Copying Rust standard-library source (%s)" % rust_release)
    copy_children(rust_library, rust_dest / "library")
    shutil.copy2(rust_notice, rust_dest / "COPYRIGHT-library.html")
    write_record(rust_dest / "TOOLCHAIN.txt", rustc_info)

    print("==> Hashing Rust standard-library source tree")
    # Sorted, unlike the original, which took whatever order the directory walk produced.
    # Nothing compares this manifest, so the order was free - and a shipped inventory that
    # reorders itself between builds is a worse artifact than one that does not.
    write_manifest(stage / ("rust-standard-library-%s.SHA256SUMS.txt" % rust_path_version),
                   tree_manifest(rust_dest))

    # 4. NSIS's LZMA compression module is CPL-1.0 with a linking exception. The exception
    #    keeps the installed application out of CPL, but the module itself remains CPL and
    #    its object-code terms require stating that source is available and how to get it.
    #    Carry the exact official archive rather than relying on a link that can move; hash
    #    it so a SourceForge error page or substituted download fails the release.
    print("==> Downloading NSIS 3.12 source")
    nsis_dest = stage / NSIS_SOURCE_NAME
    req = urllib.request.Request(NSIS_SOURCE_URL, headers=USER_AGENT)
    with urllib.request.urlopen(req, timeout=120) as r, open(nsis_dest, "wb") as f:
        shutil.copyfileobj(r, f)
    actual = sha256_file(nsis_dest)
    if actual != NSIS_SOURCE_SHA256:
        raise Fail("NSIS 3.12 source SHA-256 is %s, expected %s. Refusing to publish an "
                   "unverified or non-source download." % (actual, NSIS_SOURCE_SHA256))

    # 5. The README with rebuild directions and the lockfile-immutability note that stands in
    #    for `cargo vendor` (the crates.io versions pinned in the committed lockfiles ARE the
    #    corresponding source; crates.io is immutable).
    (stage / "README.txt").write_bytes(
        readme(version, commit, release_clean, expected_tag, rust_release, rust_commit,
               rust_path_version).encode("ascii"))

    # 6. Zip it. Written explicitly rather than via a helper so the entry order is sorted:
    #    a release artifact that shuffles its own table of contents between builds is
    #    needlessly hard to diff.
    print("==> Compressing")
    entries = sorted((p for p in stage.rglob("*") if p.is_file()),
                     key=lambda p: p.relative_to(stage).as_posix())
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        for p in entries:
            z.write(p, p.relative_to(stage).as_posix())
    shutil.rmtree(stage, ignore_errors=True)
    print("==> Corresponding source: %s  (%.1f MB)" % (out, out.stat().st_size / (1024 * 1024)))


def readme(version, commit, release_clean, expected_tag, rust_release, rust_commit,
           rust_path_version):
    provenance = (
        "Built from a clean checkout at tag %s, commit %s." % (expected_tag, commit)
        if release_clean else
        "*** NON-RELEASE DEV BUILD (built with --allow-uncommitted from an untagged or dirty "
        "tree). ***\n*** Source commit %s; this archive does NOT necessarily match any "
        "released binary. ***" % commit)
    text = README_TEMPLATE % {
        "version": version, "provenance": provenance, "rust_release": rust_release,
        "rust_commit": rust_commit, "rust_path_version": rust_path_version,
        "nsis_sha256": NSIS_SOURCE_SHA256,
    }
    return text.replace("\n", RECORD_EOL)


README_TEMPLATE = """\
Corresponding source for Kokoro Kindle Reader v%(version)s
=======================================================

%(provenance)s

This archive is the complete corresponding source (GPLv3 section 6) for the GPL-covered
binaries in the matching release: kokoro-host.exe, kokoro-panel.exe, and the modified
espeak-ng.dll + espeak-ng-data/.

Contents
--------
- kokoro-kindle-reader/            The project source at tag v%(version)s (Git-LFS resolved),
                                   including all Cargo.lock files and the native-deps/ and
                                   packaging/ build and provisioning scripts.
- espeak-ng-modified-1.52.0/       The exact modified espeak-ng source that was built: the
                                   1.52.0 tree with the horse-hoarse revert in
                                   phsource/ph_english_us (see build-espeak.py). Its .git
                                   and build output are excluded.
- espeak-ng-modified-1.52.0.SHA256SUMS.txt   SHA-256 of every file in that tree.
- rust-standard-library-%(rust_path_version)s/   The exact rust-src library/ tree for rustc
                                   %(rust_release)s, commit %(rust_commit)s, plus that toolchain's
                                   generated COPYRIGHT-library.html and TOOLCHAIN.txt.
- rust-standard-library-%(rust_path_version)s.SHA256SUMS.txt   SHA-256 of every file in that tree.
- nsis-3.12-src.tar.bz2           The exact official NSIS 3.12 source archive, including the
                                   CPL-1.0 LZMA compression module used by the installer stub.

Cargo dependencies
------------------
The Cargo crates statically linked into the executables are the immutable crates.io versions
pinned in the committed Cargo.lock files (kokoro-host/Cargo.lock and kokoro-panel/Cargo.lock
for the GPL-covered exes). crates.io does not permit republishing a version, so those pins
ARE the corresponding source; ``cargo build`` against the included lockfiles fetches exactly
them. Slint's version is recorded in kokoro-panel/Cargo.lock (used unmodified, under its
GPL-3.0-only option). To materialize them offline: ``cargo vendor`` from each crate dir.

Rust standard library
---------------------
Every Rust output also statically links the standard library supplied by rustc %(rust_release)s
(commit %(rust_commit)s). That code is outside Cargo's package graph, so it is included above in
full from this exact toolchain's rust-src component. COPYRIGHT-library.html is Rust's generated
licence and copyright inventory for that library source and its bundled dependencies.

NSIS
----
The installer/uninstaller stub is built with NSIS 3.12 (pinned in
.github/workflows/installer.yml). NSIS is not GPL and not linked into the executables; its
exact official source is the included nsis-3.12-src.tar.bz2 (SHA-256
%(nsis_sha256)s). Its licence (incl. the LZMA CPL-1.0 linking exception) ships as
licenses/nsis/NSIS-COPYING.txt in the installer.

Native runtime (ONNX Runtime / Dawn / DXC)
------------------------------------------
The synth loads onnxruntime.dll (+ onnxruntime_providers_shared.dll, dxcompiler.dll,
dxil.dll) dynamically at runtime; Dawn/Tint are statically linked inside onnxruntime.dll.
These are permissively licensed (ORT: MIT; Dawn/Tint: BSD-3-Clause; DXC: NCSA plus bundled
third-party terms - see licenses/ in the installer), not GPL, and are not modified by this
project. They are identified below by IMMUTABLE commit / version IDs - not a git tag, which
can be retargeted - so the exact source stays recoverable:
  - ONNX Runtime 1.27.0 = the cp312 win_amd64 wheel named in components.toml, SHA-256
    7ef99275b13e8cb9584bd0db7a6f00ebf76095601eeccf7d34749b89ee991c19; source is git
    commit 8f0278c77bf44b0cc83c098c6c722b92a36ac4b5 at
    https://github.com/microsoft/onnxruntime (the shipped DLL's build string
    1.27.20260615.2.8f0278c embeds that commit). ORT pins its OWN native deps by commit +
    archive hash in cmake/deps.txt at that commit; that file is the authoritative record of
    the exact revisions below.
  - Dawn (which contains Tint) = git commit ec7b457e5bb1fcec6f59733c4f3dd84d2f885a38 at
    https://github.com/google/dawn (archive SHA1 d4d64d1729104b61e654073566f1a376e16cad92,
    per ORT's cmake/deps.txt at the commit above).
  - DirectX Shader Compiler = the Microsoft.Direct3D.DXC prebuilt redistributable bundled in
    the ORT wheel, at https://github.com/microsoft/DirectXShaderCompiler (not built from ORT
    source). Each DLL's version resource embeds the short commit prefix; the full immutable
    commits are:
      - dxcompiler.dll v1.9.0.1     = git commit 3e6e148537683c22e3e74977d56516f16f39c7be
                                      (version resource: "1.9.0.1 (3e6e1485)")
      - dxil.dll       v1.8.2502.11 = git commit 2399215226737c64e76afc55ffd874ecd6fc459f
                                      (version resource: "1.8.2502.11 (239921522)")
The same immutable IDs are recorded in packaging/components.toml (included in the project
source above).

Rebuilding
----------
1. Install Git, CMake + MSVC, NSIS 3.12, Python 3, and rustup. (Python runs the native
   dependency provisioning in step 4 and the packaging scripts; any Python 3 will do, and it
   is not needed to run the installed application.) Install the recorded Rust toolchain:
   ``rustup toolchain install %(rust_release)s --component rust-src --target i686-pc-windows-msvc``.
2. Open a shell in the extracted kokoro-kindle-reader/ directory and run
   ``rustup override set %(rust_release)s``. Verify ``rustc --version --verbose`` reports
   commit %(rust_commit)s. Then install
   ``cargo install cargo-about --version 0.9.1 --locked --features cli``.
3. This source archive has resolved Git-LFS assets but no .git directory. Initialize the
   local file index needed by the packaging scripts: ``git init`` then ``git add --all``.
   No commit, user identity, or remote is required to build the installer.
4. Run ``native-deps\\fetch-deps.ps1`` and then ``packaging\\build-installer.ps1``. The
   provisioner fetches the exact upstream espeak commit and reapplies the documented patch;
   it also downloads the pinned ORT wheel. OCR models download at app runtime.
   The included espeak-ng-modified-1.52.0/ is the matching modified source for inspection
   and modification, not a Git checkout to copy over native-deps/espeak-ng-src/.
See ARCHITECTURE.md and packaging/README.md in the project source for detail.

The build is not bit-for-bit reproducible (espeak-ng is built with whatever MSVC the runner
has; see THIRD_PARTY_NOTICES.md), but the source and configuration here reproduce a
functionally identical build.
"""


if __name__ == "__main__":
    try:
        main()
    except Fail as e:
        raise SystemExit(str(e))
