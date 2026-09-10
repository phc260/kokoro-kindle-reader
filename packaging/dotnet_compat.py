"""The .NET behaviours the licence-notice scripts depend on, reproduced exactly.

These scripts were PowerShell, and their output is hashed: `verify-dependency-licenses`
re-hashes every rendered block, and CI compares the extracted installer's reports against
the generated ones. So a port cannot approximate `WebUtility.HtmlEncode` or
`File.ReadAllText` - a single differing entity or a mishandled BOM changes a hash and
fails a check that is supposed to mean "the notice was altered".

Every function here was pinned against the real .NET Framework 4.x implementation under
Windows PowerShell 5.1 rather than written from the documentation.

NOT reproduced, deliberately: `Sort-Object`. See `ordinal_key` at the bottom.
"""

import hashlib
import re
from html.entities import entitydefs

# WebUtility.HtmlEncode escapes exactly these five, and `'` as a NUMERIC reference rather
# than `&apos;` (which is not in the HTML 4 entity set).
_HTML_ESCAPES = {
    "<": "&lt;",
    ">": "&gt;",
    "&": "&amp;",
    '"': "&quot;",
    "'": "&#39;",
}


def html_encode(text):
    """`System.Net.WebUtility.HtmlEncode`.

    Measured, not assumed: beyond the five characters above it escapes code points in
    **[U+00A0, U+00FF]** as `&#N;` and passes everything else through literally. U+007F,
    U+0080 and U+009F stay literal; so do U+0100, U+2014 and astral characters - an emoji
    comes back as its own surrogate pair, not a numeric reference. A "escape all
    non-ASCII" implementation would corrupt every licence text containing an em dash or a
    non-Latin-1 name, and the corruption would look like tampering to the verifier.
    """
    out = []
    for ch in text:
        esc = _HTML_ESCAPES.get(ch)
        if esc is not None:
            out.append(esc)
        elif 0xA0 <= ord(ch) <= 0xFF:
            out.append("&#%d;" % ord(ch))
        else:
            out.append(ch)
    return "".join(out)


# A named reference, or a decimal/hex numeric one - each REQUIRING the semicolon.
#
# Deliberately not `html.unescape`: that follows the HTML5 parsing rules, which decode a
# long list of named references written WITHOUT a trailing semicolon. A licence text
# containing the literal `&copy` (which html_encode wrote as `&amp;copy`) would come back
# as a copyright sign, and its hash would then not match the text that was encoded.
#
# The named set is HTML 4.01 (`html.entities.entitydefs`), which is the table .NET's
# HtmlDecode uses. The numeric forms are what actually matter here and are easy to get
# wrong: this decodes reports rendered by cargo-about's Handlebars template, and
# Handlebars escapes `'` as `&#x27;`, backtick as `&#x60;` and `=` as `&#x3D;` - all HEX.
# A decimal-only decoder silently leaves those literal, so every clarified licence text
# containing an apostrophe hashes differently and the verifier reports it as tampering.
_HTML_UNESCAPE = re.compile(r"&(#[0-9]{1,7}|#[xX][0-9a-fA-F]{1,6}|[A-Za-z][A-Za-z0-9]{1,31});")


def html_decode(text):
    """`System.Net.WebUtility.HtmlDecode`."""

    def sub(m):
        ref = m.group(1)
        if ref[0] == "#":
            try:
                cp = int(ref[2:], 16) if ref[1] in "xX" else int(ref[1:])
            except ValueError:
                return m.group(0)
            # .NET leaves an out-of-range or surrogate reference as written rather than
            # producing an unpaired surrogate.
            if cp > 0x10FFFF or 0xD800 <= cp <= 0xDFFF:
                return m.group(0)
            return chr(cp)
        named = entitydefs.get(ref)
        return named if named is not None else m.group(0)

    return _HTML_UNESCAPE.sub(sub, text)


def sha256_text(text):
    """SHA-256 over the UTF-8 bytes of decoded text, lowercase hex.

    This is the `data-text-sha256` on every appendix heading. It covers what a recipient
    can actually read after decoding the HTML, which is why it differs from the file hash
    beside it: `read_all_text` strips a BOM, so a BOM-bearing LICENSE hashes differently
    as bytes than as text. Both are recorded on purpose.
    """
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def read_all_text(path):
    """`System.IO.File.ReadAllText`: detect and strip a BOM, else UTF-8; never throw.

    .NET substitutes U+FFFD for undecodable bytes instead of raising, and dependency
    packages do contain the odd Latin-1 licence file. Python's default would raise, which
    would turn one mis-encoded upstream notice into a failed release build.
    """
    raw = open(path, "rb").read()
    for bom, enc in ((b"\xef\xbb\xbf", "utf-8"), (b"\xff\xfe\x00\x00", "utf-32-le"),
                     (b"\x00\x00\xfe\xff", "utf-32-be"), (b"\xff\xfe", "utf-16-le"),
                     (b"\xfe\xff", "utf-16-be")):
        if raw.startswith(bom):
            return raw[len(bom):].decode(enc, errors="replace")
    return raw.decode("utf-8", errors="replace")


# .NET splits lines on exactly these three; Python's str.splitlines() also breaks on
# \v, \f, \x1c-\x1e, \x85, U+2028 and U+2029. A form feed is ordinary in old licence
# headers, and splitting on one would shift every line number in source-notices.json.
_NET_LINES = re.compile(r"\r\n|\r|\n")


def read_all_lines(path):
    """`System.IO.File.ReadAllLines`: split on CRLF/CR/LF only, no trailing empty line."""
    text = read_all_text(path)
    lines = _NET_LINES.split(text)
    if lines and lines[-1] == "":
        lines.pop()
    return lines


def write_all_text(path, text):
    """`File.WriteAllText(path, text, UTF8Encoding($false))`: UTF-8, no BOM, no newline
    translation - the caller's `\\r\\n` are written as they are."""
    with open(path, "wb") as f:
        f.write(text.encode("utf-8"))


def ordinal_key(s):
    """Sort key for every list these scripts order. **This is the one intentional
    behaviour change in the port, and it is a fix.**

    The PowerShell originals used `Sort-Object`, which is culture-aware: it compares with
    the *current culture* using .NET word-sort. Two consequences, both measured here
    under PowerShell 5.1:

      - Word-sort treats `-` as ignorable at the primary level, so `autocfg` sorts BEFORE
        `auto-launch` (compared as `autolaunch`), which no ordinal sort does.
      - The order depends on the machine's locale. Under `da-DK`, `aa` and `AA` sort
        after `z`, because Danish collates "aa" as "a-ring". So the same lockfile on the
        same commit produces a different byte sequence for a Danish developer than for an
        American one.

    That second point makes the original's order unreproducible, which is a poor property
    for a generated artifact that CI compares by hash between the build tree and the
    extracted installer. Ordinal is identical on every machine, every locale and both
    platforms - which the Linux packaging step needs anyway.

    Order carries no meaning here: the appendix is an unordered set of notices, and
    `verify_dependency_licenses` checks texts, hashes and counts, never positions.
    """
    return s
