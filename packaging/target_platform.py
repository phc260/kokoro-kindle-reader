"""What platform is this, and what does the build call things there.

The packaging scripts are the platform-agnostic layer: the LOGIC (what must ship, what is
verified, what is staged) is one implementation, and the places where an OS genuinely
differs are these tables. That is the same shape `native-deps/fetch-deps.py` uses for the
ONNX Runtime wheels - platform differences belong somewhere they can be read side by side,
not spread through the code as `if windows` at every use.

**Only combinations that really exist here are populated.** An unsupported OS or
architecture raises rather than falling back to a guess: a packaging script that silently
assumes Windows on an unknown platform produces an artifact nobody checked. Adding the
Linux release is adding rows here, and the callers should not need to change.
"""

import platform
import sys

# Rust target triples this project actually builds, per (os, arch). The x86 artifacts
# (kokoro-sapi/hook/inject) are a separate axis - Kindle is a 32-bit Windows process, so
# they are Windows-only by nature and are not in this table.
NATIVE_TRIPLE = {
    ("windows", "x86_64"): "x86_64-pc-windows-msvc",
    ("linux", "x86_64"): "x86_64-unknown-linux-gnu",
}

EXE_SUFFIX = {"windows": ".exe", "linux": ""}

# What a release artifact is called on each platform. Linux is the port plan's packaging
# step; there is no .deb yet, which is why "linux" is absent rather than guessed at.
PACKAGE_SUFFIX = {"windows": ".exe"}


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


def describe():
    """One line for a build log, so an artifact says what produced it."""
    return "%s/%s (%s)" % (current_os(), current_arch(), native_triple())
