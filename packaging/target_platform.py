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

# Where each build family's install tree is assembled before its packager runs - one
# directory per family (`build_family`), never shared. A checkout can be built from several
# systems (a dual-boot drive, a distrobox), and a single staging/ meant the last build wiped
# the other's tree, including the provenance/ its corresponding-source archive is built from.
STAGING_ROOT = Path(__file__).resolve().parent / "staging"

# Every package format this project builds: the platform whose staged tree it carries, the
# suffix a built one is recognized by, and the pattern that finds the newest in DIST_DIR.
# The suffix is what decides how a package is opened again (verify_installer_notices.UNPACKERS)
# and which platform's inventory it is checked against (`package_os`), so a .deb verified on
# Windows is still checked as the Linux package it is.
#
# Two formats can carry ONE platform: the .deb and the Arch package wrap the same staged
# Linux tree, so everything keyed by platform - components.toml's `platforms`, the
# dependency reports, the omitted legal page - applies to both, and only the wrapping differs.
#
# `source_zip` names the corresponding-source archive that must travel with a built package
# (build_corresponding_source.py), per FORMAT: the .deb and the Arch package are separate
# builds with their own provenance, so each gets its own archive, and the names keep them
# apart wherever both land - one release, or one dist/. The Windows name predates the others
# and is unchanged.
#
# `family` is the build family that builds the format - see `build_family`. It names every
# directory that build writes, and it is NOT the platform: the platform decides what ships,
# the family decides whose compiler and glibc produced it.
PACKAGE_FORMATS = {
    "nsis": {"os": "windows", "family": "windows", "suffix": ".exe", "glob": "*-setup.exe",
             "source_zip": "corresponding-source-%s.zip"},
    "deb": {"os": "linux", "family": "debian", "suffix": ".deb",
            "glob": "kokoro-kindle-reader_*.deb",
            "source_zip": "corresponding-source-%s-debian.zip"},
    "pacman": {"os": "linux", "family": "arch", "suffix": ".pkg.tar.xz",
               "glob": "kokoro-kindle-reader-*.pkg.tar.xz",
               "source_zip": "corresponding-source-%s-arch.zip"},
}
# The platforms some package carries: what components.toml's `platforms` may name.
PACKAGE_OSES = sorted({f["os"] for f in PACKAGE_FORMATS.values()})
# The build families some package is built by: the ones that get a staging tree.
PACKAGE_FAMILIES = sorted({f["family"] for f in PACKAGE_FORMATS.values()})

# Which format a Linux machine builds, by the distribution family os-release puts it in: its
# ID first, then each ID_LIKE entry (Ubuntu is `ubuntu` like `debian`; Manjaro is `manjaro`
# like `arch`). A distribution in neither family raises rather than guessing - a package for
# a package manager the machine may not have is an artifact nobody checked. The keys are also
# the Linux build families (`build_family`), and kokoro-host/build.rs keeps a copy of them,
# since a build script cannot ask Python.
LINUX_FAMILY_FORMAT = {"debian": "deb", "arch": "pacman"}
# Where os-release lives: /etc first, /usr/lib as the specification's fallback.
OS_RELEASE = [Path("/etc/os-release"), Path("/usr/lib/os-release")]

# Debian's name for each architecture, which is not the kernel's. Only the combinations in
# NATIVE_TRIPLE are here: a .deb for an architecture nothing builds would be a guess.
DEB_ARCH = {"x86_64": "amd64"}
# Arch Linux's, which is the kernel's - recorded anyway, for the same reason.
PACMAN_ARCH = {"x86_64": "x86_64"}


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


def distro_ids(paths=None):
    """os-release's ID followed by its ID_LIKE entries, in that order: the most specific
    name first, then the families it says it belongs to."""
    for path in paths or OS_RELEASE:
        if path.is_file():
            fields = {}
            for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
                key, sep, value = line.partition("=")
                if sep and not key.lstrip().startswith("#"):
                    fields[key.strip()] = value.strip().strip("\"'")
            return [i for i in [fields.get("ID", "")] + fields.get("ID_LIKE", "").split() if i]
    raise RuntimeError("No os-release file (%s): cannot tell which Linux distribution this "
                       "is." % ", ".join(str(p) for p in paths or OS_RELEASE))


def package_format(os_name=None, ids=None):
    """The package format THIS machine builds: NSIS on Windows; on Linux, the one for the
    distribution's family. `ids` is for tests; it is read from os-release otherwise."""
    os_name = os_name or current_os()
    if os_name == "windows":
        return "nsis"
    if os_name == "linux":
        ids = distro_ids() if ids is None else ids
        for i in ids:
            if i in LINUX_FAMILY_FORMAT:
                return LINUX_FAMILY_FORMAT[i]
        raise RuntimeError("No package format for this Linux distribution (os-release ID and "
                           "ID_LIKE: %s). Packages are built for the %s families; add a row "
                           "to LINUX_FAMILY_FORMAT rather than building one for another's."
                           % (", ".join(ids) or "none",
                              " and ".join(sorted(LINUX_FAMILY_FORMAT))))
    raise RuntimeError("No release package format defined for %s yet." % os_name)


def build_family(os_name=None, ids=None):
    """Whose build output this machine writes: `windows`, or on Linux the distribution family
    the package format is chosen by (`debian`, `arch`). It names native-deps/<family>/,
    espeak's build-<family>/, the package build's cargo target dir and staging/<family>/.

    The platform is not enough on Linux. Two distributions can build one checkout - a dual
    boot, a distrobox, a container with the tree mounted - and nothing in a shared build
    directory says whose compiler and glibc produced it: the provision marker records the
    recipe, and cargo calls the other's binary fresh (same triple, same rustc). So the other
    distribution's espeak and host would be packaged as if built here.

    A distribution no package is built for still gets a tree of its own, under its os-release
    ID, so the host can be developed there; only packaging needs a known family, and
    `package_format` refuses the rest. `ids` is for tests.
    """
    os_name = os_name or current_os()
    if os_name == "windows":
        return "windows"
    if os_name == "linux":
        ids = distro_ids() if ids is None else ids
        for i in ids:
            if i in LINUX_FAMILY_FORMAT:
                return i
        if ids:
            return ids[0]
        raise RuntimeError("os-release names no ID: cannot tell which build tree this "
                           "machine writes.")
    raise RuntimeError("No build family defined for %s yet." % os_name)


def package_family(fmt=None):
    """The build family that builds a package format; this machine's own when none is named."""
    return PACKAGE_FORMATS[fmt or package_format()]["family"]


def staging_dir(family=None):
    """`packaging/staging/<family>/`: the install tree one build family assembles for its
    package. Per family, not per platform: the .deb and the Arch package carry the same
    Linux tree, but each is staged from its own distribution's build."""
    family = family or package_family()
    if family not in PACKAGE_FAMILIES:
        raise RuntimeError("No package is built by the %r family (%s)."
                           % (family, ", ".join(PACKAGE_FAMILIES)))
    return STAGING_ROOT / family


def package_glob(fmt=None):
    """The DIST_DIR pattern for a format; this machine's own when none is named. A format,
    not a platform: two formats carry the Linux tree."""
    fmt = fmt or package_format()
    if fmt not in PACKAGE_FORMATS:
        raise RuntimeError("%r is not a package format (%s); a platform name is not one."
                           % (fmt, ", ".join(sorted(PACKAGE_FORMATS))))
    return PACKAGE_FORMATS[fmt]["glob"]


def format_of(package):
    """The format a built package is IN, read off its suffix - not the format this machine
    happens to build."""
    name = str(package).lower()
    for fmt, f in PACKAGE_FORMATS.items():
        if name.endswith(f["suffix"]):
            return fmt
    raise RuntimeError("%s is not a package format this project builds (%s)."
                       % (package, ", ".join(sorted(f["suffix"] for f in PACKAGE_FORMATS.values()))))


def package_os(package):
    """The platform a built package is FOR - which inventory it is checked against."""
    return PACKAGE_FORMATS[format_of(package)]["os"]


def deb_arch(arch=None):
    arch = arch or current_arch()
    name = DEB_ARCH.get(arch)
    if name is None:
        raise RuntimeError("No Debian architecture recorded for %s; add a row to DEB_ARCH "
                           "if the Linux package is built there." % arch)
    return name


def pacman_arch(arch=None):
    arch = arch or current_arch()
    name = PACMAN_ARCH.get(arch)
    if name is None:
        raise RuntimeError("No Arch Linux architecture recorded for %s; add a row to "
                           "PACMAN_ARCH if the Arch package is built there." % arch)
    return name


def describe():
    """One line for a build log, so an artifact says what produced it."""
    return "%s/%s (%s)" % (current_os(), current_arch(), native_triple())
