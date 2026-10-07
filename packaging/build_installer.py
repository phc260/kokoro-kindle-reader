#!/usr/bin/env python3
r"""Build the installer: compile the shipped binaries, stage everything the package needs,
then hand the staging tree to the platform's packager.

    python3 packaging/build_installer.py              # full: build + stage + package
    python3 packaging/build_installer.py --skip-build # reuse existing release binaries

Output, per platform:
    Windows: packaging/dist/kokoro-kindle-reader-<version>-setup.exe   (NSIS)
    Linux:   packaging/dist/kokoro-kindle-reader_<version>_amd64.deb   (dpkg-deb)

**The logic here is platform-agnostic; the platform-specific facts are the `PROFILES`
table.** Which binaries and runtime libraries ship, what the extra client artifacts are,
which packager turns a staging tree into an installable file - those differ. The order of
operations, the provenance contract, the notice staging and every fail-closed check do not.
Both platforms stage the same flat app directory, each into its own staging/<os>/; only
the last step differs, and that is the `PACKAGERS` dispatch at the bottom.
"""

import argparse
import hashlib
import os
import re
import runpy
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))

import target_platform  # noqa: E402

# --- platform facts ----------------------------------------------------------------------

WINDOWS = {
    # Provisioned native runtime: where fetch-deps puts it, and what must be in it.
    "runtime_dir": ROOT / "native-deps" / "windows" / "runtime",
    "runtime_libs": ["onnxruntime.dll", "onnxruntime_providers_shared.dll",
                     "dxcompiler.dll", "dxil.dll", "espeak-ng.dll"],
    # The x64 crates release-built here (each is its crate's own binary, by the crate's
    # name) - the expensive pair that --skip-build may reuse.
    "binaries": ["kokoro-host", "kokoro-panel"],
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
    # Other files placed in the app dir: staged path -> source.
    "app_files": {"icon.ico": ROOT / "icons" / "icon.ico"},
    # legal.html: the About page the tray and Settings open (legal.rs).
    "legal_page": True,
    "packager": "nsis",
}

LINUX = {
    "runtime_dir": ROOT / "native-deps" / "linux" / "runtime",
    # What the host opens at run time, by exactly these names - which is not everything
    # the provision holds. `native_synth::init_ort` opens `libonnxruntime.so` by path; ORT
    # dlopens `libonnxruntime_providers_shared.so` by name; espeak IS linked, so the loader
    # resolves it by its SONAME, `libespeak-ng.so.1`. All three are found beside the exe
    # through the `$ORIGIN` rpath build.rs sets. The versioned ORT copy and espeak's bare
    # link name are build-time files nothing loads, and together another 24 MB.
    "runtime_libs": ["libonnxruntime.so", "libonnxruntime_providers_shared.so",
                     "libespeak-ng.so.1"],
    # The host alone. The panel does not build for Linux yet (the port plan's
    # desktop-integration step), and the Kindle clients are Windows's by nature.
    "binaries": ["kokoro-host"],
    "client_crates": [],
    "resource_scripts": [],
    # Without the panel nothing on Linux downloads the voice and OCR models, so the package
    # ships the headless downloader instead, laid out as it is in the repo (the script in a
    # directory beside the two manifests) so its own path arithmetic finds them.
    "app_files": {
        "tools/fetch-model.py": ROOT / "native-deps" / "fetch-model.py",
        "tools/provision_util.py": ROOT / "native-deps" / "provision_util.py",
        "model-manifest.json": ROOT / "model-manifest.json",
        "ocr-manifest.json": ROOT / "ocr-manifest.json",
    },
    # No tray or panel opens an About page here, and legal.html's links name the panel, the
    # x86 clients and the NSIS stub - five dead links in this package. It is pinned
    # content, so it is left out rather than edited; verify_installer_notices.OMITTED
    # records the same decision on the checking side.
    "legal_page": False,
    "packager": "deb",
}

PROFILES = {"windows": WINDOWS, "linux": LINUX}

ESPEAK_BASE_COMMIT = "4870adfa25b1a32b4361592f1be8a40337c58d6c"
ORT_NOTICES = ["ORT-LICENSE.txt", "ORT-ThirdPartyNotices.txt"]
ESPEAK_NOTICES = ["COPYING", "COPYING.APACHE", "COPYING.BSD2", "COPYING.UCD"]
NSIS_VERSION = "v3.12"
NSIS_EXE = Path(r"C:\Program Files (x86)\NSIS\makensis.exe")
# Each platform's packaging data lives in packaging/<os>/; everything at packaging/'s root is
# shared. The script's paths are relative to its own directory, which makensis changes into.
NSIS_SCRIPT = HERE / "windows" / "installer.nsi"
DEB_DATA_DIR = HERE / "linux"

# .NET's File.WriteAllLines uses Environment.NewLine and appends one after the last line.
# The provenance records are build-local, never shipped and never compared across machines,
# so matching the platform here costs nothing and keeps a Python-written record
# byte-identical to the PowerShell one it replaces.
RECORD_EOL = "\r\n" if sys.platform.startswith("win") else "\n"


def profile():
    os_name = target_platform.current_os()
    p = PROFILES.get(os_name)
    if p is None:
        raise Fail("No installer profile for %s; add a row to PROFILES rather than letting "
                   "this fall through to another platform's." % os_name)
    return dict(p, os=os_name)


def ort_provision_expected(os_name):
    """The ORT-PROVISION.txt this build must find: produced by fetch-deps.py's own
    `ort_marker`, so the wheel pin exists once. fetch-deps has a `__main__` guard, so
    loading it runs nothing."""
    fetch_deps = runpy.run_path(str(ROOT / "native-deps" / "fetch-deps.py"),
                                run_name="fetch_deps")
    return fetch_deps["ort_marker"](os_name)


class Fail(RuntimeError):
    """A build-stopping condition. Every one of these means: do not ship."""


# ERROR_SYSTEM_INTEGRITY_POLICY_VIOLATION: Windows refused to start the file at all. Smart App
# Control does this to any unsigned executable, and makensis.exe is unsigned, so with it on the
# build dies at the NSIS check - as a CreateProcess traceback unless it is caught here.
BLOCKED_BY_POLICY = 4551


def spawn(cmd, **kwargs):
    try:
        return subprocess.run([str(c) for c in cmd], **kwargs)
    except OSError as e:
        if getattr(e, "winerror", None) != BLOCKED_BY_POLICY:
            raise
        raise Fail("Windows blocked %s from running: an Application Control policy, usually "
                   "Smart App Control, which blocks unsigned executables (doctors\\doctor.cmd "
                   "reports it). Build the installer in CI (installer.yml) instead, or turn "
                   "Smart App Control off in Windows Security - it cannot be turned back on."
                   % cmd[0]) from None


def run(cmd, cwd=None, what=None):
    rc = spawn(cmd, cwd=str(cwd) if cwd else None).returncode
    if rc:
        raise Fail(what or ("%s failed" % cmd[0]))


def capture(cmd, what=None):
    proc = spawn(cmd, capture_output=True)
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
    # In this process rather than a second interpreter. No arguments = the repository root;
    # it raises SystemExit on a mismatch, as it would on the command line.
    import verify_license_texts
    try:
        verify_license_texts.main([])
    except SystemExit as exc:
        if exc.code:
            raise Fail("checked-in licence-text verification FAILED (see above).")

    packager = PACKAGERS[prof["packager"]]["check"]()

    # Provisioned notices must be current too - checked before any cargo build so a stale
    # dependency cache costs seconds.
    runtime = prof["runtime_dir"]
    ort_expected = ort_provision_expected(prof["os"])
    if read_marker(runtime / "ORT-PROVISION.txt") != ort_expected:
        raise Fail("ONNX Runtime provision is not %s - run native-deps/fetch-deps.py so the "
                   "binaries and notices match components.toml." % ort_expected)

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

    return packager, espeak_expected, source_manifest, espeak_data


def check_nsis():
    """The packaging toolchain, pinned. Its stub, its COPYING and the corresponding-source
    instructions must all describe one version.

    Returns what the rest of the build needs from a packager: the tool, and the licence
    files it puts INTO the package (staged path -> source). NSIS writes its own
    LZMA-compressed stub into every -setup.exe, so its COPYING ships."""
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
    return {"tool": NSIS_EXE, "notices": {"licenses/nsis/NSIS-COPYING.txt": copying}}


def check_deb():
    """dpkg-deb builds the archive; dpkg-shlibdeps derives `Depends:` from the binaries
    themselves (dpkg-dev on Debian/Ubuntu).

    Neither is pinned the way NSIS is, because neither puts any of its own code into the
    package: a .deb is an `ar` of two tarballs and a version string, where an NSIS
    installer is an executable stub NSIS wrote. So there is no packager licence to ship
    and no packager source to offer - the pin on NSIS exists for exactly those two reasons.
    """
    print("==> Checking the Debian packaging tools")
    tools = {}
    for name, pkg in (("dpkg-deb", "dpkg"), ("dpkg-shlibdeps", "dpkg-dev")):
        found = shutil.which(name)
        if not found:
            raise Fail("%s not found - install %s (sudo apt install %s)." % (name, pkg, pkg))
        tools[name] = found
    return {"tool": tools, "notices": {}}


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
    packager, espeak_expected, espeak_source_manifest, espeak_data = preflight(prof)

    binaries = [ROOT / crate / "target" / "release" / target_platform.exe_name(crate)
                for crate in prof["binaries"]]

    clients = build_clients(prof)

    # Release-build the shipped Rust crates (each stages its own runtime next to the exe).
    if not args.skip_build:
        for crate in prof["binaries"]:
            print("==> cargo build --release (%s)" % crate)
            run(["cargo", "build", "--release"], cwd=ROOT / crate,
                what="%s build failed" % crate)

    # `--skip-build` is safe only when the reusable host/panel outputs were built from this
    # exact tracked tree and Rust toolchain. Without these records, a clean release checkout
    # could pair old target/ binaries with newer corresponding source while every Git check
    # still passed.
    #
    # They live beside the host's output, in a folder per OS: target/ is shared by both
    # platforms when one checkout is built from each side of a dual boot, and one set of
    # names meant a Linux build overwrote the Windows records - so Windows's --skip-build
    # refused over binaries nobody had touched. The file names stay fixed; the frozen copies
    # in staging/<os>/provenance/ are read by those names.
    records = ROOT / "kokoro-host" / "target" / "release" / "kkr-provenance" / prof["os"]
    project_record = records / "kkr-project-source.SHA256SUMS.txt"
    rustc_record = records / "kkr-build-rustc.txt"
    output_record = records / "kkr-build-outputs.SHA256SUMS.txt"

    current_project = project_source_manifest()
    current_rustc = [ln for ln in
                     capture(["rustc", "--version", "--verbose"],
                             "rustc --version --verbose failed.").replace("\r\n", "\n").split("\n")
                     if ln]
    if not current_rustc:
        raise Fail("rustc --version --verbose failed.")
    # A standalone cargo build can replace any exe without touching the source/toolchain
    # records. Bind those records to the actual outputs before accepting --skip-build.
    current_outputs = ["%s  %s" % (sha256_file(p), p.name) for p in binaries]

    if args.skip_build:
        if not all(r.is_file() for r in (project_record, rustc_record, output_record)):
            raise Fail("--skip-build requires provenance from a prior successful full build.")
        if (not records_match(read_record(project_record), current_project) or
                not records_match(read_record(rustc_record), current_rustc) or
                not records_match(read_record(output_record), current_outputs)):
            raise Fail("--skip-build provenance does not match this source tree/toolchain/"
                       "output; run a full build.")
    else:
        records.mkdir(parents=True, exist_ok=True)
        write_record(project_record, current_project)
        write_record(rustc_record, current_rustc)
        write_record(output_record, current_outputs)

    # --- stage ---------------------------------------------------------------------------
    # Per OS, so a build on one platform never wipes the other's tree or its provenance/.
    stage = target_platform.staging_dir(prof["os"])
    shutil.rmtree(stage, ignore_errors=True)
    stage.mkdir(parents=True)

    for exe in binaries:
        shutil.copy2(exe, stage)
    # Staging reads the provision directly, not the host target directory. With
    # --skip-build the latter can hold libraries copied by an older host build and would
    # therefore bypass the version markers checked above.
    for name in prof["runtime_libs"]:
        shutil.copy2(prof["runtime_dir"] / name, stage)
    shutil.copytree(espeak_data, stage / espeak_data.name)
    for rel, src in prof["app_files"].items():
        (stage / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, stage / rel)

    # Freeze the build records beside this staging tree. Source packaging must describe this
    # installer even if another build or native provision replaces target/runtime in
    # between. This directory is packaging metadata; no packager installs it.
    provenance = stage / "provenance"
    provenance.mkdir()
    for record in (project_record, output_record,
                   prof["runtime_dir"] / "ESPEAK-PROVISION.txt", espeak_source_manifest):
        shutil.copy2(record, provenance)

    # The Cloud Reader OCR models are NOT bundled. Like the Kokoro voice model they are
    # DOWNLOADED at first run into <app_data>/ocr/, per ocr-manifest.json and
    # SHA-256-verified - by the panel on Windows, by the shipped fetch-model.py on Linux. So
    # there is nothing to stage: a fresh install ships no ocr/ dir, the host answers /ocr
    # with `missing` until the download lands, and the extension surfaces that state.
    # fetch-ocr-models.py still provisions them for a DEBUG build.

    stage_notices(stage, prof, packager)

    if clients or prof["resource_scripts"]:
        res = stage / "resources"
        res.mkdir()
        for artifact in clients.values():
            shutil.copy2(artifact, res)
        for script in prof["resource_scripts"]:
            shutil.copy2(script, res)

    # --- package -------------------------------------------------------------------------
    # Created here for both packagers: makensis does not create its OutFile's folder (it
    # fails with "Can't open output file"), and a fresh checkout has no dist/.
    target_platform.DIST_DIR.mkdir(exist_ok=True)
    out = PACKAGERS[prof["packager"]]["package"](stage, packager)
    print("==> Installer: %s  (%.1f MB)" % (out, out.stat().st_size / (1024 * 1024)))


def stage_notices(stage, prof, packager):
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
    if prof["legal_page"]:
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
    generate_dependency_licenses.main(["--platform", prof["os"]])
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

    # The packager's own licence, where the packager puts code of its own into the package.
    # The NSIS stub is LZMA-compressed (installer.nsi: SetCompressor /SOLID lzma), so the
    # shipped stub carries NSIS's zlib/libpng + bzip2 + CPL-1.0 (LZMA module, with its
    # linking exception) terms; its COPYING is shipped verbatim from the installed toolchain
    # so it always matches the version this build used. dpkg-deb contributes none.
    for rel, src in packager["notices"].items():
        (stage / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, stage / rel)


def copy_tree_contents(src, dest):
    """Copy the CHILDREN of `src` into `dest`, preserving structure."""
    for item in sorted(Path(src).iterdir()):
        target = Path(dest) / item.name
        if item.is_dir():
            shutil.copytree(item, target, dirs_exist_ok=True)
        else:
            shutil.copy2(item, target)


# --- packagers ---------------------------------------------------------------------------

def package_nsis(stage, packager):
    print("==> makensis")
    run([packager["tool"], NSIS_SCRIPT], what="makensis failed")
    built = sorted(target_platform.DIST_DIR.glob(target_platform.package_glob("windows")),
                   key=lambda p: p.stat().st_mtime, reverse=True)
    if not built:
        raise Fail("makensis produced no installer.")
    return built[0]


DEB_PACKAGE = "kokoro-kindle-reader"
# The app dir: the staged tree as-is, so the host finds its libraries through `$ORIGIN` and
# espeak-ng-data beside `current_exe()` exactly as it does in target/release, and
# legal.html's relative links resolve. /usr/lib/<package> is Debian's place for a
# package's private executables and libraries - these must not be on the system library
# path, where a distribution's own (unmodified) libespeak-ng would be told apart by nothing.
DEB_APP_DIR = "usr/lib/%s" % DEB_PACKAGE
# /usr/bin links -> their target inside the app dir. Both resolve to the real file before
# they look for anything beside themselves (`current_exe()` reads /proc/self/exe; the
# script resolves `__file__`), so the link is only a name on PATH.
DEB_COMMANDS = {"kokoro-host": "kokoro-host", "kokoro-fetch-models": "tools/fetch-model.py"}
# The only files that ship executable. Shared libraries are 0644 in a .deb; everything
# else is data.
DEB_EXECUTABLES = ["kokoro-host", "tools/fetch-model.py"]
# Checked-in data files placed outside the app dir: source in DEB_DATA_DIR -> path in the
# package. Rendered with LF whatever the checkout did to them - systemd reads a CR as part
# of the value, and this tree is also checked out on Windows.
DEB_DATA = {
    "kokoro-host.service": "usr/lib/systemd/user/kokoro-host.service",
    "README.Debian": "usr/share/doc/%s/README.Debian" % DEB_PACKAGE,
    "copyright": "usr/share/doc/%s/copyright" % DEB_PACKAGE,
}
DEB_CONTROL = """\
Package: %(package)s
Version: %(version)s
Architecture: %(arch)s
Maintainer: Alan P.H. Chiu <phc260@nyu.edu>
Installed-Size: %(installed_kib)d
Depends: %(depends)s
Section: sound
Priority: optional
Homepage: https://github.com/phc260/kokoro-kindle-reader
Description: Local Kokoro-82M narrator for Kindle Cloud Reader
 Runs the Kokoro-82M text-to-speech model and PP-OCR text recognition on this
 machine and serves them on 127.0.0.1 to the Kokoro Kindle Reader browser
 extension (Chrome or Edge).
 .
 After installing, run kokoro-fetch-models, then
 systemctl --user enable --now kokoro-host. See
 /usr/share/doc/kokoro-kindle-reader/README.Debian.
"""


def cargo_version(manifest):
    """The `[package]` version: the first top-level `version =` line, which in these
    manifests is the package's own (dependencies spell theirs inside `{ ... }`)."""
    from dotnet_compat import read_all_text
    m = re.search(r'^version\s*=\s*"([0-9][0-9A-Za-z.+~-]*)"\s*$', read_all_text(manifest), re.M)
    if not m:
        raise Fail("No package version found in %s." % manifest)
    return m.group(1)


def write_lf(src, dest):
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(src.read_bytes().replace(b"\r\n", b"\n"))


def normalize_modes(root, executables):
    """Set every mode explicitly - 0755 directories and executables, 0644 everything else -
    and then prove the filesystem kept them.

    Copied modes cannot be trusted here. This checkout lives on NTFS on a dual-boot
    machine, where every file reads back 0777, and dpkg-deb records whatever it finds: the
    package would install world-writable files into /usr/lib (and dpkg-deb refuses a
    DEBIAN/ that is group- or world-writable). A filesystem that ignores chmod would make
    that silent, so the result is re-read rather than assumed.
    """
    want = {}
    for dirpath, _dirs, files in os.walk(root):
        want[dirpath] = 0o755
        for name in files:
            path = os.path.join(dirpath, name)
            if not os.path.islink(path):
                want[path] = 0o644
    for exe in executables:
        if not os.path.isfile(exe):
            raise Fail("%s should ship executable but is not in the package." % exe)
        want[str(exe)] = 0o755
    for path, mode in want.items():
        os.chmod(path, mode)
    wrong = [p for p, mode in want.items() if stat.S_IMODE(os.stat(p).st_mode) != mode]
    if wrong:
        raise Fail("The package tree did not keep its file modes (e.g. %s) - %s is on a "
                   "filesystem that ignores chmod. Point TMPDIR at a Linux filesystem."
                   % (wrong[0], tempfile.gettempdir()))


def is_elf(path):
    with open(path, "rb") as f:
        return f.read(4) == b"\x7fELF"


def deb_depends(work, root, shlibdeps):
    """`Depends:` read off the binaries by dpkg-shlibdeps, not written by hand.

    Every ELF in the package is asked, found by its magic rather than listed, so a library
    added to the profile is covered without anyone remembering to. dpkg-shlibdeps needs a
    `debian/control` in its working directory and the package tree at
    `debian/<package>/DEBIAN` to recognize the libraries we ship ourselves (found through
    `$ORIGIN`) as ours rather than as undeclared dependencies - which is why the tree is
    laid out that way.
    """
    control = work / "debian" / "control"
    control.write_text("Source: %s\n\nPackage: %s\nArchitecture: any\n"
                       % (DEB_PACKAGE, DEB_PACKAGE), encoding="ascii")
    elves = [p for p in sorted(root.rglob("*"))
             if p.is_file() and not p.is_symlink() and is_elf(p)]
    proc = subprocess.run([shlibdeps, "-O"] + ["-e%s" % p for p in elves],
                          cwd=str(work), capture_output=True)
    if proc.returncode:
        sys.stderr.write(proc.stderr.decode("utf-8", "replace"))
        raise Fail("dpkg-shlibdeps failed.")
    control.unlink()
    for line in proc.stdout.decode("utf-8", "replace").splitlines():
        if line.startswith("shlibs:Depends="):
            return [d.strip() for d in line.split("=", 1)[1].split(",") if d.strip()]
    raise Fail("dpkg-shlibdeps reported no shared-library dependencies for %d binaries."
               % len(elves))


def package_deb(stage, packager):
    version = cargo_version(ROOT / "kokoro-host" / "Cargo.toml")
    out = target_platform.DIST_DIR / ("%s_%s_%s.deb" % (DEB_PACKAGE, version, target_platform.deb_arch()))
    print("==> dpkg-deb (%s)" % out.name)

    # Laid out in a temporary directory rather than under packaging/, for the modes: see
    # normalize_modes.
    work = Path(tempfile.mkdtemp(prefix="kkr-deb-"))
    try:
        root = work / "debian" / DEB_PACKAGE
        app = root / DEB_APP_DIR
        shutil.copytree(stage, app,
                        ignore=lambda d, names: ["provenance"] if Path(d) == stage else [])
        bindir = root / "usr" / "bin"
        bindir.mkdir(parents=True)
        for name, target in DEB_COMMANDS.items():
            if not (app / target).is_file():
                raise Fail("/usr/bin/%s would point at %s, which is not staged."
                           % (name, target))
            os.symlink(os.path.relpath(app / target, bindir), bindir / name)
        for src, dest in DEB_DATA.items():
            write_lf(DEB_DATA_DIR / src, root / dest)
        (root / "DEBIAN").mkdir()
        normalize_modes(root, [app / e for e in DEB_EXECUTABLES])

        # python3 runs kokoro-fetch-models; nothing ELF-shaped says so.
        depends = deb_depends(work, root, packager["tool"]["dpkg-shlibdeps"]) + ["python3"]
        installed = sum(os.lstat(os.path.join(d, f)).st_size
                        for d, _dirs, files in os.walk(root) for f in files)
        control = root / "DEBIAN" / "control"
        control.write_text(DEB_CONTROL % {
            "package": DEB_PACKAGE, "version": version, "arch": target_platform.deb_arch(),
            "installed_kib": (installed + 1023) // 1024, "depends": ", ".join(depends),
        }, encoding="utf-8")
        os.chmod(control, 0o644)

        if out.exists():
            out.unlink()
        # xz, named rather than left to dpkg-deb's default, which is zstd on Ubuntu: xz is
        # what every dpkg reads and what Python's tarfile opens without a third-party
        # module, and verify_installer_notices.py unpacks this with nothing else.
        run([packager["tool"]["dpkg-deb"], "--root-owner-group", "-Zxz", "--build",
             root, out], what="dpkg-deb failed")
    finally:
        shutil.rmtree(work, ignore_errors=True)
    return out


PACKAGERS = {
    "nsis": {"check": check_nsis, "package": package_nsis},
    "deb": {"check": check_deb, "package": package_deb},
}


if __name__ == "__main__":
    try:
        main()
    except Fail as e:
        raise SystemExit(str(e))
