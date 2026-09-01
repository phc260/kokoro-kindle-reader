# Provision the Cloud Reader OCR models into native-deps/ocr/.
#
#   native-deps/ocr/det.onnx      en_PP-OCRv3_det_infer      (text DETECTION, DBNet)
#   native-deps/ocr/rec.onnx      en_PP-OCRv5_mobile_rec     (text RECOGNITION, CTC)
#   native-deps/ocr/en_dict.txt   ppocrv5_en_dict            (that recognizer's alphabet)
#
# 9.80 MiB together. Both sources are Apache-2.0 ONNX conversions of PaddleOCR models; why this
# pair, and why two models rather than one engine, is in kokoro-ocr/README.md.
#
# THE DIGESTS ARE THE PIN, and they are checked here AND at run time. `kokoro-ocr` carries the
# same three constants and re-verifies them on every /status probe, so a file swapped after
# install is `corrupt`, not silently a different recognizer. If a digest fails here the file is
# deleted rather than left on disk: a half-written model that hashes wrong would otherwise sit
# there looking provisioned.
#
# Separate from fetch-deps.ps1 on purpose. That script needs Python, CMake and MSVC and takes
# minutes; this one needs network and nothing else, and the host BUILDS without it (the models
# are loaded at run time, and a host that cannot find them reports `missing`). Kept ASCII, like
# every .ps1 here - PowerShell 5.1 misreads a UTF-8 no-BOM em-dash.
#
# EVERY URL PINS A REVISION, never a branch. A branch is whatever it points at today, and these
# are weights: the digest below would start failing on some future commit and the failure would
# read as a corrupt download rather than as upstream moving.
#
# The recognizer comes from the MEDIA host, not `raw.githubusercontent.com`. It is stored in Git
# LFS, and raw hands back the 132-byte pointer file with a 200 - which is exactly the shape of
# download that a size check would wave through. The pointer's own `oid sha256` happens to be
# the pinned digest, so this is checkable: the file must BE those bytes, not describe them.
#
# -VerifyOnly checks all three against their pins and throws without touching the network - a
# standalone integrity check for a dev's native-deps\ocr. (The installer no longer stages these:
# the OCR models are NOT bundled; the panel downloads them at first run per ocr-manifest.json,
# whose pins mirror the ones here and in kokoro-ocr/src/lib.rs - keep the three in sync.)
param(
    [switch]$Force,
    [switch]$VerifyOnly
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$dir = Join-Path $PSScriptRoot 'ocr'
New-Item -ItemType Directory -Force $dir | Out-Null

# SWHL/RapidOCR (detector) and PT-Perkasa-Pilar-Utama/ppu-paddle-ocr-models (recognizer +
# dictionary), both Apache-2.0.
$hfRev = '1cfba2e90fc938db55889873735088de210cc173'
$ghRev = '3a180da5b1a3bab3371d970f4da42cb9b354a9a7'
$ghPath = "PT-Perkasa-Pilar-Utama/ppu-paddle-ocr-models/$ghRev/recognition/multi/en/v5"
# Two hosts for one repo, and the split is LFS: `media` serves the real bytes of an LFS file and
# 404s for anything else, `raw` serves ordinary files and hands back a 132-byte pointer for an
# LFS one. The model is LFS, the dictionary is not.
$ghLfs = "https://media.githubusercontent.com/media/$ghPath"
$ghRaw = "https://raw.githubusercontent.com/$ghPath"

$assets = @(
    @{
        Name = 'det.onnx'
        Url  = "https://huggingface.co/SWHL/RapidOCR/resolve/$hfRev/PP-OCRv4/en_PP-OCRv3_det_infer.onnx"
        Sha  = 'f139598bc2af4e4b6fe98dec11574e30edfdd91fc94ac1425c18ace3bd5a866b'
    },
    @{
        Name = 'rec.onnx'
        Url  = "$ghLfs/en_PP-OCRv5_mobile_rec_infer.onnx"
        Sha  = '1081b104a3c44d103511f150763d997a846994431c5775a800c802254c1124bf'
    },
    @{
        Name = 'en_dict.txt'
        Url  = "$ghRaw/ppocrv5_en_dict.txt"
        Sha  = 'c60d46e9e01d500ed6388fe8681051eac9cf6692e0d57238315be171927a0a1b'
    }
)

foreach ($a in $assets) {
    $path = Join-Path $dir $a.Name
    if ((Test-Path $path) -and -not $Force) {
        $have = (Get-FileHash $path -Algorithm SHA256).Hash
        if ($have -eq $a.Sha.ToUpper()) {
            Write-Host ("==> {0} already provisioned" -f $a.Name)
            continue
        }
        if ($VerifyOnly) {
            throw ("{0} hashes to {1}, not the pinned {2}" -f $a.Name, $have, $a.Sha)
        }
        Write-Host ("==> {0} does not match its pin - refetching" -f $a.Name)
    }
    elseif ($VerifyOnly) {
        throw ("{0} is missing from {1} - run native-deps\fetch-ocr-models.ps1" -f $a.Name, $dir)
    }

    Write-Host ("==> Fetching {0}" -f $a.Name)
    Invoke-WebRequest -Uri $a.Url -OutFile $path -UseBasicParsing

    $have = (Get-FileHash $path -Algorithm SHA256).Hash
    if ($have -ne $a.Sha.ToUpper()) {
        Remove-Item $path -Force
        throw ("{0} hashes to {1}, not the pinned {2} - refusing to leave it on disk" -f $a.Name, $have, $a.Sha)
    }
}

Write-Host '==> OCR models provisioned:'
Get-ChildItem $dir | ForEach-Object {
    Write-Host ("    {0,-14} {1,9:N0} bytes" -f $_.Name, $_.Length)
}
