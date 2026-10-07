#!/usr/bin/env python3
r"""Post-build check: unpack the produced installer and assert the whole licence/notice
tree is present and non-empty.

GPLv3 requires the notices accompany the binaries; a staging bug or a dropped copy step
produces an installer that LOOKS complete and is not. This is the same "fail loudly"
contract as the OCR-model digests - the installer build is not "done" until this passes.

    python3 packaging/verify_installer_notices.py                 # newest package for this OS
    python3 packaging/verify_installer_notices.py --setup path.exe
    python3 packaging/verify_installer_notices.py --setup packaging/dist/kokoro-kindle-reader_0.4.0_amd64.deb

**Everything here is platform-agnostic except `unpack`.** What must ship, how a shipped
file is located, and every content check are the same questions for any package; only
"how do I get the files out of this thing" differs, and that is one dispatch table. Which
platform's inventory applies is read off the PACKAGE (its suffix), never off the machine
running the check. Paths are compared with `/` internally and matched case-insensitively,
because the inventory is written in the installer's own `\` idiom and the Windows tree it
describes came off a case-insensitive filesystem.
"""

import argparse
import contextlib
import io
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import target_platform  # noqa: E402 - needs the path above
import verify_license_texts  # noqa: E402
from generate_dependency_licenses import reports as dependency_reports  # noqa: E402

# Project-level notices not owned by one non-Cargo component. Unicode-3.0 is the standalone
# text linked by THIRD_PARTY_NOTICES.md for the Rust unicode-ident dependency; the generated
# per-binary reports are checked separately below.
PROJECT_NOTICES = ["LICENSE", "THIRD_PARTY_NOTICES.md", "legal.html", "licenses/Unicode-3.0.txt"]

# Pinned project texts a package deliberately leaves out. legal.html is the page the Windows
# tray and Settings open; its per-binary links name the panel, the three x86 clients and the
# NSIS stub, none of which is in the Linux package, and nothing there opens it. Named here,
# per package, so the omission is a decision this check knows about rather than a file that
# went missing - build_installer.py's `legal_page` is the other half.
OMITTED = {"windows": [], "linux": ["legal.html"]}

# The file that anchors the install root: bare notice names must sit beside it. Named per
# platform rather than hardcoded, so the Linux package looks for `kokoro-host`, not a file
# that cannot exist there.
def root_anchor(os_name=None):
    return target_platform.exe_name("kokoro-host", os_name)


def _norm(p):
    """One separator for comparison. The inventory and this script both speak `\\`; the
    filesystem underneath may not."""
    return str(p).replace("\\", "/")


# --- the one platform-specific step -----------------------------------------------------

def _seven_zip():
    for cand in ("7z", "7za"):
        found = shutil.which(cand)
        if found:
            return found
    for cand in (r"C:\Program Files\7-Zip\7z.exe", r"C:\Program Files (x86)\7-Zip\7z.exe"):
        if os.path.isfile(cand):
            return cand
    raise RuntimeError("7-Zip (7z) not found; needed to unpack the NSIS installer.")


def _unpack_nsis(package, dest):
    exe = _seven_zip()
    proc = subprocess.run([exe, "x", str(package), "-o%s" % dest, "-y"],
                          stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if proc.returncode:
        sys.stderr.write(proc.stderr.decode("utf-8", "replace"))
        raise RuntimeError("7z failed to extract %s" % package)


def _unpack_deb(package, dest):
    """A .deb is an `ar` archive of `debian-binary`, `control.tar.*` and `data.tar.*`; the
    installed files are the last. Read here in Python rather than with dpkg-deb, so a .deb
    can be checked on any machine with nothing installed - which is also why
    build_installer.py compresses it with xz and not dpkg's zstd default.
    """
    with open(package, "rb") as f:
        if f.read(8) != b"!<arch>\n":
            raise RuntimeError("%s is not an ar archive, so not a .deb." % package)
        while True:
            header = f.read(60)
            if not header:
                break
            if len(header) != 60 or header[58:60] != b"`\n":
                raise RuntimeError("%s has a truncated or malformed ar header." % package)
            name = header[:16].decode("ascii").strip().rstrip("/")
            size = int(header[48:58].decode("ascii").strip())
            body = f.read(size)
            if size % 2:
                f.read(1)  # ar pads every member to an even offset
            if not name.startswith("data.tar"):
                continue
            try:
                tar = tarfile.open(fileobj=io.BytesIO(body), mode="r:*")
            except tarfile.ReadError:
                raise RuntimeError("%s's %s is compressed in a format Python cannot read; "
                                   "build_installer.py builds with -Zxz." % (package, name))
            with tar:
                # The "data" filter refuses absolute paths and links leaving the tree; the
                # package's own /usr/bin links are relative and stay inside it.
                if hasattr(tarfile, "data_filter"):
                    tar.extractall(dest, filter="data")
                else:
                    tar.extractall(dest)
            return
    raise RuntimeError("%s has no data.tar member." % package)


# Keyed by package suffix, which is what actually decides how to open it - see
# target_platform.PACKAGE_SUFFIX.
UNPACKERS = {".exe": _unpack_nsis, ".deb": _unpack_deb}


def unpack(package, dest):
    unpacker = UNPACKERS.get(Path(package).suffix.lower())
    if unpacker is None:
        raise RuntimeError("No unpacker for %s; add one to UNPACKERS." % Path(package).suffix)
    unpacker(package, dest)


# --- locating a shipped file ------------------------------------------------------------

class Shipped:
    """The extracted tree, indexed so a required path can be matched by suffix.

    NSIS/7z put the app files under an internal folder, and the .deb under
    usr/lib/kokoro-kindle-reader/, so matching is by relative-path SUFFIX rather than by
    absolute path. Symlinks are not shipped files: the .deb's /usr/bin/kokoro-host is a
    link to the real one, and anchoring on it would put the install root in /usr/bin.

    A BARE name (no directory in the suffix, e.g. `LICENSE`) must be THE install-root file,
    beside the executables - not any nested namesake. The ORT wheel ships its own
    `licenses/onnxruntime/.../LICENSE`, and other subtrees could too (`resources/`,
    `espeak-ng-data/`), so a plain `*/LICENSE` suffix match would let any of them satisfy a
    check for the top-level LICENSE after the real one was dropped.
    """

    def __init__(self, root, anchor_name=None):
        anchor_name = (anchor_name or root_anchor()).lower()
        self.all = [p for p in Path(root).rglob("*") if p.is_file() and not p.is_symlink()]
        anchor = next((p for p in self.all if p.name.lower() == anchor_name), None)
        if anchor is None:
            raise RuntimeError("%s not found in the extracted package; cannot anchor the "
                               "install root." % anchor_name)
        self.install_root = anchor.parent

    def find(self, suffix):
        s = _norm(suffix)
        if "/" not in s:
            want = s.lower()
            for p in self.all:
                if p.parent == self.install_root and p.name.lower() == want:
                    return p
            return None
        want = ("/" + s).lower()
        for p in self.all:
            if _norm(p).lower().endswith(want):
                return p
        return None


# --- components.toml ---------------------------------------------------------------------

# `[ \t\r]*$`, not `[ \t]*$`: components.toml is CRLF, and Python's `$` under re.M matches
# BEFORE the `\n` - with the `\r` still to its left. .NET's `\s` swallows that `\r`, so the
# PowerShell original matched where a `[ \t]*$` port matches nothing at all. It fails closed
# (every component reads as absent), but it fails the build just the same.
_COMPONENT = re.compile(r"^[ \t]*\[\[component\]\][ \t\r]*$", re.M)
_NOTICE_DECL = re.compile(r"^[ \t]*notice[ \t]*=", re.M)
_NOTICE_LINE = re.compile(
    r"^[ \t]*notice[ \t]*=[ \t]*\[(?P<items>[^\r\n]*)\][ \t]*(?:#[^\r\n]*)?[ \t\r]*$", re.M)
_STRING = r'"[^"\\\r\n]+"'
_LIST = re.compile(r"^[ \t]*%s(?:[ \t]*,[ \t]*%s)*[ \t]*$" % (_STRING, _STRING))
_VALUE = re.compile(r'"(?P<value>[^"\\\r\n]+)"')
_PLATFORMS_DECL = re.compile(r"^[ \t]*platforms[ \t]*=", re.M)
_PLATFORMS_LINE = re.compile(
    r"^[ \t]*platforms[ \t]*=[ \t]*\[(?P<items>[^\r\n]*)\][ \t]*(?:#[^\r\n]*)?[ \t\r]*$", re.M)
# PowerShell's wildcard metacharacters, which WildcardPattern.ContainsWildcardCharacters
# reports; a notice must name one exact file, never a pattern.
_WILDCARD = re.compile(r"[*?\[\]]")


def _is_rooted(path):
    """Rooted on ANY platform, not just this one: `/x`, `\\x` and `C:\\x` all qualify.

    `Path.is_absolute()` would answer for the running OS only, so a Linux run of this check
    would wave through a `C:\\...` entry that the Windows installer must never contain.
    """
    return (path.startswith("/") or path.startswith("\\") or
            re.match(r"^[A-Za-z]:", path) is not None)


def component_notice_paths(manifest_path, os_name):
    """The exact per-component notice paths, for the package built for `os_name`, from the
    authoritative non-Cargo inventory.

    Intentionally a narrow, fail-closed parser for components.toml's documented one-line
    `notice = ["path", ...]` schema. Accepting only this small shape avoids a TOML
    dependency while making format drift an error rather than a silently skipped component.

    A component with a `platforms = [...]` line applies to those packages only (the NSIS
    stub is not in a .deb; libonnxruntime.so is not in a -setup.exe); one without applies
    to all. An unknown platform name is an error, not a component that silently matches
    nothing - a misspelt "linux" would otherwise drop its notices from every check.
    """
    from dotnet_compat import ordinal_key, read_all_text
    toml = read_all_text(manifest_path)
    component_count = len(_COMPONENT.findall(toml))
    declaration_count = len(_NOTICE_DECL.findall(toml))
    lines = list(_NOTICE_LINE.finditer(toml))

    if component_count == 0:
        raise RuntimeError("components.toml contains no component blocks.")
    if declaration_count != component_count:
        raise RuntimeError("components.toml must have exactly one notice field per component: "
                           "%d component(s), %d notice field(s)."
                           % (component_count, declaration_count))
    if len(lines) != declaration_count:
        raise RuntimeError("Every components.toml notice field must use the one-line "
                           '`notice = ["path", ...]` schema.')

    paths = []
    for block in _COMPONENT.split(toml)[1:]:
        if len(_NOTICE_DECL.findall(block)) != 1:
            raise RuntimeError("components.toml must have exactly one notice field per "
                               "component.")
        if not _applies(block, os_name):
            continue
        items = _NOTICE_LINE.search(block).group("items")
        if not _LIST.match(items):
            raise RuntimeError("Malformed components.toml notice list: [%s]" % items)
        for value in _VALUE.finditer(items):
            path = value.group("value")
            segments = re.split(r"[/\\]", path)
            if (_is_rooted(path) or path.endswith("/") or path.endswith("\\") or
                    ".." in segments or "." in segments or "" in segments or
                    _WILDCARD.search(path)):
                raise RuntimeError("Component notice must name an exact install-relative "
                                   "file: %s" % path)
            paths.append(path)

    return sorted({p.lower(): p for p in paths}.values(), key=ordinal_key)


def _applies(block, os_name):
    decls = _PLATFORMS_DECL.findall(block)
    if not decls:
        return True
    line = _PLATFORMS_LINE.search(block)
    if len(decls) != 1 or line is None or not _LIST.match(line.group("items")):
        raise RuntimeError("A components.toml platforms field must be one line of the form "
                           '`platforms = ["windows", ...]`.')
    names = [v.group("value") for v in _VALUE.finditer(line.group("items"))]
    unknown = [n for n in names if n not in target_platform.PACKAGE_SUFFIX]
    if unknown:
        raise RuntimeError("components.toml names unknown platform(s) %s; known: %s."
                           % (", ".join(unknown), ", ".join(sorted(target_platform.PACKAGE_SUFFIX))))
    return os_name in names


# --- the checks ---------------------------------------------------------------------------

def verify(setup):
    from dotnet_compat import ordinal_key, read_all_text
    import verify_dependency_licenses

    setup = Path(setup)
    os_name = target_platform.package_os(setup)
    print("==> Verifying the %s package %s on %s"
          % (os_name, setup, target_platform.describe()))

    work = Path(tempfile.gettempdir()) / ("kkr-verify-" + uuid.uuid4().hex)
    work.mkdir(parents=True)
    try:
        unpack(setup, work)
        tree = Shipped(work, root_anchor(os_name))
        root = tree.install_root

        omitted = OMITTED[os_name]
        required = ([n for n in PROJECT_NOTICES if n not in omitted] +
                    component_notice_paths(HERE / "components.toml", os_name))
        required = sorted({r.lower(): r for r in required}.values(), key=ordinal_key)

        missing, empty, group = [], [], []
        for r in required:
            f = tree.find(r)
            if f is None:
                missing.append(r)
            elif f.stat().st_size == 0:
                empty.append(r)

        # The UI's local legal page must lead to files at their actual install-relative
        # paths, not just namesakes elsewhere in the extracted tree.
        legal = tree.find("legal.html")
        if legal is not None and "legal.html" in omitted:
            group.append("legal.html ships, but this package is declared to omit it")
        elif legal is not None:
            for link in re.finditer(r'href="(?P<path>[^"]+)"', read_all_text(legal)):
                rel = link.group("path")
                if rel.startswith("https://"):
                    continue
                dest = root / rel.replace("\\", "/")
                if not dest.is_file() or dest.stat().st_size == 0:
                    group.append("legal.html link is missing or empty: %s" % rel)

        # Presence is insufficient for the fixed, checked-in texts: verify the installer
        # carries the reviewed bytes, not a truncated or wrong-revision file. Provisioned
        # notice trees are intentionally allowed as additions.
        # In this process, with its output hushed since this check builds its own report; it
        # raises SystemExit on a mismatch, as it would on the command line.
        hushed = io.StringIO()
        try:
            with contextlib.redirect_stdout(hushed), contextlib.redirect_stderr(hushed):
                verify_license_texts.main(["--root", str(root), "--allow-additional"] +
                                          ["--omit=%s" % n for n in omitted])
        except SystemExit as exc:
            if exc.code:
                group.append("checked-in licence texts do not match packaging/license-texts.sha256")

        # ORT's own LICENSE + ThirdPartyNotices come from exact canonical paths in
        # components.toml, not a directory marker: a co-location check can be satisfied by
        # an unrelated namesake in a sibling subtree after ORT's real notice is dropped.
        #
        # The Rust Standard Library is outside Cargo's graph. Presence alone is not enough:
        # prove the staged HTML is Rust's generated library-only report and TOOLCHAIN.txt
        # carries the immutable commit identifying the source copied into the
        # corresponding-source archive.
        rust_copyright = tree.find("licenses/rust/COPYRIGHT-library.html")
        if rust_copyright is None or rust_copyright.stat().st_size == 0:
            group.append("licenses\\rust\\COPYRIGHT-library.html (missing or empty)")
        elif "Copyright notices for The Rust Standard Library" not in read_all_text(rust_copyright):
            group.append("licenses\\rust\\COPYRIGHT-library.html (not the generated library report)")

        toolchain = tree.find("licenses/rust/TOOLCHAIN.txt")
        if toolchain is None or toolchain.stat().st_size == 0:
            group.append("licenses\\rust\\TOOLCHAIN.txt (missing or empty)")
        else:
            text = read_all_text(toolchain)
            if (not re.search(r"^release:\s+\S+\s*$", text, re.M) or
                    not re.search(r"^commit-hash:\s+[0-9a-f]{40}\s*$", text, re.M)):
                group.append("licenses\\rust\\TOOLCHAIN.txt (missing release or immutable commit)")

        # The per-binary generated Cargo dependency reports, one per crate that package
        # ships (five on Windows, the host alone on Linux). Each must also carry the
        # exact licence/notice files harvested from its resolved crate packages;
        # cargo-about's normalized SPDX fallback can contain copyright placeholders, so the
        # appendix is load-bearing.
        for name in dependency_reports(os_name):
            rel = "licenses/dependencies/%s" % name
            shown = rel.replace("/", "\\")
            report = tree.find(rel)
            if report is None or report.stat().st_size == 0:
                group.append("%s (missing or empty)" % shown)
                continue
            text = read_all_text(report)
            if 'data-license-appendix="1"' not in text or "data-source-sha256=" not in text:
                group.append("%s (missing exact packaged licence-file appendix)" % shown)
            try:
                verify_dependency_licenses.verify(report)
            except Exception as e:  # noqa: BLE001 - any failure is a notice failure
                group.append("%s (clarified/embedded licence texts missing or changed: %s)"
                             % (shown, e))
    finally:
        shutil.rmtree(work, ignore_errors=True)

    if missing or empty or group:
        if missing:
            print("MISSING:\n  %s" % "\n  ".join(missing))
        if empty:
            print("EMPTY:\n  %s" % "\n  ".join(empty))
        if group:
            print("GROUPS:\n  %s" % "\n  ".join(group))
        raise RuntimeError("Installer notice-tree verification FAILED (see above).")

    print("==> OK: all required licence/notice files present and non-empty in the installer.")


def main(argv=None):
    ap = argparse.ArgumentParser(description="Verify the installer's licence/notice tree.")
    ap.add_argument("--setup", help="the installer to verify (default: the newest built one)")
    args = ap.parse_args(argv)

    setup = args.setup
    pattern = target_platform.package_glob()
    if not setup:
        built = sorted(target_platform.DIST_DIR.glob(pattern),
                       key=lambda p: p.stat().st_mtime, reverse=True)
        setup = built[0] if built else None
    if not setup or not Path(setup).exists():
        raise SystemExit("No installer package found to verify (looked for %s)."
                         % (target_platform.DIST_DIR / pattern))
    try:
        verify(setup)
    except RuntimeError as e:
        raise SystemExit(str(e))


if __name__ == "__main__":
    main()
