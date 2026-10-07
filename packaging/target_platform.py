"""What platform is this, and what does the build call things there.

The packaging scripts are the platform-agnostic layer: the LOGIC (what must ship, what is
verified, what is staged) is one implementation, and the places where an OS genuinely
differs are these tables. That is the same shape `native-deps/fetch-deps.py` uses for the
ONNX Runtime wheels - platform differences belong somewhere they can be read side by side,
not spread through the code as `if windows` at every use.

**Only combinations that really exist here are populated.** An unsupported OS or
architecture raises rather than falling back to a guess: a packaging script that silently
assumes Windows on an unknown platform produces an artifact nobody checked.
"""

import os
import platform
import sys
from pathlib import Path

# Rust target triples this project actually builds, per (os, arch). The x86 artifacts
# (kokoro-sapi/hook/inject) are a separate axis - Kindle is a 32-bit Windows process, so
# they are Windows-only by nature and are not in this table.
NATIVE_TRIPLE = {
    ("windows", "x86_64"): "x86_64-pc-windows-msvc",
    ("linux", "x86_64"): "x86_64-unknown-linux-gnu",
}

EXE_SUFFIX = {"windows": ".exe", "linux": ""}

# Where every release artifact is written: the installer package and the corresponding-source
# archive that must ship beside it, and nothing else, so the folder IS the release.
DIST_DIR = Path(__file__).resolve().parent / "dist"

# Where each platform's install tree is assembled before its packager runs - one directory
# per OS, never shared. A checkout can be built from both OSes (a dual-boot drive), and a
# single staging/ meant the last build wiped the other platform's tree, including the
# provenance/ the Windows corresponding-source archive is built from.
STAGING_ROOT = Path(__file__).resolve().parent / "staging"

# What a release artifact is called on each platform, and the pattern that finds the newest
# one in DIST_DIR. The suffix is also what decides how a package is opened again
# (verify_installer_notices.UNPACKERS) and which platform's inventory it is checked against
# (`package_os`), so a .deb verified on Windows is still checked as the Linux package it is.
PACKAGE_SUFFIX = {"windows": ".exe", "linux": ".deb"}
PACKAGE_GLOB = {"windows": "*-setup.exe", "linux": "kokoro-kindle-reader_*.deb"}

# Debian's name for each architecture, which is not the kernel's. Only the combinations in
# NATIVE_TRIPLE are here: a .deb for an architecture nothing builds would be a guess.
DEB_ARCH = {"x86_64": "amd64"}


def current_os():
    """`windows` or `linux`. Raises on anything else rather than guessing."""
    if sys.platform.startswith("win"):
        return "windows"
    if sys.platform.startswith("linux"):
        return "linux"
    raise RuntimeError("Unsupported platform %r for the packaging scripts." % sys.platform)


def current_arch():
    """`x86_64` or `aarch64`, normalized across the several names each goes by.

    `platform.machine()` reports the same CPU as AMD64/x86_64/x64 and as
    arm64/aarch64/ARM64 depending on the OS and the interpreter's own build, so comparing
    its raw value is a bug waiting for the first machine that spells it differently.
    """
    raw = platform.machine().lower()
    if raw in ("amd64", "x86_64", "x64"):
        return "x86_64"
    if raw in ("arm64", "aarch64"):
        return "aarch64"
    raise RuntimeError("Unsupported architecture %r for the packaging scripts."
                       % platform.machine())


def native_triple(os_name=None, arch=None):
    key = (os_name or current_os(), arch or current_arch())
    triple = NATIVE_TRIPLE.get(key)
    if triple is None:
        raise RuntimeError(
            "No Rust target triple recorded for %s/%s. This project builds %s; add a row to "
            "NATIVE_TRIPLE if that changed." % (key[0], key[1],
                                                ", ".join("%s/%s" % k for k in NATIVE_TRIPLE)))
    return triple


def exe_name(stem, os_name=None):
    """`kokoro-host` -> `kokoro-host.exe` on Windows, unchanged on Linux."""
    return stem + EXE_SUFFIX[os_name or current_os()]


def package_suffix(os_name=None):
    os_name = os_name or current_os()
    suffix = PACKAGE_SUFFIX.get(os_name)
    if suffix is None:
        raise RuntimeError("No release package format defined for %s yet." % os_name)
    return suffix


def staging_dir(os_name=None):
    """`packaging/staging/<os>/`: the install tree for that platform's package."""
    os_name = os_name or current_os()
    if os_name not in PACKAGE_SUFFIX:
        raise RuntimeError("No release package format defined for %s yet." % os_name)
    return STAGING_ROOT / os_name


def package_glob(os_name=None):
    os_name = os_name or current_os()
    pattern = PACKAGE_GLOB.get(os_name)
    if pattern is None:
        raise RuntimeError("No release package format defined for %s yet." % os_name)
    return pattern


def package_os(package):
    """The platform a built package is FOR, read off its suffix - not the platform this
    script happens to be running on."""
    suffix = os.path.splitext(str(package))[1].lower()
    for os_name, s in PACKAGE_SUFFIX.items():
        if s == suffix:
            return os_name
    raise RuntimeError("%s is not a package format this project builds (%s)."
                       % (package, ", ".join(sorted(PACKAGE_SUFFIX.values()))))


def deb_arch(arch=None):
    arch = arch or current_arch()
    name = DEB_ARCH.get(arch)
    if name is None:
        raise RuntimeError("No Debian architecture recorded for %s; add a row to DEB_ARCH "
                           "if the Linux package is built there." % arch)
    return name


def describe():
    """One line for a build log, so an artifact says what produced it."""
    return "%s/%s (%s)" % (current_os(), current_arch(), native_triple())
