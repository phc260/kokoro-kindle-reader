#!/usr/bin/env python3
r"""Provision the Kokoro voice model and the Cloud Reader OCR models into the host's
app-data dir: the panel's first-run download, without the panel.

Fetches the 31 files pinned by model-manifest.json - config, tokenizer, the 325 MB
onnx/model.onnx and 27 voices/*.bin, 324 MiB in all - and the 3 pinned by
ocr-manifest.json (9.8 MiB) into

    <app_data>/onnx-community/Kokoro-82M-v1.0-ONNX/<path>
    <app_data>/ocr/<path>

which is exactly where a RELEASE kokoro-host looks for them (`boot` in main.rs,
`ocr_assets` in webserve.rs). Every file is SHA-256-verified against its manifest before it
is committed.

WHY THIS EXISTS when the panel already downloads both: the panel is the only other
downloader in the tree and it does not build for Linux yet (that is the port plan's
desktop-integration step). Without this, a Linux host starts, logs "model.onnx not found"
and synthesizes nothing, and answers /ocr with `missing` - so the Linux package would
install a narrator with nothing to narrate with. The .deb therefore ships this script as
`kokoro-fetch-models`, laid out as it is here (this file in tools/, the two manifests one
level up), so the same path arithmetic finds them in both places. On Windows the panel
stays the normal route; this works there too, and --verify-only is the panel's
"Verify & repair" on a machine with no GUI.

THE MANIFESTS ARE THE PIN. This reads the same model-manifest.json and ocr-manifest.json
the panel embeds (kokoro-panel/src/download.rs) rather than carrying its own copy of 34
digests - one more copy is one more thing to keep in sync, and the failure mode is a
machine provisioning different weights from the ones a release installs. Every URL names an
immutable revision, so a retry can only ever produce the same bytes.

native-deps/fetch-ocr-models.py is a different thing: it fills native-deps/ocr/, the tree
a DEBUG build falls back to, so a `cargo run` needs no app-data provisioning. A release
build never reads that tree, which is why the OCR files are fetched here too.
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
OCR_MANIFEST = REPO / "ocr-manifest.json"
# `<app_data>/ocr`. Named here, not read from the manifest's own `dir`, for the same reason
# the panel names it: it has to be the directory webserve.rs's `OCR_DIR` reads, and a
# manifest edit must not be able to move the download somewhere the host never looks.
OCR_DIR = "ocr"


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


def planned_files(app_data):
    """Every file to provision, as (label, dest, url, size, sha256): the voice model first,
    then the OCR models - the order the panel downloads them in.

    One list rather than two loops, so "already present", `.part`-then-rename, the
    digest-then-size checks and --verify-only are written once for both manifests.
    """
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    base_url = manifest["base_url"].rstrip("/")
    model_id = manifest["model_id"]

    # `model_id` is a two-segment repo id that becomes two directories; it comes from the
    # same manifest as the paths below and gets the same guard.
    model_rel = safe_relpath(model_id)
    if model_rel is None:
        fail("model-manifest.json: unsafe model_id %r" % model_id)

    planned = []
    for f in manifest["files"]:
        safe = safe_relpath(f["path"])
        if safe is None:
            fail("model-manifest.json: unsafe path %r" % f["path"])
        planned.append((f["path"], app_data / model_rel / safe,
                        "%s/%s" % (base_url, f["path"]), f["size"], f["sha256"]))

    # Each OCR file carries its own URL: the recognizer is Git LFS and comes from a
    # different host than the detector and the dictionary (see ocr-manifest.json).
    for f in json.loads(OCR_MANIFEST.read_text(encoding="utf-8"))["files"]:
        safe = safe_relpath(f["path"])
        if safe is None:
            fail("ocr-manifest.json: unsafe path %r" % f["path"])
        planned.append(("%s/%s" % (OCR_DIR, f["path"]), app_data / OCR_DIR / safe,
                        f["url"], f["size"], f["sha256"]))
    return model_id, planned


def main():
    ap = argparse.ArgumentParser(
        description="Provision the Kokoro voice model and the Cloud Reader OCR models.")
    ap.add_argument("--dest", metavar="DIR",
                    help="app-data dir to provision into (default: the host's own)")
    ap.add_argument("--force", action="store_true",
                    help="re-fetch every file even if it already matches its pin")
    ap.add_argument("--verify-only", action="store_true",
                    help="check what is on disk against the manifests; never touch the network")
    args = ap.parse_args()

    app_data = Path(args.dest) if args.dest else app_data_dir()
    model_id, files = planned_files(app_data)
    total = sum(f[3] for f in files)
    width = len(str(len(files)))

    print("==> model %s + the Cloud Reader OCR models" % model_id)
    print("==> into  %s" % app_data)
    print("==> %d files, %s" % (len(files), human(total)))

    problems = []
    fetched = kept = 0

    for i, (rel, dest, url, size, want) in enumerate(files, 1):
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
        download(url, part)

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
            fail("%s is %d bytes but its manifest says %d - the manifest is wrong, not the "
                 "download (the SHA-256 matched)" % (rel, got, size))
        os.replace(part, dest)
        fetched += 1

    if args.verify_only:
        if problems:
            sys.stdout.flush()  # see provision_util.fail: keep stderr after the listing
            for p in problems:
                print("    %s" % p, file=sys.stderr)
            fail("==> %d of %d file(s) failed verification in %s"
                 % (len(problems), len(files), app_data))
        print("==> all %d files match their manifests" % len(files))
        return

    model_rel = safe_relpath(model_id)
    voices = sorted(p.stem for p in (app_data / model_rel / "voices").glob("*.bin"))
    print("==> voice + OCR models provisioned in %s" % app_data)
    print("    %d fetched, %d already present; %d narrators" % (fetched, kept, len(voices)))


if __name__ == "__main__":
    main()
