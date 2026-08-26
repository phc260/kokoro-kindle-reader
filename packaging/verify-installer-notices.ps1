# Post-build check: extract the produced -setup.exe and assert the whole licence/notice
# tree is present and non-empty. GPLv3 requires the notices accompany the binaries; a
# staging bug or a dropped copy step produces an installer that LOOKS complete and is not.
# This is the same "fail loudly" contract as the OCR-model digests - the installer build is
# not "done" until this passes.
#
#   packaging\verify-installer-notices.ps1                 # newest packaging\*-setup.exe
#   packaging\verify-installer-notices.ps1 -Setup path.exe
#
# ASCII only (PS 5.1). Uses 7-Zip to unpack the NSIS installer (present on windows-latest).
param([string]$Setup)
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot

if (-not $Setup) {
    $Setup = (Get-ChildItem (Join-Path $here '*-setup.exe') |
              Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
}
if (-not $Setup -or -not (Test-Path $Setup)) { throw 'No -setup.exe found to verify.' }
Write-Host "==> Verifying $Setup"

# Locate 7-Zip.
$sevenZip = (Get-Command '7z' -ErrorAction SilentlyContinue).Source
foreach ($cand in 'C:\Program Files\7-Zip\7z.exe', 'C:\Program Files (x86)\7-Zip\7z.exe') {
    if (-not $sevenZip -and (Test-Path $cand)) { $sevenZip = $cand }
}
if (-not $sevenZip) { throw '7-Zip (7z) not found; needed to unpack the NSIS installer.' }

$dir = Join-Path $env:TEMP ("kkr-verify-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $dir | Out-Null
& $sevenZip x $Setup "-o$dir" -y *> $null
if ($LASTEXITCODE) { throw "7z failed to extract $Setup" }

# Find a file anywhere in the extracted tree (NSIS/7z layout puts app files under an
# internal folder), returning its FileInfo or $null. Match by relative-path suffix so
# 'licenses\espeak-ng\COPYING.UCD' is unambiguous but path-prefix-agnostic.
$all = Get-ChildItem $dir -Recurse -File
function Find-Shipped([string]$suffix) {
    $s = ($suffix -replace '/', '\')
    $all | Where-Object { $_.FullName -replace '/', '\' -like "*\$s" } | Select-Object -First 1
}

# Required single files (relative to the install root) - must exist AND be non-empty.
$required = @(
    'LICENSE',
    'THIRD_PARTY_NOTICES.md',
    'licenses\GPL-3.0.txt',
    'licenses\Apache-2.0.txt',
    'licenses\Unicode-3.0.txt',
    'licenses\dawn-BSD-3-Clause.txt',
    'licenses\dxcompiler-NCSA.txt',
    'licenses\espeak-ng-BSD-2-Clause.txt',
    'licenses\espeak-ng\COPYING',
    'licenses\espeak-ng\COPYING.APACHE',
    'licenses\espeak-ng\COPYING.BSD2',
    'licenses\espeak-ng\COPYING.UCD',
    'licenses\nsis\NSIS-COPYING.txt'
)

$missing = @()
$empty = @()
foreach ($r in $required) {
    $f = Find-Shipped $r
    if (-not $f) { $missing += $r }
    elseif ($f.Length -eq 0) { $empty += $r }
}

# Required directory groups - must contain at least one non-empty file.
function Test-Group([string]$suffixDir, [string]$filter, [int]$min) {
    $sd = ($suffixDir -replace '/', '\')
    $hits = $all | Where-Object {
        ($_.FullName -replace '/', '\') -like "*\$sd\*$filter" -and $_.Length -gt 0
    }
    return $hits.Count -ge $min
}

$groupErrors = @()
# ORT notice set (LICENSE/ThirdPartyNotices/Privacy, provisioned from the wheel).
if (-not (Test-Group 'licenses\onnxruntime' '' 1)) {
    $groupErrors += 'licenses\onnxruntime\ (ORT wheel notices) is missing or empty'
}
# The five per-binary generated Rust dependency reports.
if (-not (Test-Group 'licenses\dependencies' '.html' 5)) {
    $groupErrors += 'licenses\dependencies\*.html (expected 5 per-binary cargo-about reports)'
}

Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue

if ($missing.Count -or $empty.Count -or $groupErrors.Count) {
    if ($missing.Count) { Write-Host "MISSING:`n  $($missing -join "`n  ")" }
    if ($empty.Count)   { Write-Host "EMPTY:`n  $($empty -join "`n  ")" }
    if ($groupErrors.Count) { Write-Host "GROUPS:`n  $($groupErrors -join "`n  ")" }
    throw 'Installer notice-tree verification FAILED (see above).'
}

Write-Host "==> OK: all required licence/notice files present and non-empty in the installer."
