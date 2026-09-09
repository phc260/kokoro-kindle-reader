#!/usr/bin/env python3
r"""Reproducibly provision native-deps/ (the runtime libraries the host stages + the espeak
library it links) with no manual venv / hardcoded paths, so a fresh clone or CI runner can
build the synth.

ONE recipe for both platforms. This replaced a PowerShell script and a bash script that had
to be kept pin-for-pin identical by hand; `fetch-deps.ps1` is now a thin harness that calls
this, and Linux invokes it directly. Unifying them also settled a divergence they had already grown --
the bash side accepted the first LICENSE it found anywhere in the wheel, which is exactly the
lax check the PowerShell side's comments warned against. Both now use the strict one.

Populates, per platform:

  Windows                                   Linux
  -------                                   -----
  runtime/onnxruntime.dll (+3)              linux/runtime/libonnxruntime.so (+2)
  runtime/espeak-ng.dll                     linux/runtime/libespeak-ng.so*
  runtime/espeak-ng-data/                   linux/runtime/espeak-ng-data/
  runtime/notices/                          linux/runtime/notices/
  runtime/*-PROVISION.txt                   linux/runtime/*-PROVISION.txt
  espeak-ng-src/  (shared clone)            espeak-ng-src/  (shared clone)
  espeak-ng-notices/  (shared)              espeak-ng-notices/  (shared)

THE TWO WHEELS ARE DIFFERENT PACKAGES, DELIBERATELY. Windows takes onnxruntime-webgpu,
because the Dawn WebGPU EP is what it runs on. Linux takes plain onnxruntime (CPU): the
WebGPU wheel exists there too, but native WebGPU is Vulkan on Linux and none of it has been
validated -- not the library's own dependencies, not the `ort` binding's registration, not
the drivers. The CPU milestone must not be able to fail for a GPU reason.

The ONNX model is loaded through `ort`'s load-dynamic, so the runtime library is opened at
run time rather than linked -- no ORT headers or import library needed.

Requires: python3, git, cmake, a C toolchain (MSVC on Windows, gcc/clang on Linux).
Idempotent: pass --force to re-provision.
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
WINDOWS = os.name == "nt"
# The script's own directory is sys.path[0] when run directly; make that explicit so
# importing this module from elsewhere (a test, a wrapper) resolves the helper too.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from provision_util import (  # noqa: E402 - needs the path above
    download,
    fail,
    rmtree_force,
    sha256_file,
    sha256_text,
)

ORT_VERSION = "1.27.0"

# Pin ONE exact wheel per platform, not only the release number. PyPI publishes distinct
# cp311..cp314 wheels for 1.27.0 and their native library bytes differ even though their
# embedded source/version IDs agree, so selecting by the machine's Python would make the
# shipped payload vary while packaging/components.toml continued to describe one provision.
# cp312 on both platforms.
WHEELS = {
    "windows": {
        "distribution": "onnxruntime-webgpu",
        "name": "onnxruntime_webgpu-1.27.0-cp312-cp312-win_amd64.whl",
        "url": ("https://files.pythonhosted.org/packages/df/28/"
                "016260c51c877ba5b3eba823b43107e894659e73e0ecf250eb07e801e3c2/"
                "onnxruntime_webgpu-1.27.0-cp312-cp312-win_amd64.whl"),
        "sha256": "7ef99275b13e8cb9584bd0db7a6f00ebf76095601eeccf7d34749b89ee991c19",
        # Exactly the four inventoried DLLs, by name.
        "exact_libs": ["onnxruntime.dll", "onnxruntime_providers_shared.dll",
                       "dxcompiler.dll", "dxil.dll"],
        "glob_libs": [],
        "alias": None,
    },
    "linux": {
        "distribution": "onnxruntime",
        "name": ("onnxruntime-1.27.0-cp312-cp312-"
                 "manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl"),
        "url": ("https://files.pythonhosted.org/packages/26/81/"
                "24dd9b31b0fb912ee19ca53ac1c9764bfd79d58a2ccef564eb693be831a5/"
                "onnxruntime-1.27.0-cp312-cp312-"
                "manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl"),
        "sha256": "7c65a7438632d55dfbc8a02ee60bd6cf7dd9d1ba05a43d4b851452f32338e194",
        # `libonnxruntime.so*` does NOT match `libonnxruntime_providers_shared.so`
        # (`libonnxruntime_` vs `libonnxruntime.`), which is how that shim came to be missing
        # here once. ORT dlopens it by plain name, so it has to be staged.
        "exact_libs": ["libonnxruntime_providers_shared.so"],
        "glob_libs": ["libonnxruntime.so*"],
        # The wheel ships the library versioned only (libonnxruntime.so.1.27.0, SONAME
        # libonnxruntime.so.1) with no plain-name symlink, and `native_synth::init_ort` opens
        # exactly "libonnxruntime.so". This copy is what creates that name.
        "alias": ("libonnxruntime.so", "libonnxruntime.so.*"),
    },
}

ESPEAK_COMMIT = "4870adfa25b1a32b4361592f1be8a40337c58d6c"
ESPEAK_REPO = "https://github.com/espeak-ng/espeak-ng.git"
# We ship a MODIFIED libespeak-ng (GPL-3.0-or-later), and parts of the espeak-ng tree carry
# ADDITIONAL licences that must accompany the binaries: COPYING is the GPLv3 text,
# COPYING.APACHE / COPYING.BSD2 cover code shims, and COPYING.UCD covers the Unicode
# Character Database data baked into espeak-ng-data/. COPYING.UCD is NOT the same document as
# licenses/Unicode-3.0.txt (that is the Unicode v3 licence for the unicode-ident crate) --
# the two are distinct and both are required.
ESPEAK_NOTICES = ["COPYING", "COPYING.APACHE", "COPYING.BSD2", "COPYING.UCD"]

# ORT's own notices, anchored to the package root the shipped binaries came out of. Matched
# precisely rather than as LICENSE*, which would swallow a vendored LICENSE.third-party.
ORT_LICENSE_RE = re.compile(r"^LICENSE(\.(txt|md))?$")
ORT_TPN_RE = re.compile(r"^ThirdPartyNotices(\.txt)?$")
NOTICE_PATTERNS = ("LICENSE", "NOTICE", "ThirdPartyNotices", "Privacy")


def layout():
    """Where this platform's provision lives, and which build dir espeak uses."""
    if WINDOWS:
        return {
            "runtime": HERE / "runtime",
            "espeak_build": HERE / "espeak-ng-src" / "build-x64",
            # The DLL is redirected up to src/ on Windows by a RUNTIME_OUTPUT_DIRECTORY
            # inside `if (MINGW OR WIN32 OR MSVC)`; the import lib stays in src/libespeak-ng.
            "espeak_lib_dirs": ["src"],
            "espeak_lib_globs": ["espeak-ng.dll"],
        }
    return {
        "runtime": HERE / "linux" / "runtime",
        "espeak_build": HERE / "espeak-ng-src" / "build-linux",
        # No redirect applies off Windows (a shared library is a LIBRARY target), so the
        # SOVERSION chain lands beside the CMakeLists that declares the target. The second
        # candidate exists only so a future layout change fails loudly rather than silently.
        "espeak_lib_dirs": ["src/libespeak-ng", "src"],
        "espeak_lib_globs": ["libespeak-ng.so*"],
    }


def marker_matches(path, expected):
    """Compare a provision marker ignoring line endings and trailing newlines, so a marker
    written by the PowerShell script this replaced (CRLF) still reads as current and does not
    force a needless re-provision."""
    try:
        actual = Path(path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return False
    norm = lambda s: s.replace("\r\n", "\n").replace("\r", "\n").rstrip("\n")
    return norm(actual) == norm(expected)


def write_marker(path, text):
    """Write a marker LAST, once everything it vouches for exists. Any earlier failure must
    leave the cache unmarked, and an unmarked cache is re-provisioned rather than trusted."""
    with open(path, "w", encoding="ascii", newline="\n") as f:
        f.write(text + "\n")


def nonempty_file(p):
    return Path(p).is_file() and Path(p).stat().st_size > 0


def nonempty_dir(p):
    p = Path(p)
    return p.is_dir() and any(f.is_file() for f in p.rglob("*"))


# ----------------------------------------------------------------- ORT wheel

def provision_ort(paths, force):
    wheel = WHEELS["windows" if WINDOWS else "linux"]
    runtime = paths["runtime"]
    notices = runtime / "notices"
    runtime.mkdir(parents=True, exist_ok=True)

    marker = runtime / "ORT-PROVISION.txt"
    expected = "%s=%s\nwheel=%s\nwheel-sha256=%s" % (
        wheel["distribution"], ORT_VERSION, wheel["name"], wheel["sha256"])

    # Re-fetch when ANY expected piece is missing, not just the libraries. Each of the notice
    # anchors was added after the libraries, so a provision predating one has the libraries
    # and not it -- and gating that on --force is how an installer build ends up staging
    # licence text that was never fetched.
    have_libs = all(nonempty_file(runtime / n) for n in wheel["exact_libs"])
    if wheel["alias"]:
        have_libs = have_libs and nonempty_file(runtime / wheel["alias"][0])
    if not force and have_libs and marker_matches(marker, expected) \
            and nonempty_file(notices / "ORT-LICENSE.txt") \
            and nonempty_file(notices / "ORT-ThirdPartyNotices.txt"):
        return False

    print("==> Fetching %s %s wheel" % (wheel["distribution"], ORT_VERSION))
    if marker.exists():
        marker.unlink()

    with tempfile.TemporaryDirectory(prefix="ort-%s-" % ORT_VERSION) as tmp:
        tmp = Path(tmp)
        whl = tmp / wheel["name"]
        download(wheel["url"], whl)

        # Download, VERIFY, then extract -- in that order. A tampered or truncated archive is
        # discarded with the temp dir, never unpacked.
        actual = sha256_file(whl)
        if actual != wheel["sha256"]:
            fail("%s wheel SHA-256 is %s, expected %s.\n"
                 "Refusing an unverified or wrong-ABI wheel."
                 % (wheel["distribution"], actual, wheel["sha256"]))

        extracted = tmp / "x"
        zipfile.ZipFile(whl).extractall(extracted)
        capi = extracted / "onnxruntime" / "capi"
        if not capi.is_dir():
            fail("%s wheel has no onnxruntime/capi directory." % wheel["distribution"])

        _stage_ort_libs(wheel, capi, runtime)
        _stage_ort_notices(wheel, extracted, capi, notices)

    write_marker(marker, expected)
    return True


def _stage_ort_libs(wheel, capi, runtime):
    """Copy exactly the inventoried libraries. Remove first, so a wheel that drops or renames
    one cannot be masked by a stale file from an older version."""
    for pattern in wheel["glob_libs"] + wheel["exact_libs"]:
        for stale in runtime.glob(pattern):
            stale.unlink()
    if wheel["alias"]:
        stale = runtime / wheel["alias"][0]
        if stale.exists():
            stale.unlink()

    for name in wheel["exact_libs"]:
        src = capi / name
        if not nonempty_file(src):
            fail("%s %s wheel is missing non-empty %s at %s"
                 % (wheel["distribution"], ORT_VERSION, name, capi))
        shutil.copy2(src, runtime / name)

    for pattern in wheel["glob_libs"]:
        found = sorted(capi.glob(pattern))
        if not found:
            fail("%s %s wheel contains no %s under %s"
                 % (wheel["distribution"], ORT_VERSION, pattern, capi))
        for f in found:
            shutil.copy2(f, runtime / f.name)

    if wheel["alias"]:
        dest_name, source_glob = wheel["alias"]
        if not (runtime / dest_name).exists():
            versioned = sorted(runtime.glob(source_glob))
            if not versioned:
                fail("no %s to create %s from" % (source_glob, dest_name))
            # A copy, not a symlink: the staged tree is what gets packaged, and it has to
            # stand on its own wherever it is unpacked.
            shutil.copy2(versioned[0], runtime / dest_name)


def _stage_ort_notices(wheel, extracted, capi, notices):
    """Keep the wheel's OWN licence + notice files, next to the binaries they describe.

    We redistribute binaries out of this wheel, and dxcompiler.dll's licence (University of
    Illinois/NCSA) requires its notice accompany them -- as does everything ORT links
    statically, which is far more than this script could enumerate. ORT's ThirdPartyNotices
    is the authoritative record of all of it, and taking it from the wheel keeps it matched
    to the exact build being shipped; a copy transcribed into the repo by hand would silently
    stop being true at the next version bump.
    """
    # Start from empty, so a rename upstream cannot leave a stale notice behind describing a
    # version that is no longer the one being shipped.
    rmtree_force(notices)
    notices.mkdir(parents=True)

    # Searched RECURSIVELY rather than by a fixed path: the wheel's internal layout is
    # upstream's to change, and a path that quietly stopped matching would ship the binaries
    # with none of their notices. Nothing found is never "fine, carry on".
    found = [f for f in extracted.rglob("*")
             if f.is_file() and f.name.startswith(NOTICE_PATTERNS)]
    if not found:
        fail("No licence or notice file found in the %s wheel (looked for %s under %s). "
             "Shipping those binaries without them is exactly what this step exists to "
             "prevent - find where upstream moved them and widen the search."
             % (wheel["distribution"], "/".join(p + "*" for p in NOTICE_PATTERNS), extracted))

    # "Some notice file exists" is not enough: the two that redistribution actually turns on
    # are the wheel's OWN LICENSE (the ORT MIT text) and its ThirdPartyNotices (everything ORT
    # links statically -- Dawn/Tint, DXC, and more). Two ways a lax check goes wrong:
    # Privacy.md alone satisfying the search (ships neither), and a vendored dependency's
    # LICENSE.third-party (or a nested dep's own pair) standing in for ORT's own.
    #
    # So this is ORT-SPECIFIC: the shipped binaries came from `capi`, so ORT's OWN notices are
    # the pair at that package's ROOT -- the parent of capi. A vendored dependency lives in
    # some other subtree, so pinning the directory is what ties the notices to the exact
    # package the binaries came out of.
    pkg_root = capi.parent
    lic = next((f for f in found
                if ORT_LICENSE_RE.match(f.name) and f.parent == pkg_root), None)
    tpn = next((f for f in found
                if ORT_TPN_RE.match(f.name) and f.parent == pkg_root), None)
    if not (lic and tpn and lic.stat().st_size and tpn.stat().st_size):
        lic_dirs = [str(f.parent) for f in found if ORT_LICENSE_RE.match(f.name)]
        tpn_dirs = [str(f.parent) for f in found if ORT_TPN_RE.match(f.name)]
        fail("%s wheel: ORT's own non-empty LICENSE and ThirdPartyNotices were not both "
             "found at the package root %s (the parent of the binary dir %s). Found LICENSE "
             "in [%s]; ThirdPartyNotices in [%s]. The wheel's own notices must come from the "
             "package the shipped binaries came out of, not a vendored subtree; if upstream "
             "moved them, update this anchor."
             % (wheel["distribution"], pkg_root, capi,
                "; ".join(lic_dirs), "; ".join(tpn_dirs)))

    for f in found:
        # Preserve each file's path RELATIVE TO THE WHEEL ROOT, not just its basename. Two
        # distinct files sharing a basename and their immediate parent's name would still
        # collide under a basename-plus-one-level scheme, and the second would be silently
        # dropped. A full relative path cannot collide, because extraction already gave every
        # file a distinct path.
        dest = notices / f.relative_to(extracted)
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(f, dest)

    # Emit the VERIFIED pair under fixed, ORT-specific canonical names at the notices root.
    # This is the anchor the installer verifier checks: the originals keep ORT's own names at
    # the wheel's own nested path, so requiring THOSE by path would mean hardcoding an
    # upstream-controlled layout. A file named ORT-LICENSE.txt sitting directly here is one
    # nothing else in the tree produces, so requiring it by exact path is a check no namesake
    # can pass. Byte copies, so the canonical anchor IS ORT's text, not a stand-in.
    shutil.copy2(lic, notices / "ORT-LICENSE.txt")
    shutil.copy2(tpn, notices / "ORT-ThirdPartyNotices.txt")


# ----------------------------------------------------------------- espeak-ng

def ensure_espeak_clone(src):
    """The build needs the source clone to exist (it is gitignored, so a fresh checkout or CI
    runner will not have it). Fetch the immutable commit DIRECTLY: a shallow clone of a moved
    tag would not contain the commit the build requires."""
    if not (src / ".git").is_dir():
        subprocess.run(["git", "init", "--quiet", str(src)], check=True)

    # Also recovers an interrupted first fetch, which leaves .git but no HEAD.
    have = subprocess.run(["git", "-C", str(src), "cat-file", "-e",
                           ESPEAK_COMMIT + "^{commit}"],
                          capture_output=True).returncode == 0
    if not have:
        print("==> Fetching espeak-ng commit %s" % ESPEAK_COMMIT)
        subprocess.run(["git", "-C", str(src), "fetch", "--depth", "1",
                        ESPEAK_REPO, ESPEAK_COMMIT], check=True)
        subprocess.run(["git", "-C", str(src), "checkout", "--quiet", "--detach",
                        ESPEAK_COMMIT], check=True)


def source_manifest(src, out):
    """The exact source that produced these binaries, hashed file by file -- what a release's
    corresponding source is checked against, so it must describe the tree that was actually
    built and exclude only the build outputs.

    Follows symlinks. `find -type f` does not match them, which is how
    src/include/espeak/speak_lib.h -- a tracked source file -- came to be silently absent
    from the Linux manifest while the Windows one included it. A corresponding-source
    manifest that quietly omits a file is the one failure it exists to prevent.
    """
    # Top level only, matching what was excluded before: a build directory at the root, not
    # every directory anywhere called build.
    skip = {".git", "build", "build-x64", "build-linux"}
    rows = []
    for dirpath, dirnames, filenames in os.walk(src):
        if Path(dirpath).resolve() == Path(src).resolve():
            dirnames[:] = [d for d in dirnames if d not in skip]
        for fn in filenames:
            p = Path(dirpath) / fn
            rel = p.relative_to(src).as_posix()
            rows.append((rel.encode("utf-8"), sha256_file(p)))
    rows.sort(key=lambda r: r[0])  # byte order, as LC_ALL=C sort gave
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        for rel, digest in rows:
            f.write("%s  %s\n" % (digest, rel.decode("utf-8")))
    return len(rows)


def provision_espeak(paths, force):
    src = HERE / "espeak-ng-src"
    runtime = paths["runtime"]
    build = paths["espeak_build"]
    ensure_espeak_clone(src)

    marker = runtime / "ESPEAK-PROVISION.txt"
    manifest = runtime / "espeak-ng-source.SHA256SUMS.txt"
    # The recipe's identity, not just its version: a build-flag or patch edit invalidates the
    # cache even though the pin did not move.
    expected = ("espeak-ng=1.52.0+horse-hoarse-revert;base=%s\nbuild-script-sha256=%s"
                % (ESPEAK_COMMIT, sha256_text(HERE / "build-espeak.py")))

    staged_lib = runtime / ("espeak-ng.dll" if WINDOWS else "libespeak-ng.so")
    if not force and nonempty_file(staged_lib) \
            and nonempty_dir(runtime / "espeak-ng-data") \
            and marker_matches(marker, expected) and nonempty_file(manifest):
        return False

    for stale in (marker, manifest):
        if stale.exists():
            stale.unlink()
    rmtree_force(runtime / "espeak-ng-data")

    print("==> Building espeak-ng (1.52.0 + horse-hoarse revert)")
    rc = subprocess.run([sys.executable, str(HERE / "build-espeak.py")]).returncode
    if rc != 0:
        fail("build-espeak.py failed (%d)" % rc)

    lib_dir = None
    for cand in paths["espeak_lib_dirs"]:
        d = build / cand
        if any(d.glob(g) for g in paths["espeak_lib_globs"]):
            lib_dir = d
            break
    if lib_dir is None:
        fail("espeak-ng build produced no %s under %s"
             % (" / ".join(paths["espeak_lib_globs"]), build))

    for pattern in paths["espeak_lib_globs"]:
        for f in sorted(lib_dir.glob(pattern)):
            # follow_symlinks=False keeps the SOVERSION chain a chain rather than three
            # copies of the same 600 KB; the loader resolves espeak by its SONAME.
            dest = runtime / f.name
            if dest.exists() or dest.is_symlink():
                dest.unlink()
            shutil.copy2(f, dest, follow_symlinks=False)

    data = build / "espeak-ng-data"
    if not nonempty_dir(data):
        fail("espeak-ng build produced no data tree at %s" % data)
    shutil.copytree(data, runtime / "espeak-ng-data")

    n = source_manifest(src, manifest)
    print("    espeak source manifest: %d files" % n)

    provision_espeak_notices(src, force=True)
    write_marker(marker, expected)
    return True


def provision_espeak_notices(src, force):
    """espeak-ng's own licence texts, from the exact clone we build, so they stay matched to
    the shipped library the same way the ORT notices do."""
    out = HERE / "espeak-ng-notices"
    missing = [c for c in ESPEAK_NOTICES if not nonempty_file(out / c)]
    if force or missing:
        rmtree_force(out)
        out.mkdir(parents=True)
        # Named files, not a wildcard sweep: ship exactly these licence texts, nothing else
        # the clone happens to contain. Each must exist -- a modified GPL binary shipped
        # without its licence text is the failure this block prevents.
        for c in ESPEAK_NOTICES:
            s = src / c
            if not nonempty_file(s):
                fail("espeak-ng licence file %s not found in the 1.52.0 clone at %s. "
                     "Shipping the modified library without its notices is what this step "
                     "exists to prevent - re-run with --force." % (c, src))
            shutil.copy2(s, out / c)
    for c in ESPEAK_NOTICES:
        if not nonempty_file(out / c):
            fail("espeak-ng notice provision is incomplete after copying: %s" % c)


# ----------------------------------------------------------------- entry point

def main():
    ap = argparse.ArgumentParser(description="Provision native-deps/ for this platform.")
    ap.add_argument("--ort-version", default=ORT_VERSION)
    ap.add_argument("--force", action="store_true", help="re-provision even if cached")
    args = ap.parse_args()

    if args.ort_version != ORT_VERSION:
        fail("Unsupported ORT version %s. Add its exact wheel URL and SHA-256 to WHEELS "
             "and update packaging/components.toml before provisioning it."
             % args.ort_version)

    paths = layout()
    provision_ort(paths, args.force)
    provision_espeak(paths, args.force)
    provision_espeak_notices(HERE / "espeak-ng-src", force=False)

    runtime = paths["runtime"]
    notices = runtime / "notices"
    espk_notices = HERE / "espeak-ng-notices"
    print("==> native-deps provisioned (%s):" % ("windows" if WINDOWS else "linux"))
    is_lib = lambda f: f.is_file() and (f.suffix in (".dll", ".so") or ".so." in f.name)
    print("    runtime libraries : %d" % sum(1 for f in runtime.iterdir() if is_lib(f)))
    print("    ORT notices       : %d"
          % (sum(1 for f in notices.rglob("*") if f.is_file()) if notices.is_dir() else 0))
    print("    espeak notices    : %d"
          % (sum(1 for f in espk_notices.iterdir() if f.is_file())
             if espk_notices.is_dir() else 0))
    print("    espeak-ng-data    : %d files"
          % sum(1 for f in (runtime / "espeak-ng-data").rglob("*") if f.is_file()))


if __name__ == "__main__":
    main()
