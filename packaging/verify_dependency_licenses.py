#!/usr/bin/env python3
"""Verify the actual rendered clarification texts, not cargo-about's exit status.

cargo-about 0.9.1 only WARNS and falls back to canonical SPDX text when a pinned upstream
fetch or hash fails, so a green exit proves nothing about what was rendered. This checks
every ordinary appendix text and its expected block count too, not only the special
clarified/embedded notices: the old presence-only check allowed a normal LICENSE or source
header to be replaced or removed while the special clarifications still passed.

Runs against extracted reports as well as generated ones; no builds, no network.

Port of verify-dependency-licenses.ps1.
"""

import argparse
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from dotnet_compat import html_decode, read_all_text, sha256_text  # noqa: E402
import source_notices  # noqa: E402

# Deliberately limited to our configuration's unquoted crate keys and one-line checksum
# fields. Reject schema drift instead of silently skipping a requirement.
DECLARATION = re.compile(r"^[ \t]*\[\[[^\r\n]*\.clarify\.(?:git|files)\]\][ \t]*$", re.M)
BLOCK = re.compile(
    r"^\[\[(?P<crate>[A-Za-z0-9_-]+)\.clarify\.(?:git|files)\]\]\r?\n(?P<fields>.*?)(?=^\[|\Z)",
    re.M | re.S)
CHECKSUM = re.compile(r'^checksum = "(?P<hash>[0-9a-f]{64})"[ \t]*$', re.M)

COUNT_MARKER = re.compile(r'data-packaged-notice-count="(?P<count>[0-9]+)"')
APPENDIX_TEXT = re.compile(
    r'<h4 data-source-sha256="[0-9a-f]{64}" data-text-sha256="(?P<hash>[0-9a-f]{64})">'
    r"(?P<label>.*?)</h4>\s*<pre>(?P<text>.*?)</pre>", re.S)
HEADING = re.compile(r"<h4 data-source-sha256=")
INVENTORY = re.compile(
    r'data-report-crate="(?P<crate>[A-Za-z0-9_-]+)" data-report-version="(?P<version>[^"]+)"')
SECTION = re.compile(
    r'<section class="(?:resolved-license|source-license)">(?P<body>.*?)</section>', re.S)
PRE = re.compile(r"<pre>(?P<text>.*?)</pre>", re.S)
LICENSE_CRATE = re.compile(
    r'data-license-crate="(?P<crate>[A-Za-z0-9_-]+)" data-license-version="(?P<version>[^"]+)"')


def verify(report, config=None, source_notices_path=None):
    """Raise on any failure; return the (block count, clarified-text count) on success."""
    config = Path(config) if config else HERE / "about.toml"
    notices_path = Path(source_notices_path) if source_notices_path else HERE / "source-notices.json"
    embedded_requirements = source_notices.read_requirements(notices_path)

    toml = read_all_text(config)
    declarations = DECLARATION.findall(toml)
    blocks = list(BLOCK.finditer(toml))
    if not declarations or len(declarations) != len(blocks):
        raise ValueError("about.toml clarification files must use the checked one-line "
                         "table schema.")
    required = {}
    for block in blocks:
        crate = block.group("crate")
        checksums = CHECKSUM.findall(block.group("fields"))
        if len(checksums) != 1:
            raise ValueError("Expected one SHA-256 in %s clarification." % crate)
        required.setdefault(crate, []).append(checksums[0])

    html = read_all_text(report)

    count_markers = COUNT_MARKER.findall(html)
    if len(count_markers) != 1 or int(count_markers[0]) < 1:
        raise ValueError("Missing or malformed packaged-notice inventory in %s" % report)
    expected_count = int(count_markers[0])

    appendix = list(APPENDIX_TEXT.finditer(html))
    heading_count = len(HEADING.findall(html))
    if len(appendix) != expected_count or heading_count != expected_count:
        raise ValueError("Missing or malformed packaged notice blocks in %s (expected %d)."
                         % (report, expected_count))
    for notice in appendix:
        text = html_decode(notice.group("text"))
        if sha256_text(text) != notice.group("hash"):
            raise ValueError("Packaged notice text changed in %s (%s)."
                             % (report, html_decode(notice.group("label"))))

    inventory = list(INVENTORY.finditer(html))
    if not inventory:
        raise ValueError("No dependency inventory in %s" % report)
    sections = list(SECTION.finditer(html))
    if not sections:
        raise ValueError("No machine-checkable licence sections in %s" % report)

    rendered = {}
    for section in sections:
        body = section.group("body")
        texts = PRE.findall(body)
        crates = list(LICENSE_CRATE.finditer(body))
        if len(texts) != 1 or not crates:
            raise ValueError("Malformed licence section in %s" % report)
        digest = sha256_text(html_decode(texts[0]))
        for m in crates:
            # Separate versions: one version's correct notice cannot bless another's
            # failed clarification merely because their crate names are the same.
            rendered.setdefault("%s %s" % (m.group("crate"), m.group("version")),
                                set()).add(digest)

    checked = 0
    for entry in inventory:
        crate, version = entry.group("crate"), entry.group("version")
        key = "%s %s" % (crate, version)
        embedded = source_notices.require(embedded_requirements, crate, version)
        hashes = list(required.get(crate, [])) + [e["sha256"] for e in embedded]
        for h in hashes:
            if not h:
                continue
            if h not in rendered.get(key, ()):
                raise ValueError(
                    "Missing exact clarified or embedded licence text for %s in %s "
                    "(SHA-256 %s). Check upstream retrieval, about.toml and "
                    "source-notices.json; canonical fallback is not sufficient."
                    % (key, report, h))
            checked += 1

    print("==> OK: %d packaged notice(s), %d clarified/embedded text(s) verified in %s"
          % (expected_count, checked, report))
    return expected_count, checked


def main(argv=None):
    ap = argparse.ArgumentParser(description="Verify a dependency-licence report.")
    ap.add_argument("--report", required=True)
    ap.add_argument("--config")
    ap.add_argument("--source-notices")
    args = ap.parse_args(argv)
    try:
        verify(args.report, args.config, args.source_notices)
    except (ValueError, RuntimeError) as e:
        raise SystemExit(str(e))


if __name__ == "__main__":
    main()
