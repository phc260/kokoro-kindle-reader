"""Exact additional obligations embedded in source, outside the package's declared licence
expression or leading Rust comments.

Line ranges include the full licence, short notice and upstream attribution; hashes cover
UTF-8 joined with LF, no final LF. A new dependency version or changed excerpt requires
reviewing the upstream notice - which is what `require` refusing an unreviewed version is
for: a dependency bump must not silently carry last version's notice forward.

Port of source-notices.ps1, which was dot-sourced by the generator and the verifier.
"""

import os
import re

from dotnet_compat import read_all_lines, sha256_text

# PowerShell's -match and -notin are case-INSENSITIVE unless spelled -cmatch/-cnotin, so
# the original accepted `ISC`/`isc` alike and an uppercase digest. Kept as it was: a port
# that quietly tightens validation is a port that rejects an input the reviewed
# source-notices.json is entitled to contain. The digest COMPARISON below is
# case-sensitive, exactly as the original's -cne was.
_CRATE = re.compile(r"^[A-Za-z0-9_-]+$", re.IGNORECASE)
_VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?$", re.IGNORECASE)
_PATH = re.compile(r"^[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)+$", re.IGNORECASE)
_SHA256 = re.compile(r"^[0-9a-f]{64}$", re.IGNORECASE)
_LICENSES = ("W3C-20150513", "ISC")
_INT32_MAX = 2147483647


def _is_int(v):
    # bool is an int in Python and is not one in JSON; reject it explicitly.
    return isinstance(v, int) and not isinstance(v, bool)


def read_requirements(entries_or_path):
    """Validate and return the inventory. Accepts a path or an already-parsed list."""
    import json

    if isinstance(entries_or_path, (str, os.PathLike)):
        from dotnet_compat import read_all_text
        entries = json.loads(read_all_text(entries_or_path))
    else:
        entries = entries_or_path
    if isinstance(entries, dict):
        entries = [entries]
    if not entries:
        raise ValueError("The embedded-source notice inventory is empty.")

    seen = set()
    for e in entries:
        if (not _CRATE.match(str(e.get("crate", ""))) or
                not _VERSION.match(str(e.get("version", ""))) or
                not _PATH.match(str(e.get("path", ""))) or
                ".." in str(e.get("path", "")).split("/") or
                "." in str(e.get("path", "")).split("/") or
                not _is_int(e.get("first_line")) or
                e["first_line"] < 1 or e["first_line"] > _INT32_MAX or
                not _is_int(e.get("last_line")) or
                e["last_line"] < e["first_line"] or e["last_line"] > _INT32_MAX or
                str(e.get("license", "")).upper() not in
                tuple(x.upper() for x in _LICENSES) or
                not _SHA256.match(str(e.get("sha256", "")))):
            raise ValueError("Malformed or unreviewed embedded-source notice requirement.")
        key = "%s %s %s" % (e["crate"], e["version"], e["path"])
        if key in seen:
            raise ValueError("Duplicate source notice: %s" % key)
        seen.add(key)
    return list(entries)


def require(entries, crate, version):
    """The reviewed notices for one exact (crate, version).

    A crate that HAS entries but none for this version is an error rather than an empty
    list: it means the dependency moved and nobody re-read the upstream notice, and
    returning nothing would let the bump ship with the obligation silently dropped.
    """
    for_crate = [e for e in entries if e["crate"] == crate]
    for_version = [e for e in for_crate if e["version"] == version]
    if for_crate and not for_version:
        raise ValueError(
            "Re-review embedded source notices for %s %s before distributing it."
            % (crate, version))
    return for_version


def notice_text(entry, package_dir):
    """The pinned line range, verified against its digest before it is used."""
    path = os.path.join(package_dir, entry["path"])
    lines = read_all_lines(path)
    if len(lines) < entry["last_line"]:
        raise ValueError("Source notice was truncated: %s" % path)
    text = "\n".join(lines[entry["first_line"] - 1:entry["last_line"]])
    if sha256_text(text) != entry["sha256"]:
        raise ValueError(
            "Embedded source notice changed in %s; review it before updating "
            "source-notices.json." % path)
    return text
