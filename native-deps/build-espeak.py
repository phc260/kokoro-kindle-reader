#!/usr/bin/env python3
"""Build libespeak-ng as a shared library + compile espeak-ng-data, pinned to the EXACT
phoneme behavior kokoro-js's `phonemizer` npm package uses, so native phonemization matches
what the Kokoro model was trained on.

ONE recipe for both platforms. This file replaced a PowerShell script and a bash script that
had to be kept pin-for-pin identical by hand -- an invariant that existed only because the
recipe was duplicated, whose failure mode was the worst kind: a phoneme difference does not
raise an error, it makes the voice say something slightly different, on one OS only.
`build-espeak.ps1` and `build-espeak.sh` are now thin harnesses that call this.

Two things make parity exact:

1. Pin espeak-ng to tag 1.52.0. Master (post-1.52.0) adds a stray palatalization after high
   front vowels that phonemizer's bundled espeak lacks; 1.52.0 does not.
2. Revert the "horse-hoarse merger" (commit 5b01dd86, phsource/ph_english_us phoneme `o@`):
   phonemizer bundles a PRE-merger espeak, so "for/four/-ore" words must emit the long
   close-mid back rounded `o`, not the post-merger open-mid one. Only the `ipa` lines are
   touched -- the FMT/formant is irrelevant since we consume espeak's IPA text, not its audio.

The Kokoro model was trained on phonemizer's output, so matching it (pre-merger) is correct
for THIS model even though the merger is more modern General American.

A DISTRIBUTION'S OWN libespeak-ng IS NOT A SUBSTITUTE. It is unmodified, and probably not
1.52.0 either; either difference changes the phonemes and so the voice. That is why this
builds from source rather than depending on a package.

espeak-ng-src is a gitignored clone next to this script.
"""

import hashlib
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SRC = HERE / "espeak-ng-src"
WINDOWS = os.name == "nt"

# The immutable commit behind 1.52.0, not only the mutable tag name.
EXPECTED_COMMIT = "4870adfa25b1a32b4361592f1be8a40337c58d6c"

# phonemizer's pre-merger espeak distinguishes O@ (horse/for/north -> open-mid vowel) from
# o@ (hoarse/four/shore/more/-ore -> close-mid vowel). 1.52.0 merged the two; restore o@ and
# leave O@ unchanged. Verified against the kokoro-js/phonemizer oracle. Written as code-point
# escapes so this file stays ASCII and no editor, locale or transfer can rewrite the one
# thing the digest below is checking.
MERGED = "\u0254\u02d0"    # open-mid back rounded + length mark (post-merger)
REVERTED = "o\u02d0"       # close-mid back rounded + length mark (pre-merger)

# The patched file, newline-normalized. Proves this is the ONE documented modification and
# not merely a tree where that one block also happens to look right.
EXPECTED_PATCHED_SHA256 = "ffa5cbde9ec07c8c76ac9e37e505861cbf1c7582e1c72ec90e1d21f9fa0bea23"

# Build directories are per-platform so one clone can serve both without either clobbering
# the other's objects. Both are tolerated as untracked by the clean-tree check below.
BUILD_DIR_NAME = "build-x64" if WINDOWS else "build-linux"
ALLOWED_UNTRACKED = {"build-x64", "build-linux", "build"}


def fail(msg):
    print(msg, file=sys.stderr)
    raise SystemExit(1)


def git(*args, check=True):
    """Run git in the espeak clone and return its stdout, stripped."""
    p = subprocess.run(
        ["git", "-C", str(SRC), *args],
        capture_output=True, text=True,
    )
    if check and p.returncode != 0:
        fail("git %s failed in %s:\n%s" % (" ".join(args), SRC, p.stderr.strip()))
    return p.stdout.strip()


def sha256_text(path):
    """SHA-256 of a file's bytes with newlines normalized, so a checkout policy cannot
    change the reviewed digest."""
    raw = path.read_bytes().replace(b"\r\n", b"\n").replace(b"\r", b"\n")
    return hashlib.sha256(raw).hexdigest()


def pin_source():
    """Check the clone out at the immutable commit."""
    if not (SRC / ".git").is_dir():
        fail("espeak-ng source not at %s - clone it first:\n"
             "  git clone https://github.com/espeak-ng/espeak-ng.git \"%s\"" % (SRC, SRC))

    current = git("rev-parse", "HEAD")
    if current != EXPECTED_COMMIT:
        print("checking out espeak-ng 1.52.0 commit %s (was: %s)" % (EXPECTED_COMMIT, current))
        git("stash", "--quiet", check=False)
        git("checkout", EXPECTED_COMMIT, "--quiet")
        current = git("rev-parse", "HEAD")
    if current != EXPECTED_COMMIT:
        fail("espeak-ng source is %s, expected immutable 1.52.0 commit %s"
             % (current, EXPECTED_COMMIT))


def revert_horse_hoarse():
    """Apply the one documented modification, idempotently, and prove it is the only one."""
    ph = SRC / "phsource" / "ph_english_us"
    text = ph.read_text(encoding="utf-8")
    match = re.search(r"phoneme o@.*?endphoneme", text, re.S)
    if not match:
        fail("espeak-ng %s has no phoneme o@ block at %s" % (EXPECTED_COMMIT, ph))

    if MERGED in match.group(0):
        block = match.group(0).replace(MERGED, REVERTED)
        text = text[:match.start()] + block + text[match.end():]
        # newline="" so the file's own line endings survive the round trip; the digest below
        # normalizes anyway, but rewriting them would show up as noise in `git diff`.
        with open(ph, "w", encoding="utf-8", newline="") as f:
            f.write(text)
        print("reverted horse-hoarse merger in phoneme o@")
    elif REVERTED in match.group(0):
        print("horse-hoarse revert already applied in phoneme o@")
    else:
        fail("phoneme o@ contains neither the expected merged nor reverted IPA sequence")

    verify = re.search(r"phoneme o@.*?endphoneme",
                       ph.read_text(encoding="utf-8"), re.S)
    if not verify or MERGED in verify.group(0) or REVERTED not in verify.group(0):
        fail("horse-hoarse revert verification failed; refusing to build an untracked variant")

    actual = sha256_text(ph)
    if actual != EXPECTED_PATCHED_SHA256:
        fail("Patched ph_english_us hash is %s, expected %s"
             % (actual, EXPECTED_PATCHED_SHA256))


def assert_tree_is_only_the_documented_change():
    """The clone must carry the one documented modification and nothing else. A build from a
    tree with extra edits would produce a binary whose corresponding source we cannot name."""
    tracked = [l for l in git("diff", "--name-only").splitlines() if l]
    staged = [l for l in git("diff", "--cached", "--name-only").splitlines() if l]
    untracked = [
        l for l in git("ls-files", "--others", "--directory",
                       "--no-empty-directory").splitlines() if l
    ]
    unexpected = [u for u in untracked if u.split("/", 1)[0] not in ALLOWED_UNTRACKED]

    if tracked != ["phsource/ph_english_us"] or staged or unexpected:
        fail("espeak-ng has modifications beyond the documented ph_english_us revert: "
             "tracked=%s, staged=%s, untracked=%s" % (tracked, staged, unexpected))


def vcvarsall():
    """Locate MSVC's environment script via vswhere (Windows only)."""
    program_files_x86 = os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")
    vswhere = Path(program_files_x86) / "Microsoft Visual Studio" / "Installer" / "vswhere.exe"
    if not vswhere.is_file():
        fail("vswhere not found at %s - install Visual Studio with the MSVC toolchain." % vswhere)
    p = subprocess.run([str(vswhere), "-latest", "-products", "*",
                        "-property", "installationPath"],
                       capture_output=True, text=True)
    if p.returncode != 0 or not p.stdout.strip():
        fail("vswhere could not locate a Visual Studio installation.")
    bat = Path(p.stdout.strip()) / "VC" / "Auxiliary" / "Build" / "vcvarsall.bat"
    if not bat.is_file():
        fail("vcvarsall.bat not found at %s" % bat)
    return bat


def build():
    """Configure + build. No audio/async/mbrola deps - we only call espeak_Synth for the
    phoneme trace, and every one of those would be a runtime dependency to package."""
    build_dir = SRC / BUILD_DIR_NAME
    if (build_dir / "CMakeCache.txt").is_file():
        shutil.rmtree(build_dir)

    configure = [
        "cmake", "-S", str(SRC), "-B", str(build_dir),
        "-DCMAKE_BUILD_TYPE=Release", "-DBUILD_SHARED_LIBS=ON",
        "-DUSE_ASYNC=OFF", "-DUSE_MBROLA=OFF", "-DUSE_LIBSONIC=OFF",
        "-DUSE_LIBPCAUDIO=OFF", "-DESPEAK_BUILD_DOC=OFF",
    ]

    if WINDOWS:
        # vcvarsall x64 -> NMake, so cl targets x64. It must run in the SAME shell as cmake,
        # which is why this is one `cmd /c` line rather than two subprocess calls.
        configure += ["-G", "NMake Makefiles"]
        quoted = " ".join('"%s"' % a if " " in a else a for a in configure)
        line = '"%s" x64 && %s && cmake --build "%s"' % (vcvarsall(), quoted, build_dir)
        rc = subprocess.run(["cmd", "/D", "/c", line]).returncode
    else:
        rc = subprocess.run(configure).returncode
        if rc == 0:
            rc = subprocess.run(
                ["cmake", "--build", str(build_dir), "-j", str(os.cpu_count() or 1)]
            ).returncode
    if rc != 0:
        fail("espeak-ng build failed (%d)" % rc)
    return build_dir


def main():
    pin_source()
    revert_horse_hoarse()
    assert_tree_is_only_the_documented_change()
    build_dir = build()

    print("\n=== artifacts ===")
    for pattern in ("libespeak-ng.so*", "espeak-ng.dll", "libespeak-ng.dll"):
        for f in sorted(build_dir.rglob(pattern)):
            print("  %s (%d bytes)" % (f, f.stat().st_size))
    data = build_dir / "espeak-ng-data"
    if data.is_dir():
        print("data dir: %s (%d files)" % (data, sum(1 for _ in data.rglob("*") if _.is_file())))


if __name__ == "__main__":
    main()
