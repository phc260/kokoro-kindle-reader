#!/usr/bin/env python3
r"""Verify the checked-in licence texts byte-for-byte after normalizing line endings.

Presence/non-empty checks are not enough: a truncated text, a notice copied from the wrong
upstream revision, or a missing copyright line still produces an installer that looks
complete. license-texts.sha256 pins the reviewed text while remaining stable across Git's
CRLF conversion on Windows.

With no arguments, also require the manifest to inventory every checked-in file under
licenses/, plus root LICENSE, THIRD_PARTY_NOTICES.md and legal.html. --allow-additional is
used against an extracted installer, whose licenses/ directory also contains provisioned and
generated notice trees.

`verify-license-texts.ps1` is a thin harness over this.
"""

import argparse
import hashlib
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent
MANIFEST = HERE / "license-texts.sha256"

# Two spaces between digest and path, as sha256sum writes them.
LINE_RE = re.compile(r"^([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._/-]*)$")

# Always expected at the root, whether or not licenses/ exists.
ROOT_TEXTS = ("LICENSE", "THIRD_PARTY_NOTICES.md", "legal.html")


def normalized_sha256(data):
    """Hash the decoded text with CRLF/CR folded to LF, so Git's checkout policy cannot
    change a reviewed digest."""
    return hashlib.sha256(
        data.replace(b"\r\n", b"\n").replace(b"\r", b"\n")
    ).hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", default=None,
                    help="tree to verify (default: the repository root)")
    ap.add_argument("--allow-additional", action="store_true",
                    help="do not require the manifest to inventory the tree exactly")
    args = ap.parse_args()

    root = Path(args.root).resolve() if args.root else REPO_ROOT
    entries = {}
    errors = []

    for line in MANIFEST.read_text(encoding="utf-8").splitlines():
        trimmed = line.strip()
        if not trimmed or trimmed.startswith("#"):
            continue
        m = LINE_RE.match(trimmed)
        if not m:
            errors.append("Malformed checksum line: %s" % line)
            continue
        expected, relative = m.group(1), m.group(2)

        segments = relative.split("/")
        if Path(relative).is_absolute() or any(s in (".", "..", "") for s in segments):
            errors.append("Unsafe checksum path: %s" % relative)
            continue
        if relative in entries:
            errors.append("Duplicate checksum path: %s" % relative)
            continue
        entries[relative] = expected

        path = (root / relative).resolve()
        # Refuse anything that resolves outside the tree being verified, however it got
        # there -- the segment check above is not enough once symlinks are possible.
        try:
            path.relative_to(root)
        except ValueError:
            errors.append("Checksum path escapes the root: %s" % relative)
            continue
        if not path.is_file():
            errors.append("Missing checked-in licence text: %s" % relative)
            continue

        data = path.read_bytes()
        if not data:
            errors.append("Empty checked-in licence text: %s" % relative)
            continue
        if data.startswith(b"\xef\xbb\xbf"):
            errors.append(
                "UTF-8 BOM is not allowed in checked-in licence text: %s" % relative)
            continue
        try:
            data.decode("utf-8")
        except UnicodeDecodeError:
            errors.append("Checked-in licence text is not valid UTF-8: %s" % relative)
            continue

        actual = normalized_sha256(data)
        if actual != expected:
            errors.append(
                "%s checksum mismatch: expected %s, got %s - verify the complete "
                "replacement against its upstream revision before updating "
                "packaging/license-texts.sha256." % (relative, expected, actual))

    if not entries:
        errors.append("license-texts.sha256 contains no entries.")

    if not args.allow_additional:
        actual_paths = list(ROOT_TEXTS)
        license_dir = root / "licenses"
        if license_dir.is_dir():
            actual_paths += [
                f.relative_to(root).as_posix()
                for f in license_dir.rglob("*") if f.is_file()
            ]
        for relative in sorted(set(actual_paths)):
            if relative not in entries:
                errors.append(
                    "Checked-in licence text is not pinned in license-texts.sha256: %s"
                    % relative)
        for relative in entries:
            if relative not in actual_paths:
                errors.append(
                    "license-texts.sha256 has no matching checked-in file: %s" % relative)

    if errors:
        print("LICENCE TEXT ERROR(S):\n  " + "\n  ".join(errors))
        print("Checked-in licence-text verification FAILED (see above).", file=sys.stderr)
        raise SystemExit(1)
    print("==> OK: %d checked-in licence text(s) match reviewed SHA-256s." % len(entries))


if __name__ == "__main__":
    main()
