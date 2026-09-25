#!/usr/bin/env python3
r"""Provision the Kokoro voice model into the host's app-data dir (dev).

Fetches the 31 files pinned by model-manifest.json - config, tokenizer, the 325 MB
onnx/model.onnx and 27 voices/*.bin, 324 MiB in all - into

    <app_data>/onnx-community/Kokoro-82M-v1.0-ONNX/<path>

which is exactly where kokoro-host looks for them (`boot` in main.rs). Every file is
SHA-256-verified against the manifest before it is committed.

WHY THIS EXISTS when the panel already downloads the model: the panel is the only
downloader in the tree and it does not build for Linux yet (that is the port plan's
desktop-integration step). Without this, a freshly provisioned Ubuntu host starts, logs
"model.onnx not found" and synthesizes nothing - so the Linux milestone of recognizing a
page and speaking it is unreachable on the machine it is about. On Windows the panel stays
the normal route; this works there too, and --verify-only is the panel's "Verify & repair"
on a machine with no GUI.

THE MANIFEST IS THE PIN. This reads the same model-manifest.json the panel embeds
(kokoro-panel/src/download.rs) rather than carrying its own copy of 31 digests - one more
copy is one more thing to keep in sync, and the failure mode is a dev provisioning
different weights from the ones a release installs. `base_url` names an immutable
HuggingFace revision, so a retry can only ever produce the same bytes.

This does NOT fetch the Cloud Reader OCR models: that is native-deps/fetch-ocr-models.py,
which fills native-deps/ocr/ - the tree webserve.rs falls back to when <app_data>/ocr is
absent. They stay separate because only synthesis needs these 324 MiB.

DEV ONLY. A release install never runs this; the panel does it, with a progress bar.
"""

import argparse
import json
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
# The script's own directory is sys.path[0] when run directly; make that explicit so
# importing this module from elsewhere (a test, a wrapper) resolves the helpers too.
sys.path.insert(0, str(HERE))
from provision_util import download, fail, sha256_file  # noqa: E402 - needs the path above

MANIFEST = REPO / "model-manifest.json"


def app_data_dir():
    """Where kokoro-host looks. Mirrors `app_data_dir` in kokoro-host/src/main.rs - BOTH
    branches, because the host and this script have to name one directory or the model
    lands somewhere nothing reads.

    Windows keeps the reverse-DNS identifier under %APPDATA% (it matches prior releases, so
    an existing install's model is reused rather than re-downloaded); off Windows it is the
    plainly-named XDG data dir. One directory holds settings, the voice model and the OCR
    models there - see main.rs for why that is deliberate and where it will eventually split.

    The one divergence: the host treats a missing HOME as an empty path and carries on,
    which quietly makes its app-data dir RELATIVE to the working directory. That is a
    curiosity for a daemon and a real hazard for something about to write 324 MiB, so this
    stops and asks for --dest instead.
    """
    if os.name == "nt":
        return Path(os.environ.get("APPDATA", "")) / "com.phc260.kokoro-kindle-reader"
    xdg = os.environ.get("XDG_DATA_HOME")
    if xdg:  # empty is unset, as the host's own filter has it
        return Path(xdg) / "kokoro-kindle-reader"
    home = os.environ.get("HOME")
    if not home:
        fail("neither XDG_DATA_HOME nor HOME is set, so there is no app-data dir to "
             "provision into - pass --dest to name one explicitly.")
    return Path(home) / ".local" / "share" / "kokoro-kindle-reader"


def safe_relpath(rel):
    """`rel` as a path under the model dir, or None if it is not a plain relative path.

    Mirrors `file_path` in kokoro-panel/src/download.rs, which requires every component to
    be `Normal` - rejecting an absolute path, a drive prefix, a `..` or a bare `.`. The
    manifest is checked in and trusted today; the guard is what stops a future
    externally-sourced one from becoming a path-traversal WRITE, which is precisely what
    this script would otherwise hand it.

    Backslash is rejected outright rather than split on: it separates components on Windows
    and is an ordinary filename character on Linux, so `..\\..\\x` would survive a
    '/'-only split here and then escape once Windows resolved it.
    """
    if not rel or "\\" in rel:
        return None
    parts = rel.split("/")
    if any(p in ("", ".", "..") for p in parts):
        return None
    if any(":" in p for p in parts):  # a drive prefix, or an NTFS alternate stream
        return None
    return Path(*parts)


def human(n):
    if n >= 1 << 20:
        return "%.1f MiB" % (n / float(1 << 20))
    if n >= 1 << 10:
        return "%.1f KiB" % (n / float(1 << 10))
    return "%d bytes" % n


def main():
    ap = argparse.ArgumentParser(description="Provision the Kokoro voice model (dev).")
    ap.add_argument("--dest", metavar="DIR",
                    help="app-data dir to provision into (default: the host's own)")
    ap.add_argument("--force", action="store_true",
                    help="re-fetch every file even if it already matches its pin")
    ap.add_argument("--verify-only", action="store_true",
                    help="check what is on disk against the manifest; never touch the network")
    args = ap.parse_args()

    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    base_url = manifest["base_url"].rstrip("/")
    model_id = manifest["model_id"]
    files = manifest["files"]

    # `model_id` is a two-segment repo id that becomes two directories; it comes from the
    # same manifest as the paths below and gets the same guard.
    model_rel = safe_relpath(model_id)
    if model_rel is None:
        fail("model-manifest.json: unsafe model_id %r" % model_id)

    base = (Path(args.dest) if args.dest else app_data_dir()) / model_rel
    total = sum(f["size"] for f in files)
    width = len(str(len(files)))

    print("==> model %s" % model_id)
    print("==> into  %s" % base)
    print("==> %d files, %s" % (len(files), human(total)))

    problems = []
    fetched = kept = 0

    for i, f in enumerate(files, 1):
        rel, size, want = f["path"], f["size"], f["sha256"]
        safe = safe_relpath(rel)
        if safe is None:
            fail("model-manifest.json: unsafe path %r" % rel)
        dest = base / safe
        tag = "[%*d/%d] %-24s" % (width, i, len(files), rel)

        if args.verify_only:
            if not dest.exists():
                problems.append("%s is missing" % rel)
                print("%s MISSING" % tag)
                continue
            have = sha256_file(dest)
            if have != want:
                problems.append("%s hashes to %s, not the pinned %s" % (rel, have, want))
                print("%s CORRUPT" % tag)
                continue
            if dest.stat().st_size != size:
                problems.append("%s is %d bytes; the manifest says %d"
                                % (rel, dest.stat().st_size, size))
                print("%s WRONG SIZE" % tag)
                continue
            kept += 1
            print("%s ok" % tag)
            continue

        if dest.exists() and not args.force:
            # Hash rather than trust the length, as fetch-ocr-models.py does. The panel
            # skips on size alone because it has a progress bar and a repair button; this
            # script IS that repair on a machine with no panel, so "already provisioned"
            # here has to mean the bytes are right, not merely that there are enough of them.
            if sha256_file(dest) == want:
                kept += 1
                print("%s ok" % tag)
                continue
            print("%s does not match its pin - refetching" % tag)

        print("%s fetching %s" % (tag, human(size)))
        dest.parent.mkdir(parents=True, exist_ok=True)
        # `.part` then rename, so an interrupted fetch never leaves a short file sitting
        # where the host would load it. Same name the panel's own partial takes
        # (Rust's `with_extension`), so the two cannot litter the dir differently.
        part = dest.with_suffix(".part")
        download("%s/%s" % (base_url, rel), part)

        have = sha256_file(part)
        if have != want:
            # Delete rather than leave it: a file that hashes wrong would otherwise sit
            # there looking provisioned, and the next run would skip it on presence.
            part.unlink()
            fail("%s hashes to %s, not the pinned %s - refusing to leave it on disk"
                 % (rel, have, want))
        got = part.stat().st_size
        if got != size:
            # The hash already proved the bytes, so this is not a bad download - it is the
            # manifest's `size` disagreeing with them. Worth stopping for: the panel's
            # present-check is size-only, so it would re-download forever a file this
            # script had just called good.
            part.unlink()
            fail("%s is %d bytes but model-manifest.json says %d - the manifest is wrong, "
                 "not the download (the SHA-256 matched)" % (rel, got, size))
        os.replace(part, dest)
        fetched += 1

    if args.verify_only:
        if problems:
            sys.stdout.flush()  # see provision_util.fail: keep stderr after the listing
            for p in problems:
                print("    %s" % p, file=sys.stderr)
            fail("==> %d of %d file(s) failed verification in %s"
                 % (len(problems), len(files), base))
        print("==> all %d files match model-manifest.json" % len(files))
        return

    voices = sorted(p.stem for p in (base / "voices").glob("*.bin"))
    print("==> voice model provisioned in %s" % base)
    print("    %d fetched, %d already present; %d narrators" % (fetched, kept, len(voices)))
    print("    the Cloud Reader OCR models are separate: native-deps/fetch-ocr-models.py")


if __name__ == "__main__":
    main()
