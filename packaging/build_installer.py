#!/usr/bin/env python3
r"""Build the installer: compile the shipped binaries, stage everything the package needs,
then hand the staging tree to the platform's packager.

    python3 packaging/build_installer.py              # full: build + stage + package
    python3 packaging/build_installer.py --skip-build # reuse existing release binaries

Output: packaging/kokoro-kindle-reader-<version>-setup.exe

Port of build-installer.ps1.

**The logic here is platform-agnostic; the platform-specific facts are the `PROFILES`
table.** Which runtime libraries ship, what the extra client artifacts are, which packager
turns a staging tree into an installable file - those differ. The order of operations, the
provenance contract, the notice staging and every fail-closed check do not. Only the
Windows profile is populated: the Linux package is the port plan's packaging step, and a
profile that guessed at it would produce an artifact nobody had checked.
"""

import argparse
import hashlib
import re
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))

import target_platform  # noqa: E402

# --- platform facts ----------------------------------------------------------------------

WINDOWS = {
    # Provisioned native runtime: where fetch-deps puts it, and what must be in it.
    "runtime_dir": ROOT / "native-deps" / "runtime",
    "runtime_libs": ["onnxruntime.dll", "onnxruntime_providers_shared.dll",
                     "dxcompiler.dll", "dxil.dll", "espeak-ng.dll"],
    # Kindle is a 32-bit process and loads the COM shim in-process, so these are x86 and
    # exist only here. The hook + injector force the Kokoro voice in Kindle 18632.
    "client_crates": [
        ("kokoro-sapi", "i686-pc-windows-msvc", "KokoroSapi.dll"),
        ("kokoro-hook", "i686-pc-windows-msvc", "kokoro_hook.dll"),
        ("kokoro-inject", "i686-pc-windows-msvc", "kokoro-inject.exe"),
    ],
    # Elevation-requiring helpers that ship beside them; PowerShell by nature (regsvr32,
    # icacls, reg load) and staying that way - see CLAUDE.md.
    "resource_scripts": [ROOT / "kokoro-sapi" / "kindle-voice-guard.ps1",
                         ROOT / "kokoro-sapi" / "voice-setup.ps1"],
    "packager": "nsis",
}

PROFILES = {"windows": WINDOWS}

# The ONNX Runtime wheel this project is pinned to. The marker proves the cache was
# produced by that exact recipe; see native-deps/fetch-deps.py.
ORT_PROVISION_EXPECTED = (
    "onnxruntime-webgpu=1.27.0\n"
    "wheel=onnxruntime_webgpu-1.27.0-cp312-cp312-win_amd64.whl\n"
    "wheel-sha256=7ef99275b13e8cb9584bd0db7a6f00ebf76095601eeccf7d34749b89ee991c19")
ESPEAK_BASE_COMMIT = "4870adfa25b1a32b4361592f1be8a40337c58d6c"
ORT_NOTICES = ["ORT-LICENSE.txt", "ORT-ThirdPartyNotices.txt"]
ESPEAK_NOTICES = ["COPYING", "COPYING.APACHE", "COPYING.BSD2", "COPYING.UCD"]
NSIS_VERSION = "v3.12"
NSIS_EXE = Path(r"C:\Program Files (x86)\NSIS\makensis.exe")

# .NET's File.WriteAllLines uses Environment.NewLine and appends one after the last line.
# The provenance records are build-local, never shipped and never compared across machines,
# so matching the platform here costs nothing and keeps a Python-written record
# byte-identical to the PowerShell one it replaces.
RECORD_EOL = "\r\n" if sys.platform.startswith("win") else "\n"


def profile():
    os_name = target_platform.current_os()
    p = PROFILES.get(os_name)
    if p is None:
        raise Fail("No installer profile for %s. Building a package there is the port "
                   "plan's packaging step; add a row to PROFILES rather than letting this "
                   "fall through to the Windows one." % os_name)
    return p


class Fail(RuntimeError):
    """A build-stopping condition. Every one of these means: do not ship."""


def run(cmd, cwd=None, what=None):
    rc = subprocess.run([str(c) for c in cmd], cwd=str(cwd) if cwd else None).returncode
    if rc:
        raise Fail(what or ("%s failed" % cmd[0]))


def capture(cmd, what=None):
    proc = subprocess.run([str(c) for c in cmd], capture_output=True)
    if proc.returncode:
        sys.stderr.write(proc.stderr.decode("utf-8", "replace"))
        raise Fail(what or ("%s failed" % cmd[0]))
    return proc.stdout.decode("utf-8", "replace")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def normalized_text_sha256(path):
    """SHA-256 of a text file with newlines normalized, decoded STRICTLY.

    Strict is the point: this identifies a recipe (`build-espeak.py`) whose digest is
    recorded in a provision marker, so a file that is not valid UTF-8 is a problem to
    report, not one to paper over with replacement characters.
    """
    text = open(path, "rb").read().decode("utf-8")
    normalized = text.replace("\r\n", "\n").replace("\r", "\n")
    return hashlib.sha256(normalized.encode("utf-8")).hexdigest()


def nonempty(path):
    return path.is_file() and path.stat().st_size > 0


def read_marker(path):
    if not path.is_file():
        return ""
    return (open(path, "rb").read().decode("utf-8", "replace")
            .replace("\r\n", "\n").replace("\r", "\n").rstrip("\n"))


# --- provenance ----------------------------------------------------------------------------

def project_source_manifest():
    """`<sha256>  <path>` for every tracked file, so `--skip-build` can prove the binaries
    it is about to reuse came from this exact tree."""
    from dotnet_compat import ordinal_key
    out = capture(["git", "-C", str(ROOT), "ls-files"],
                  "git ls-files failed while recording installer source provenance.")
    tracked = [line for line in out.replace("\r\n", "\n").split("\n") if line]
    if not tracked:
        raise Fail("git ls-files failed while recording installer source provenance.")
    lines = []
    for rel in sorted(tracked, key=ordinal_key):
        path = ROOT / rel
        if not path.is_file():
            raise Fail("Tracked build input is missing: %s" % rel)
        lines.append("%s  %s" % (sha256_file(path), rel))
    return lines


def write_record(path, lines):
    # ASCII, as the PowerShell original wrote it: these records hold hex digests and
    # repo-relative paths, and a non-ASCII byte in one means something is wrong upstream.
    path.write_bytes((RECORD_EOL.join(lines) + RECORD_EOL).encode("ascii"))


def read_record(path):
    return [ln for ln in
            open(path, "rb").read().decode("ascii", "replace")
            .replace("\r\n", "\n").split("\n") if ln]


def records_match(built, current):
    """Compare provenance as a SET of lines, not as a joined string.

    The record means "these files had these hashes"; the order it happens to be written in
    is incidental. Comparing the joined text made the check sensitive to sort order, which
    is exactly the culture-dependent thing this port replaced with an ordinal sort - so a
    record written by the PowerShell build would have failed a Python `--skip-build` over
    nothing but collation. Set comparison is both more robust and what the check means.
    """
    return sorted(built) == sorted(current)


# --- preflight -------------------------------------------------------------------------------

def preflight(prof):
    """Everything that must hold before a single Rust output is compiled.

    Ordered cheapest-first on purpose: a stale inventory or a wrong NSIS should cost
    seconds, not a full release build.
    """
    # Fail fast on inventory drift: the in-repo assets pinned in components.toml (the five
    # Material Symbols SVGs compiled into kokoro-panel.exe) must still hash to their
    # recorded SHA-256. license-check.yml runs this on PRs, but a tag or manual build can
    # start from a commit that never went through one.
    print("==> Verifying non-Cargo component hashes (components.toml)")
    import verify_component_hashes
    try:
        verify_component_hashes.verify()
    except ValueError as e:
        raise Fail(str(e))

    # A file can be present and non-empty while still being truncated or copied from the
    # wrong upstream revision; verify the reviewed content before spending time on a build.
    print("==> Verifying checked-in licence texts")
    # A subprocess rather than an import: the file name has a hyphen, so it is not
    # importable, and sys.executable keeps it the same interpreter either way.
    run([sys.executable, HERE / "verify-license-texts.py"],
        what="checked-in licence-text verification FAILED (see above).")

    makensis = check_packager(prof)

    # Provisioned notices must be current too - checked before any cargo build so a stale
    # dependency cache costs seconds.
    runtime = prof["runtime_dir"]
    if read_marker(runtime / "ORT-PROVISION.txt") != ORT_PROVISION_EXPECTED:
        raise Fail("ONNX Runtime provision is not %s - run native-deps/fetch-deps.py so the "
                   "binaries and notices match components.toml." % ORT_PROVISION_EXPECTED)

    espeak_expected = ("espeak-ng=1.52.0+horse-hoarse-revert;base=%s\nbuild-script-sha256=%s"
                       % (ESPEAK_BASE_COMMIT,
                          normalized_text_sha256(ROOT / "native-deps" / "build-espeak.py")))
    source_manifest = runtime / "espeak-ng-source.SHA256SUMS.txt"
    espeak_data = runtime / "espeak-ng-data"
    if (read_marker(runtime / "ESPEAK-PROVISION.txt") != espeak_expected or
            not nonempty(source_manifest) or not espeak_data.is_dir() or
            not any(p.is_file() for p in espeak_data.rglob("*"))):
        raise Fail("espeak-ng provision is not %s with a source manifest - run "
                   "native-deps/fetch-deps.py so the binary and corresponding source stay "
                   "paired." % espeak_expected)

    missing = [n for n in prof["runtime_libs"] if not nonempty(runtime / n)]
    if missing:
        raise Fail("Native runtime provision is incomplete at %s (missing or empty: %s) - "
                   "run native-deps/fetch-deps.py." % (runtime, ", ".join(missing)))

    notices = runtime / "notices"
    missing = [n for n in ORT_NOTICES if not nonempty(notices / n)]
    if missing:
        raise Fail("ONNX Runtime notice provision is missing or stale at %s (%s) - run "
                   "native-deps/fetch-deps.py. It provisions the canonical files from the "
                   "same wheel as the runtime DLLs." % (notices, ", ".join(missing)))

    return makensis, espeak_expected, source_manifest, espeak_data


def check_packager(prof):
    """The packaging toolchain, pinned. Its stub, its COPYING and the corresponding-source
    instructions must all describe one version."""
    if prof["packager"] != "nsis":
        raise Fail("No packager check for %r." % prof["packager"])
    print("==> Checking NSIS toolchain (%s)" % NSIS_VERSION)
    if not NSIS_EXE.is_file():
        raise Fail("makensis not found at %s - install NSIS." % NSIS_EXE)
    found = capture([NSIS_EXE, "/VERSION"], "makensis /VERSION failed").strip()
    if found != NSIS_VERSION:
        raise Fail("NSIS version mismatch at %s (found '%s', expected '%s'). Install the "
                   "pinned 3.12 toolchain so the stub and staged COPYING match "
                   "packaging/components.toml and the corresponding-source instructions."
                   % (NSIS_EXE, found, NSIS_VERSION))
    copying = NSIS_EXE.parent / "COPYING"
    if not nonempty(copying):
        raise Fail("NSIS COPYING not found at %s - the installed NSIS is missing its licence "
                   "file; the LZMA-compressed stub must ship NSIS's licence terms." % copying)
    return NSIS_EXE


# --- build ------------------------------------------------------------------------------------

def build_clients(prof):
    """The extra artifacts that ship beside the two main binaries. On Windows these are the
    x86 Kindle clients; they are always rebuilt, because they are cheap and `--skip-build`
    is about the expensive pair."""
    built = {}
    for crate, triple, artifact in prof["client_crates"]:
        print("==> cargo build --release --target %s (%s)" % (triple, crate))
        run(["cargo", "build", "--release", "--target", triple], cwd=ROOT / crate,
            what="%s build failed (need the %s target?)" % (crate, triple))
        built[artifact] = ROOT / crate / "target" / triple / "release" / artifact
    return built


def main(argv=None):
    ap = argparse.ArgumentParser(description="Build the installer.")
    ap.add_argument("--skip-build", action="store_true",
                    help="reuse existing release binaries (provenance must match)")
    args = ap.parse_args(argv)

    from dotnet_compat import read_all_text

    prof = profile()
    print("==> Building on %s" % target_platform.describe())
    makensis, espeak_expected, espeak_source_manifest, espeak_data = preflight(prof)

    host_rel = ROOT / "kokoro-host" / "target" / "release"
    panel_rel = ROOT / "kokoro-panel" / "target" / "release"
    host_exe = host_rel / target_platform.exe_name("kokoro-host")
    panel_exe = panel_rel / target_platform.exe_name("kokoro-panel")

    clients = build_clients(prof)

    # Release-build both Rust crates (each stages its own runtime next to the exe).
    if not args.skip_build:
        for crate in ("kokoro-host", "kokoro-panel"):
            print("==> cargo build --release (%s)" % crate)
            run(["cargo", "build", "--release"], cwd=ROOT / crate,
                what="%s build failed" % crate)

    # `--skip-build` is safe only when the reusable host/panel outputs were built from this
    # exact tracked tree and Rust toolchain. Without these records, a clean release checkout
    # could pair old target/ binaries with newer corresponding source while every Git check
    # still passed.
    project_record = host_rel / "kkr-project-source.SHA256SUMS.txt"
    rustc_record = host_rel / "kkr-build-rustc.txt"
    output_record = host_rel / "kkr-build-outputs.SHA256SUMS.txt"

    current_project = project_source_manifest()
    current_rustc = [ln for ln in
                     capture(["rustc", "--version", "--verbose"],
                             "rustc --version --verbose failed.").replace("\r\n", "\n").split("\n")
                     if ln]
    if not current_rustc:
        raise Fail("rustc --version --verbose failed.")
    # A standalone cargo build can replace either exe without touching the source/toolchain
    # records. Bind those records to the actual outputs before accepting --skip-build.
    current_outputs = ["%s  %s" % (sha256_file(p), p.name) for p in (host_exe, panel_exe)]

    if args.skip_build:
        if not all(r.is_file() for r in (project_record, rustc_record, output_record)):
            raise Fail("--skip-build requires provenance from a prior successful full build.")
        if (not records_match(read_record(project_record), current_project) or
                not records_match(read_record(rustc_record), current_rustc) or
                not records_match(read_record(output_record), current_outputs)):
            raise Fail("--skip-build provenance does not match this source tree/toolchain/"
                       "output; run a full build.")
    else:
        write_record(project_record, current_project)
        write_record(rustc_record, current_rustc)
        write_record(output_record, current_outputs)

    # --- stage ---------------------------------------------------------------------------
    stage = HERE / "staging"
    shutil.rmtree(stage, ignore_errors=True)
    (stage / "resources").mkdir(parents=True)

    shutil.copy2(host_exe, stage)
    shutil.copy2(panel_exe, stage)
    # Staging reads the provision directly, not the host target directory. With
    # --skip-build the latter can hold libraries copied by an older host build and would
    # therefore bypass the version markers checked above.
    for name in prof["runtime_libs"]:
        shutil.copy2(prof["runtime_dir"] / name, stage)
    shutil.copytree(espeak_data, stage / espeak_data.name)
    shutil.copy2(ROOT / "icons" / "icon.ico", stage / "icon.ico")

    # Freeze the build records beside this staging tree. Source packaging must describe this
    # installer even if another build or native provision replaces target/runtime in
    # between. This directory is packaging metadata; installer.nsi does not install it.
    provenance = stage / "provenance"
    provenance.mkdir()
    for record in (project_record, output_record,
                   prof["runtime_dir"] / "ESPEAK-PROVISION.txt", espeak_source_manifest):
        shutil.copy2(record, provenance)

    # The Cloud Reader OCR models are NOT bundled. Like the Kokoro voice model they are
    # DOWNLOADED at first run by the panel into <app_data>/ocr/, per ocr-manifest.json and
    # SHA-256-verified. So there is nothing to stage: a fresh install ships no ocr/ dir, the
    # host answers /ocr with `missing` until the download lands, and the extension surfaces
    # that state. fetch-ocr-models.py still provisions them for DEV.

    stage_notices(stage, prof, makensis)

    res = stage / "resources"
    for artifact in clients.values():
        shutil.copy2(artifact, res)
    for script in prof["resource_scripts"]:
        shutil.copy2(script, res)

    # --- package -------------------------------------------------------------------------
    print("==> makensis")
    run([makensis, HERE / "installer.nsi"], what="makensis failed")

    built = sorted(HERE.glob("*-setup%s" % target_platform.package_suffix()),
                   key=lambda p: p.stat().st_mtime, reverse=True)
    if not built:
        raise Fail("makensis produced no installer.")
    out = built[0]
    print("==> Installer: %s  (%.1f MB)" % (out, out.stat().st_size / (1024 * 1024)))


def stage_notices(stage, prof, makensis):
    """The licence/notice tree that must accompany the binaries.

    The bundle links espeak-ng (GPL-3.0-or-later, and MODIFIED - see build-espeak.py) and
    Slint under its GPL-3.0-only option, so the installed app as a whole is conveyed under
    GPLv3: the notices and the GPL text must ship WITH the binaries, not merely live in the
    repo. Every step here throws rather than shipping without - an installer missing licence
    text looks complete and is not.
    """
    from dotnet_compat import read_all_text

    shutil.copy2(ROOT / "LICENSE", stage)
    shutil.copy2(ROOT / "THIRD_PARTY_NOTICES.md", stage)
    shutil.copy2(ROOT / "legal.html", stage)
    shutil.copytree(ROOT / "licenses", stage / "licenses")

    # ONNX Runtime's own licence + notice set, staged from native-deps rather than kept in
    # the repo so it stays matched to the exact wheel the shipped libraries came out of.
    # fetch-deps preserves the wheel's own directory structure, so this copies recursively:
    # a flat copy would silently drop everything inside a subdirectory.
    ort_stage = stage / "licenses" / "onnxruntime"
    ort_stage.mkdir(parents=True, exist_ok=True)
    copy_tree_contents(prof["runtime_dir"] / "notices", ort_stage)

    # espeak-ng's own COPYING* set (GPLv3 + Apache + BSD2 + UCD). We ship a MODIFIED
    # espeak-ng library and espeak-ng-data/, and COPYING.UCD in particular covers the
    # Unicode data baked into that data directory - a DIFFERENT document from
    # licenses/Unicode-3.0.txt.
    espeak_notices = ROOT / "native-deps" / "espeak-ng-notices"
    missing = [n for n in ESPEAK_NOTICES if not nonempty(espeak_notices / n)]
    if missing:
        raise Fail("Incomplete espeak-ng notices at %s - run native-deps/fetch-deps.py "
                   "(missing or empty: %s). It provisions the exact COPYING* set alongside "
                   "the espeak build." % (espeak_notices, ", ".join(missing)))
    espeak_stage = stage / "licenses" / "espeak-ng"
    espeak_stage.mkdir(parents=True, exist_ok=True)
    for src in sorted(espeak_notices.iterdir()):
        if src.is_file():
            shutil.copy2(src, espeak_stage)

    # The Cargo dependency closure's notices - generated fresh from the Cargo.lock files
    # this build just compiled against. Regenerating every build, rather than provisioning
    # once like the ORT notices, is deliberate: this closure moves with ordinary
    # `cargo update`s in a way the exact pinned wheel does not.
    print("==> Generating Rust dependency licence notices")
    import generate_dependency_licenses
    generate_dependency_licenses.main([])
    dep_stage = stage / "licenses" / "dependencies"
    dep_stage.mkdir(parents=True, exist_ok=True)
    for html in sorted((HERE / "dependency-licenses").glob("*.html")):
        shutil.copy2(html, dep_stage)

    # The Rust standard library is statically linked into every Rust output but is NOT a
    # Cargo package, so cargo-about cannot see it. Rust ships a generated per-toolchain
    # COPYRIGHT-library.html covering std/core/alloc/compiler-builtins and their bundled
    # source dependencies. Stage that exact file from the sysroot used for this build, plus
    # rustc's release and immutable commit, rather than checking in a copy that would drift
    # whenever the `stable` toolchain moves.
    sysroot = capture(["rustc", "--print", "sysroot"], "rustc --print sysroot failed.").strip()
    if not sysroot:
        raise Fail("rustc --print sysroot failed.")
    std_notice = Path(sysroot) / "share" / "doc" / "rust" / "COPYRIGHT-library.html"
    if not std_notice.is_file():
        raise Fail("Rust standard-library notice not found at %s - the Rust toolchain cannot "
                   "be redistributed without its generated copyright/licence report."
                   % std_notice)
    if "Copyright notices for The Rust Standard Library" not in read_all_text(std_notice):
        raise Fail("Unexpected Rust standard-library notice content: %s" % std_notice)
    toolchain = [ln for ln in
                 capture(["rustc", "--version", "--verbose"]).replace("\r\n", "\n").split("\n")
                 if ln]
    if not re.search(r"^commit-hash:\s+[0-9a-f]{40}\s*$", "\n".join(toolchain), re.M):
        raise Fail("Could not record rustc release + immutable commit for the shipped "
                   "standard library.")
    rust_stage = stage / "licenses" / "rust"
    rust_stage.mkdir(parents=True, exist_ok=True)
    shutil.copy2(std_notice, rust_stage / "COPYRIGHT-library.html")
    write_record(rust_stage / "TOOLCHAIN.txt", toolchain)

    # The packager's own licence. The NSIS stub is LZMA-compressed (installer.nsi:
    # SetCompressor /SOLID lzma), so the shipped stub carries NSIS's zlib/libpng + bzip2 +
    # CPL-1.0 (LZMA module, with its linking exception) terms. Ship its COPYING verbatim
    # from the installed toolchain so it always matches the version this build used.
    nsis_stage = stage / "licenses" / "nsis"
    nsis_stage.mkdir(parents=True, exist_ok=True)
    shutil.copy2(makensis.parent / "COPYING", nsis_stage / "NSIS-COPYING.txt")


def copy_tree_contents(src, dest):
    """Copy the CHILDREN of `src` into `dest`, preserving structure."""
    for item in sorted(Path(src).iterdir()):
        target = Path(dest) / item.name
        if item.is_dir():
            shutil.copytree(item, target, dirs_exist_ok=True)
        else:
            shutil.copy2(item, target)


if __name__ == "__main__":
    try:
        main()
    except Fail as e:
        raise SystemExit(str(e))
