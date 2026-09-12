#!/usr/bin/env python3
r"""Verify the SHA-256s recorded in packaging/components.toml for the in-repo shipped assets.

cargo-about covers the Cargo closure and the extraction test proves the notice tree ships;
this closes the gap components.toml claimed but nothing enforced - a Material Symbol SVG
(compiled into kokoro-panel.exe) changing without its recorded digest being updated, leaving
the "authoritative inventory" silently stale while the build stays green.

Only assets that are BOTH checked into the repo AND pinned by sha256 in components.toml are
checkable here - the five kokoro-panel/ui/*.svg. The ORT libraries and OCR models are
provisioned or downloaded (pinned by the wheel and ocr-manifest.json, not by us), and
icon.ico is an LFS asset with no digest in the manifest.

    python3 packaging/verify_component_hashes.py

Port of verify-component-hashes.ps1.
"""

import hashlib
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))

UI_DIR = Path("kokoro-panel") / "ui"

# A full TOML parser is not a dependency worth adding for two quoted scalars per block, and
# `tomllib` is 3.11+ while these scripts must run on whatever Python 3 is present. Splitting
# on the block header and matching the two keys per block is exact for this shape - and any
# drift from it is caught by the completeness check below, not waved through.
_BLOCK_SPLIT = re.compile(r"^\[\[component\]\]", re.M)
_NAME = re.compile(r'^[ \t]*name[ \t]*=[ \t]*"([^"]+)"', re.M)
_SHA256 = re.compile(r'^[ \t]*sha256[ \t]*=[ \t]*"([0-9a-fA-F]{64})"', re.M)
_SVG_NAME = re.compile(r"^([A-Za-z0-9._-]+\.svg)\b")
_IMAGE_URL = re.compile(r'@image-url\(\s*"([^"]+\.svg)"')


def verify():
    from dotnet_compat import ordinal_key, read_all_text

    toml = read_all_text(HERE / "components.toml")
    checked = 0
    errors = []
    manifest_svgs = []

    for block in _BLOCK_SPLIT.split(toml):
        name_m = _NAME.search(block)
        if not name_m:
            continue
        name = name_m.group(1)
        # Only the SVG assets map to an in-repo file with a pinned digest.
        svg_m = _SVG_NAME.match(name)
        if not svg_m:
            continue
        svg = svg_m.group(1)
        manifest_svgs.append(svg)

        sha_m = _SHA256.search(block)
        if not sha_m:
            errors.append("component '%s' (%s) has no sha256 in components.toml" % (name, svg))
            continue
        expected = sha_m.group(1).lower()
        path = ROOT / UI_DIR / svg
        if not path.is_file():
            errors.append("component '%s' references missing file %s"
                          % (name, (UI_DIR / svg).as_posix()))
            continue
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual != expected:
            errors.append("%s sha256 mismatch: components.toml has %s, file is %s - update "
                          "components.toml (and re-check the notice) if the change is "
                          "intended." % ((UI_DIR / svg).as_posix(), expected, actual))
        else:
            checked += 1

    # COMPLETENESS: a per-block hash check only sees the blocks that still exist, so deleting
    # a component block - or embedding a new SVG without one - would pass silently. The
    # authoritative set of SVGs that actually ship is whatever panel.slint compiles in via
    # @image-url(...). The manifest's SVG set must equal it exactly: no missing block, no
    # orphan entry.
    slint = read_all_text(ROOT / UI_DIR / "panel.slint")
    referenced = sorted({Path(m.group(1)).name for m in _IMAGE_URL.finditer(slint)},
                        key=ordinal_key)
    in_manifest = sorted(set(manifest_svgs), key=ordinal_key)
    for m in referenced:
        if m not in in_manifest:
            errors.append("panel.slint compiles in '%s' but components.toml has no component "
                          "block for it" % m)
    for o in in_manifest:
        if o not in referenced:
            errors.append("components.toml inventories '%s' but panel.slint no longer "
                          "references it (stale block)" % o)

    if checked == 0 and not errors:
        raise ValueError("No SVG components found in components.toml - the parser or the "
                         "manifest changed shape.")
    if errors:
        sys.stdout.flush()
        print("MISMATCH/MISSING:\n  %s" % "\n  ".join(errors), file=sys.stderr)
        raise ValueError("components.toml component-hash verification FAILED (see above).")

    print("==> OK: %d component SHA-256(s) match, and the manifest's SVG set equals "
          "panel.slint's (%d glyphs)." % (checked, len(referenced)))
    return checked, len(referenced)


if __name__ == "__main__":
    try:
        verify()
    except ValueError as e:
        raise SystemExit(str(e))
