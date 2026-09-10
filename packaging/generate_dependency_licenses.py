#!/usr/bin/env python3
r"""Generate the Cargo dependency-licence notices for every crate the installer actually
ships, from THIS checkout's locked dependency graph - not a hand-maintained prose list.

THIRD_PARTY_NOTICES.md used to assert license coverage for a handful of unusual crates by
name. That list drifted from the truth (four crates it called "OR alternatives already
covered by MIT/Apache-2.0" turned out to be sole-licensed ISC/Zlib/CDLA-Permissive-2.0),
and a lockfile of 700+ transitive crates was never going to stay accurate by hand
regardless. `cargo about` reads the SAME Cargo.lock the release build just used, resolves
every crate's licence expression against the ACCEPTED list in about.toml, and renders the
full text (with that crate's own copyright holder, where known) via packaging/about.hbs.
THIRD_PARTY_NOTICES.md stays the human-readable overview; this is the notice of record for
the Cargo closure. The Rust standard library comes from the compiler sysroot rather than
this graph and is staged separately by build-installer.ps1.

`--fail` is the mechanism that satisfies "detect new licence categories when dependencies
change": a crate whose resolved expression cannot be satisfied from about.toml's `accepted`
list makes cargo-about exit non-zero, which this script (and build-installer.ps1, which
calls it) turns into a build failure - the same "fail loudly rather than ship incomplete"
shape as the ONNX Runtime notices and the OCR model digests.

One HTML file per SHIPPED (crate, target) pair - not one combined report - because the x86
artifacts (kokoro-sapi, kokoro-hook, kokoro-inject) and the x64 ones (kokoro-host,
kokoro-panel) resolve different dependency graphs for the same lockfile family, and
`cargo about` operates on one root crate at a time (there is no root workspace here - see
CLAUDE.md). kokoro-ocr and kokoro-protocol are path dependencies of kokoro-host and are
covered by ITS graph; kokoro-sapi-smoke is a dev/test tool and is not shipped, so it is not
included here.

Output is PROVISIONED, not tracked - same reasoning as native-deps/runtime/notices/ (see
fetch-deps.py): committing a generated report invites it to go stale the moment a lockfile
changes without anyone re-running this script. build-installer.ps1 calls it on every build
so the shipped notices always match what was just compiled.

Port of generate-dependency-licenses.ps1. One behaviour differs on purpose and it is a fix:
ordering is ordinal rather than locale-dependent - see `dotnet_compat.ordinal_key`.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))

from dotnet_compat import (  # noqa: E402 - needs the path above
    html_encode, ordinal_key, read_all_text, sha256_text, write_all_text,
)
import source_notices  # noqa: E402

# (crate directory name, target triple it's actually built for - see packaging/README.md
# and CLAUDE.md's bitness invariants).
TARGETS = [
    ("kokoro-host", "x86_64-pc-windows-msvc"),
    ("kokoro-panel", "x86_64-pc-windows-msvc"),
    ("kokoro-sapi", "i686-pc-windows-msvc"),
    ("kokoro-hook", "i686-pc-windows-msvc"),
    ("kokoro-inject", "i686-pc-windows-msvc"),
]

INTRO = ('Licence files and copyright/licence comments copied verbatim from the dependency '
         'packages resolved by Cargo.lock. Source-comment excerpts are labelled separately; '
         'their hashes cover the excerpt, not the whole source file. Additional embedded '
         'terms are pinned by dependency version, source path and complete notice hash.')

# Names that are a licence/notice file at the package root. The subdirectories below are
# NOT filtered by this - everything under licenses/ is a notice by virtue of being there.
LICENSE_FILE = re.compile(
    r"^(LICENSES?|LICENCES?|COPYINGS?|NOTICES?|COPYRIGHTS?|AUTHORS?|CONTRIBUTORS?)(?:$|[._-].*)",
    re.IGNORECASE)

# Only leading ordinary comments, not rustdoc examples (`//!`, `/*!`), doc comments (`///`,
# `/**`) or matching strings inside code. Keeps the whole comment, including multi-line
# holders and licence terms. `\Z` is Python's spelling of .NET's `\z` (absolute end).
LEADING_COMMENT = re.compile(
    r"\A\s*(?:(?://(?![!/])[^\r\n]*(?:\r?\n|\Z)|/\*(?![*!])[\s\S]*?\*/)\s*)+")
HAS_COPYRIGHT = re.compile(r"copyright|SPDX-FileCopyrightText", re.IGNORECASE)


def leading_copyright_notice(source):
    """The package's leading copyright comment, or None.

    A package can put its copyright in source headers and ship only an SPDX template as
    its LICENSE (Slint's permissive helper crates do this), so these headers are a notice
    of record, not decoration.
    """
    m = LEADING_COMMENT.match(source)
    if m and HAS_COPYRIGHT.search(m.group(0)):
        return m.group(0).rstrip()
    return None


def _subdir(package_dir, name):
    """A subdirectory by case-insensitive name, as `Test-Path` matched it on Windows.

    Done by listing rather than by `os.path.isdir` so a package shipping `LICENSES/` is
    found on Linux too - where the packaging step is headed, and where the original's
    reliance on a case-insensitive filesystem would silently have found nothing.
    """
    try:
        for entry in os.scandir(package_dir):
            if entry.is_dir() and entry.name.lower() == name:
                return entry.path
    except OSError:
        pass
    return None


def _walk_files(top):
    found = []
    for dirpath, _dirs, names in os.walk(top):
        for n in names:
            found.append(os.path.join(dirpath, n))
    return found


def _rust_sources(package_dir, is_registry):
    """Registry/git packages are finite source packages. Local path crates have targets
    and native caches, so for those scan only src/ and the root .rs files - otherwise this
    walks a multi-gigabyte target/ directory looking for copyright headers."""
    if is_registry:
        return [p for p in _walk_files(package_dir) if p.endswith(".rs")]
    out = [e.path for e in os.scandir(package_dir) if e.is_file() and e.name.endswith(".rs")]
    src = os.path.join(package_dir, "src")
    if os.path.isdir(src):
        out += [p for p in _walk_files(src) if p.endswith(".rs")]
    return out


def _relative(path, package_dir):
    """The path as the original recorded it: relative to the package, native separators."""
    return path[len(str(package_dir)):].lstrip("\\/")


def add_packaged_license_appendix(manifest, triple, report, notices, run_cargo=None):
    """Append the exact packaged licence/notice files to a cargo-about report.

    cargo-about is the licence-expression gate, but its normalized SPDX fallback text can
    contain placeholders such as "Copyright (c) <year> <owner>" even when a crate packages
    the real notice in LICENSE.md. Append those packaged files verbatim so binary
    redistribution conditions retain the actual copyright holders. The SHA-256 on every
    heading makes the appendix self-auditing and lets the installer extraction check prove
    these are harvested source files, not another hand-written summary. Each heading also
    records the rendered-text hash, and a block count detects wholly removed notices.

    `run_cargo` is the seam the offline tests use to supply fixture metadata; production
    leaves it None and the real `cargo metadata` runs.
    """
    if run_cargo is None:
        proc = subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1",
             "--filter-platform", triple, "--manifest-path", str(manifest)],
            capture_output=True)
        if proc.returncode:
            sys.stderr.write(proc.stderr.decode("utf-8", "replace"))
            raise RuntimeError("cargo metadata failed for %s (%s)" % (manifest, triple))
        metadata = json.loads(proc.stdout.decode("utf-8"))
    else:
        metadata = json.loads(run_cargo(manifest, triple))

    resolved = {n["id"] for n in metadata["resolve"]["nodes"]}
    packages = sorted(
        (p for p in metadata["packages"] if p["id"] in resolved),
        key=lambda p: (ordinal_key(p["name"]), ordinal_key(p["version"])))

    body = []
    def line(s):
        body.append(s + "\r\n")   # StringBuilder.AppendLine, on Windows

    line('<section id="packaged-license-files" data-license-appendix="1">')
    line("<h2>Exact packaged licence and notice files</h2>")
    line('<p class="intro">%s</p>' % INTRO)
    file_count = 0
    package_count = 0

    for package in packages:
        package_dir = os.path.dirname(str(package["manifest_path"]))
        embedded = source_notices.require(notices, package["name"], package["version"])

        # This one clarification uses a local copy because cargo-about mishandles its .git
        # repository suffix. Unlike remote clarifications it cannot follow .cargo_vcs_info
        # automatically, so refuse to bless a different source.
        if package["name"] == "dasp_sample":
            vcs = json.loads(read_all_text(os.path.join(package_dir, ".cargo_vcs_info.json")))
            if (package["version"] != "0.11.0" or vcs["git"].get("dirty") or
                    vcs["git"].get("sha1") != "97c3bb9b2363c0b46ac1633858bf1054fd02a980"):
                raise RuntimeError(
                    "Re-review the local dasp_sample licence copy for this dependency "
                    "version/source.")

        files = [e.path for e in os.scandir(package_dir)
                 if e.is_file() and LICENSE_FILE.match(e.name)]
        for name in ("licenses", "licences", "notices"):
            sub = _subdir(package_dir, name)
            if sub:
                files += _walk_files(sub)
        files = sorted(set(files), key=ordinal_key)

        headers = {}
        for source in _rust_sources(package_dir, bool(package.get("source"))):
            header = leading_copyright_notice(read_all_text(source))
            if not header:
                continue
            headers.setdefault(header, []).append(_relative(source, package_dir))

        if not files and not headers and not embedded:
            continue

        package_count += 1
        line("<article class=\"packaged-license\">")
        line("<h3>%s</h3>" % html_encode("%s %s" % (package["name"], package["version"])))
        authors = package.get("authors") or []
        if authors:
            line('<p class="package-authors">Package authors: %s</p>'
                 % html_encode("; ".join(authors)))

        for path in files:
            label = html_encode(_relative(path, package_dir))
            raw = open(path, "rb").read()
            import hashlib
            file_hash = hashlib.sha256(raw).hexdigest()
            text = read_all_text(path)
            # The file hash includes any encoding preamble; the text hash checks exactly
            # what a recipient can read after decoding the HTML.
            line('<h4 data-source-sha256="%s" data-text-sha256="%s">%s</h4>'
                 % (file_hash, sha256_text(text), label))
            line("<pre>%s</pre>" % html_encode(text))
            file_count += 1

        for header in sorted(headers, key=ordinal_key):
            paths = sorted((p for p in headers[header] if p), key=ordinal_key)
            label = html_encode(", ".join(paths) + " (leading comment excerpt)")
            h = sha256_text(header)
            line('<h4 data-source-sha256="%s" data-text-sha256="%s">%s</h4>' % (h, h, label))
            line("<pre>%s</pre>" % html_encode(header))
            file_count += 1

        for notice in embedded:
            text = source_notices.notice_text(notice, package_dir)
            label = html_encode("%s:%s-%s (%s; complete embedded notice)"
                                % (notice["path"], notice["first_line"],
                                   notice["last_line"], notice["license"]))
            line('<section class="source-license">')
            line('<span data-license-crate="%s" data-license-version="%s"></span>'
                 % (notice["crate"], notice["version"]))
            line('<h4 data-source-sha256="%s" data-text-sha256="%s">%s</h4>'
                 % (notice["sha256"], notice["sha256"], label))
            line("<pre>%s</pre>" % html_encode(text))
            line("</section>")
            file_count += 1

        line("</article>")

    line('<span hidden data-packaged-notice-count="%d"></span>' % file_count)
    line("</section>")

    if package_count == 0 or file_count == 0:
        raise RuntimeError("No packaged dependency licence files found for %s (%s)"
                           % (manifest, triple))

    html = read_all_text(report)
    if "</body>" not in html:
        raise RuntimeError("cargo-about report has no </body>: %s" % report)
    write_all_text(report, html.replace("</body>", "".join(body) + "</body>"))

    written = read_all_text(report)
    if 'data-license-appendix="1"' not in written or "data-source-sha256=" not in written:
        raise RuntimeError("Packaged dependency licence appendix was not written to %s" % report)
    return {"packages": package_count, "files": file_count}


def main(argv=None):
    ap = argparse.ArgumentParser(description="Generate dependency-licence notices.")
    ap.add_argument("--skip-check", action="store_true",
                    help="do not probe for cargo-about first")
    ap.add_argument("--output-dir", help="where to write the reports")
    args = ap.parse_args(argv)

    import verify_dependency_licenses

    template = HERE / "about.hbs"
    config = HERE / "about.toml"
    out_dir = Path(args.output_dir) if args.output_dir else HERE / "dependency-licenses"
    notices = source_notices.read_requirements(HERE / "source-notices.json")

    if not args.skip_check:
        try:
            probe = subprocess.run(["cargo", "about", "--version"], capture_output=True)
            ok = probe.returncode == 0
        except OSError:
            ok = False
        if not ok:
            raise SystemExit("cargo-about is not installed. Run: "
                             "cargo install cargo-about --locked --features cli")

    shutil.rmtree(out_dir, ignore_errors=True)
    out_dir.mkdir(parents=True, exist_ok=True)

    # cargo-about resolves clarification file paths relative to a registry package. Expand
    # our explicit project-local notice paths without modifying that package. Nothing else
    # in the checked-in config is changed (including the content hashes).
    effective = out_dir / "about.generated.toml"
    write_all_text(effective, read_all_text(config).replace(
        'path = "@project/', 'path = "' + str(ROOT).replace("\\", "/") + "/"))

    for crate, triple in TARGETS:
        manifest = ROOT / crate / "Cargo.toml"
        out = out_dir / ("%s.html" % crate)
        print("==> cargo about generate: %s (%s)" % (crate, triple))
        rc = subprocess.run(["cargo", "about", "generate", "--locked", "-c", str(effective),
                             "-m", str(manifest), "--target", triple, "--fail",
                             str(template), "-o", str(out)]).returncode
        if rc:
            raise SystemExit(
                "cargo about generate failed for %s (%s). If this is a newly-added "
                "dependency under a licence not yet in about.toml's `accepted` list, that "
                "is this script doing its job - add the licence (and, if it's mandatory "
                "rather than an OR alternative already covered, make sure its notice "
                "actually ships) rather than widening `accepted` to make the error go away."
                % (crate, triple))
        added = add_packaged_license_appendix(manifest, triple, out, notices)
        verify_dependency_licenses.verify(out, config, HERE / "source-notices.json")
        print("    appended exact packaged notices: %d file(s)/excerpt(s) from %d package(s)"
              % (added["files"], added["packages"]))

    print("==> Dependency licence notices generated in %s" % out_dir)
    for f in sorted(out_dir.glob("*.html"), key=lambda p: ordinal_key(p.name)):
        print("    %s  (%s KB)" % (f.name, format(round(f.stat().st_size / 1024), ",")))


if __name__ == "__main__":
    main()
