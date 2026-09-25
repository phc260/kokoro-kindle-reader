#!/usr/bin/env python3
r"""Provision the Cloud Reader OCR models into native-deps/ocr/.

  native-deps/ocr/det.onnx      en_PP-OCRv3_det_infer      (text DETECTION, DBNet)
  native-deps/ocr/rec.onnx      en_PP-OCRv5_mobile_rec     (text RECOGNITION, CTC)
  native-deps/ocr/en_dict.txt   ppocrv5_en_dict            (that recognizer's alphabet)

9.80 MiB together. Both sources are Apache-2.0 ONNX conversions of PaddleOCR models; why this
pair, and why two models rather than one engine, is in kokoro-ocr/README.md.

THE DIGESTS ARE THE PIN, and they are checked here AND at run time. `kokoro-ocr` carries the
same three constants and re-verifies them on every /status probe, so a file swapped after
install is `corrupt`, not silently a different recognizer. If a digest fails here the file is
deleted rather than left on disk: a half-written model that hashes wrong would otherwise sit
there looking provisioned.

Separate from fetch-deps.py on purpose. That recipe needs CMake and a C toolchain and takes
minutes; this one needs network and nothing else, and the host BUILDS without it (the models
are loaded at run time, and a host that cannot find them reports `missing`).

EVERY URL PINS A REVISION, never a branch. A branch is whatever it points at today, and these
are weights: the digest below would start failing on some future commit and the failure would
read as a corrupt download rather than as upstream moving.

The recognizer comes from the MEDIA host, not `raw.githubusercontent.com`. It is stored in Git
LFS, and raw hands back the 132-byte pointer file with a 200 - which is exactly the shape of
download that a size check would wave through. The pointer's own `oid sha256` happens to be
the pinned digest, so this is checkable: the file must BE those bytes, not describe them.

--verify-only checks all three against their pins and fails without touching the network - a
standalone integrity check for a dev's native-deps/ocr. (The installer does not stage these:
the OCR models are NOT bundled; the panel downloads them at first run per ocr-manifest.json,
whose pins mirror the ones here and in kokoro-ocr/src/lib.rs - keep the three in sync.)

DEV ONLY. A release install never runs this.
"""

import argparse
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
# The script's own directory is sys.path[0] when run directly; make that explicit so
# importing this module from elsewhere (a test, a wrapper) resolves the helpers too.
sys.path.insert(0, str(HERE))
from provision_util import download, fail, sha256_file  # noqa: E402 - needs the path above

# SWHL/RapidOCR (detector) and PT-Perkasa-Pilar-Utama/ppu-paddle-ocr-models (recognizer +
# dictionary), both Apache-2.0.
HF_REV = "1cfba2e90fc938db55889873735088de210cc173"
GH_REV = "3a180da5b1a3bab3371d970f4da42cb9b354a9a7"
GH_PATH = "PT-Perkasa-Pilar-Utama/ppu-paddle-ocr-models/%s/recognition/multi/en/v5" % GH_REV
# Two hosts for one repo, and the split is LFS: `media` serves the real bytes of an LFS file
# and 404s for anything else, `raw` serves ordinary files and hands back a 132-byte pointer
# for an LFS one. The model is LFS, the dictionary is not.
GH_LFS = "https://media.githubusercontent.com/media/%s" % GH_PATH
GH_RAW = "https://raw.githubusercontent.com/%s" % GH_PATH

ASSETS = [
    {
        "name": "det.onnx",
        "url": ("https://huggingface.co/SWHL/RapidOCR/resolve/%s/PP-OCRv4/"
                "en_PP-OCRv3_det_infer.onnx" % HF_REV),
        "sha256": "f139598bc2af4e4b6fe98dec11574e30edfdd91fc94ac1425c18ace3bd5a866b",
    },
    {
        "name": "rec.onnx",
        "url": "%s/en_PP-OCRv5_mobile_rec_infer.onnx" % GH_LFS,
        "sha256": "1081b104a3c44d103511f150763d997a846994431c5775a800c802254c1124bf",
    },
    {
        "name": "en_dict.txt",
        "url": "%s/ppocrv5_en_dict.txt" % GH_RAW,
        "sha256": "c60d46e9e01d500ed6388fe8681051eac9cf6692e0d57238315be171927a0a1b",
    },
]


def main():
    ap = argparse.ArgumentParser(description="Provision the Cloud Reader OCR models (dev).")
    ap.add_argument("--force", action="store_true", help="re-fetch even if already pinned")
    ap.add_argument("--verify-only", action="store_true",
                    help="check the three files against their pins; never touch the network")
    args = ap.parse_args()

    out = HERE / "ocr"
    out.mkdir(parents=True, exist_ok=True)

    for asset in ASSETS:
        path = out / asset["name"]
        if path.exists() and not args.force:
            have = sha256_file(path)
            if have == asset["sha256"]:
                print("==> %s already provisioned" % asset["name"])
                continue
            if args.verify_only:
                fail("%s hashes to %s, not the pinned %s"
                     % (asset["name"], have, asset["sha256"]))
            print("==> %s does not match its pin - refetching" % asset["name"])
        elif args.verify_only:
            fail("%s is missing from %s - run native-deps/fetch-ocr-models.py"
                 % (asset["name"], out))

        print("==> Fetching %s" % asset["name"])
        download(asset["url"], path)

        have = sha256_file(path)
        if have != asset["sha256"]:
            # Delete rather than leave it: a file that hashes wrong would otherwise sit
            # there looking provisioned, and the next run would skip it on presence.
            path.unlink(missing_ok=True)
            fail("%s hashes to %s, not the pinned %s - refusing to leave it on disk"
                 % (asset["name"], have, asset["sha256"]))

    print("==> OCR models provisioned:")
    for f in sorted(out.iterdir()):
        if f.is_file():
            print("    %-14s %9d bytes" % (f.name, f.stat().st_size))


if __name__ == "__main__":
    main()
